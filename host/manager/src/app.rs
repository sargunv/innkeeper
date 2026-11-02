//! An installed app's page. Its top opens and force stops the app. Below are
//! the app's storage, permissions and behaviour, which Android reports, and
//! Uninstall. Blocking session calls run on the worker thread.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use innkeeper_core::app_settings::{self, AppInfo, Command};
use innkeeper_core::ops::{self, Event};
use innkeeper_core::xdg::DesktopApp;
use innkeeper_profile::profile::System;

use crate::{
    errors::Op,
    hero::Hero,
    install, permissions,
    window::{WeakWindow, Window},
    worker,
};

struct Page {
    window: WeakWindow,
    app: DesktopApp,
    /// Weak, because the page's `showing` handler owns this state.
    nav: glib::WeakRef<adw::NavigationPage>,
    prefs: adw::PreferencesPage,
    hero: Hero,
    /// The groups that show Android's answer, or its progress or error.
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    uninstall: adw::PreferencesGroup,
    busy: Cell<bool>,
    /// Force Stop, which waits while Android answers another command.
    force_stop: RefCell<Option<gtk::Button>>,
}

pub fn page(window: &Window, app: &DesktopApp) -> adw::NavigationPage {
    let prefs = adw::PreferencesPage::new();
    let toolbar = adw::ToolbarView::new();
    // The page shows the app's name below its icon.
    toolbar.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
    toolbar.set_content(Some(&prefs));
    let nav = adw::NavigationPage::builder()
        .title(&app.name)
        .child(&toolbar)
        .build();
    let hero = Hero::new(
        &crate::window::app_icon(app.icon.as_ref(), 128),
        &app.name,
        &install::subtitle(&app.package, app.version.as_deref().unwrap_or_default()),
    );
    prefs.add(&hero.widget);
    let uninstall = adw::PreferencesGroup::new();
    let row = adw::ButtonRow::builder()
        .title("_Uninstall…")
        .use_underline(true)
        .build();
    row.add_css_class("destructive-action");
    uninstall.add(&row);
    let android = window.system() == System::Android;
    let state = Rc::new(Page {
        window: window.downgrade(),
        app: app.clone(),
        nav: nav.downgrade(),
        prefs,
        hero,
        groups: RefCell::default(),
        uninstall,
        busy: Cell::new(false),
        force_stop: RefCell::default(),
    });

    let s = Rc::downgrade(&state);
    state.hero.button("_Open", true).connect_clicked(move |_| {
        if let Some(s) = s.upgrade() {
            s.open();
        }
    });
    if android {
        let s = Rc::downgrade(&state);
        let force_stop = state.hero.button("_Force Stop", false);
        force_stop.connect_clicked(move |_| {
            if let Some(s) = s.upgrade() {
                s.load(Command::new(&s.app.package, "force-stop"));
            }
        });
        state.force_stop.replace(Some(force_stop));
    }
    let s = Rc::downgrade(&state);
    row.connect_activated(move |_| {
        if let Some(s) = s.upgrade() {
            s.confirm_uninstall();
        }
    });
    state.set_groups(vec![]);
    // The navigation page owns the state. Its rows use weak references, so
    // leaving the page releases its controls and doesn't retain the main window.
    // OpenHarmony doesn't report app settings.
    nav.connect_showing(move |_| {
        if android {
            state.load(Command::new(&state.app.package, "info"));
        }
    });
    nav
}

impl Page {
    fn set_force_stop_sensitive(&self, on: bool) {
        if let Some(b) = self.force_stop.borrow().as_ref() {
            b.set_sensitive(on);
        }
    }

    /// Shows `groups` between the top and Uninstall.
    fn set_groups(&self, groups: Vec<adw::PreferencesGroup>) {
        for g in self.groups.take() {
            self.prefs.remove(&g);
        }
        if self.uninstall.parent().is_some() {
            self.prefs.remove(&self.uninstall);
        }
        for g in &groups {
            self.prefs.add(g);
        }
        self.prefs.add(&self.uninstall);
        self.groups.replace(groups);
    }

    fn open(self: &Rc<Self>) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let s = self.clone();
        glib::spawn_future_local(async move {
            let h = s.hero.clone();
            s.hero.busy(Some("Opening…"));
            w.launch(&s.app.package, &s.app.name, move |m| h.busy(Some(m)))
                .await;
            s.hero.busy(None);
        });
    }

    /// Sends `command` to Android and shows the app's settings from its answer.
    fn load(self: &Rc<Self>, command: Command) {
        if self.busy.replace(true) {
            return;
        }
        self.set_force_stop_sensitive(false);
        let first = self.groups.borrow().is_empty();
        let progress = gtk::Label::new(None);
        if first {
            self.set_groups(vec![loading(&progress)]);
        }
        for g in self.groups.borrow().iter() {
            g.set_sensitive(false);
        }
        let state = self.clone();
        let Some(profile) = self.window.upgrade().map(|w| w.profile()) else {
            return;
        };
        glib::spawn_future_local(async move {
            let result = worker::run(
                move |report| app_settings::request(&profile, command, report),
                move |e| {
                    if let Event::Status(m) = e {
                        progress.set_text(&install::status_text(&m));
                    }
                },
            )
            .await;
            state.busy.set(false);
            state.set_force_stop_sensitive(true);
            match result {
                Ok(info) => state.show(info),
                Err(e) => {
                    if let Some(window) = state.window.upgrade() {
                        window.show_error(Op::AppSettings, Some(&state.app.name), &e);
                    }
                    // A failed command may have applied or not, so the old
                    // controls could show the wrong state. Replace them with a
                    // button that reads the settings again.
                    let retry = adw::ButtonRow::builder()
                        .title("_Try Again")
                        .use_underline(true)
                        .build();
                    let s = Rc::downgrade(&state);
                    retry.connect_activated(move |_| {
                        if let Some(s) = s.upgrade() {
                            s.load(Command::new(&s.app.package, "info"));
                        }
                    });
                    let group = adw::PreferencesGroup::new();
                    group.add(&retry);
                    state.set_groups(vec![group]);
                }
            }
        });
    }

    fn show(self: &Rc<Self>, info: AppInfo) {
        let storage = adw::PreferencesGroup::builder().title("Storage").build();
        storage.add(&size_row("App", info.app_bytes));
        let data = size_row("Data", info.data_bytes.saturating_sub(info.cache_bytes));
        data.add_suffix(&self.clear_button("clear-data", true));
        storage.add(&data);
        let cache = size_row("Cache", info.cache_bytes);
        cache.add_suffix(&self.clear_button("clear-cache", info.cache_bytes > 0));
        storage.add(&cache);

        let permissions = adw::PreferencesGroup::builder()
            .title("Permissions")
            .build();
        if info.permissions.is_empty() {
            permissions.set_description(Some("None"));
        }
        let titles = permissions::titles(info.permissions.iter().map(|p| p.name.as_str()));
        for (p, (title, subtitle)) in info.permissions.iter().zip(titles) {
            let row = self.switch(&title, "permission", &p.name, p.granted);
            row.set_subtitle(&subtitle);
            row.set_sensitive(p.editable);
            permissions.add(&row);
        }

        let behaviour = adw::PreferencesGroup::builder().title("Behaviour").build();
        behaviour.add(&self.switch("Escape as Back", "escape-back", "", info.escape_back));
        behaviour.add(&self.switch("Run in Background", "background", "", info.background));
        behaviour.add(&self.defaults(&info.defaults));

        self.set_groups(vec![storage, permissions, behaviour]);
    }

    /// A Clear button that sends `action`. Clearing data asks first. An empty
    /// cache has nothing to clear.
    fn clear_button(self: &Rc<Self>, action: &str, enabled: bool) -> gtk::Button {
        let button = gtk::Button::builder()
            .label("Clear")
            .valign(gtk::Align::Center)
            .sensitive(enabled)
            .build();
        let data = action == "clear-data";
        let (s, action) = (Rc::downgrade(self), action.to_string());
        button.connect_clicked(move |_| {
            let Some(s) = s.upgrade() else {
                return;
            };
            let action = action.clone();
            glib::spawn_future_local(async move {
                if data && !s.confirm_clear_data().await {
                    return;
                }
                s.load(Command::new(&s.app.package, &action));
            });
        });
        button
    }

    async fn confirm_clear_data(&self) -> bool {
        let Some(window) = self.window.upgrade() else {
            return false;
        };
        let d = adw::AlertDialog::new(Some(&format!("Clear {}’s Data?", self.app.name)), None);
        d.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear Data")]);
        d.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        d.choose_future(Some(&window.win)).await == "clear"
    }

    /// A row with the types that the app opens by default and a button that
    /// clears them.
    fn defaults(self: &Rc<Self>, defaults: &[String]) -> adw::ActionRow {
        let row = adw::ActionRow::builder().title("Open by Default").build();
        let text = defaults.join(", ");
        let types = gtk::Label::builder()
            .label(if defaults.is_empty() { "None" } else { &text })
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .max_width_chars(24)
            .css_classes(["dim-label"])
            .build();
        if !defaults.is_empty() {
            types.set_tooltip_text(Some(&text));
        }
        row.add_suffix(&types);
        let clear = gtk::Button::builder()
            .label("Clear")
            .valign(gtk::Align::Center)
            .sensitive(!defaults.is_empty())
            .build();
        let s = Rc::downgrade(self);
        clear.connect_clicked(move |_| {
            if let Some(s) = s.upgrade() {
                s.load(Command::new(&s.app.package, "clear-defaults"));
            }
        });
        row.add_suffix(&clear);
        row
    }

    /// A switch that sends `action`, for `permission` if it is one.
    fn switch(
        self: &Rc<Self>,
        title: &str,
        action: &str,
        permission: &str,
        active: bool,
    ) -> adw::SwitchRow {
        let row = adw::SwitchRow::builder()
            .title(title)
            .use_markup(false)
            .active(active)
            .build();
        let (s, action, permission) = (
            Rc::downgrade(self),
            action.to_string(),
            permission.to_string(),
        );
        row.connect_active_notify(move |row| {
            let Some(s) = s.upgrade() else {
                return;
            };
            let mut command = Command::new(&s.app.package, &action);
            command.permission = permission.clone();
            command.enabled = row.is_active();
            s.load(command);
        });
        row
    }

    fn confirm_uninstall(self: &Rc<Self>) {
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let d = adw::AlertDialog::new(Some(&format!("Uninstall {}?", self.app.name)), None);
        let keep = adw::SwitchRow::builder().title("Keep App Data").build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        list.append(&keep);
        d.set_extra_child(Some(&list));
        d.add_responses(&[("cancel", "_Cancel"), ("uninstall", "_Uninstall")]);
        d.set_response_appearance("uninstall", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let s = self.clone();
        glib::spawn_future_local(async move {
            if d.choose_future(Some(&w.win)).await != "uninstall" {
                return;
            }
            let keep_data = keep.is_active();
            s.hero.busy(Some("Uninstalling…"));
            s.prefs.set_sensitive(false);
            let nav = s.nav.upgrade();
            if let Some(n) = &nav {
                n.set_can_pop(false);
            }
            let (pkg, profile) = (s.app.package.clone(), w.profile());
            let h = s.hero.clone();
            let r = worker::run(
                move |rep| ops::uninstall(&profile, &pkg, keep_data, rep),
                move |e| {
                    if let Event::Status(text) = e {
                        h.busy(Some(&install::status_text(&text)));
                    }
                },
            )
            .await;
            if let Some(n) = &nav {
                n.set_can_pop(true);
            }
            s.prefs.set_sensitive(true);
            s.hero.busy(None);
            match r {
                Ok(_) => {
                    w.nav.pop_to_tag("main");
                }
                Err(e) => w.show_error(Op::Uninstall, Some(&s.app.name), &e),
            }
            w.refresh();
        });
    }
}

fn size_row(title: &str, bytes: u64) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).build();
    row.add_suffix(
        &gtk::Label::builder()
            .label(glib::format_size(bytes))
            .css_classes(["dim-label", "numeric"])
            .build(),
    );
    row
}

/// A group with a spinner and `progress`, while Android answers.
fn loading(progress: &gtk::Label) -> adw::PreferencesGroup {
    let b = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::Center)
        .margin_top(24)
        .build();
    b.append(&adw::Spinner::new());
    progress.add_css_class("dim-label");
    b.append(progress);
    let group = adw::PreferencesGroup::new();
    group.add(&b);
    group
}
