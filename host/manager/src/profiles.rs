//! The Profiles page: every profile, each a separate Android setup with its
//! own apps, data and settings, to create, delete, start and stop. Every core
//! call runs off the GTK thread.
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use innkeeper_core::profiles::{self, Mode, Profile, Settings, Size};

use crate::{
    errors::Op,
    window::{WeakWindow, Window},
    worker,
};

struct Page {
    window: WeakWindow,
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
    /// The profile this window shows the apps of.
    current: RefCell<String>,
    /// A profile being started, stopped or deleted from here.
    busy: RefCell<Option<String>>,
    /// Profiles whose settings show.
    expanded: RefCell<HashSet<String>>,
    /// Refreshes the list while the page shows.
    timer: RefCell<Option<glib::SourceId>>,
}

pub fn page(window: &Window) -> adw::NavigationPage {
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .tooltip_text("New Profile")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    let group = adw::PreferencesGroup::builder()
        .title("Profiles")
        .description(
            "Each profile is a separate Android with its own apps, data and settings. \
             Profiles can run side by side.",
        )
        .header_suffix(&add)
        .build();
    let content = adw::PreferencesPage::new();
    content.add(&group);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let page = adw::NavigationPage::builder()
        .title("Profiles")
        .child(&toolbar)
        .build();
    let state = Rc::new(Page {
        window: window.downgrade(),
        group,
        rows: RefCell::default(),
        current: RefCell::default(),
        busy: RefCell::default(),
        timer: RefCell::default(),
        expanded: RefCell::default(),
    });
    let s = Rc::downgrade(&state);
    add.connect_clicked(move |_| {
        if let Some(s) = s.upgrade() {
            s.create();
        }
    });
    let s = state.clone();
    page.connect_showing(move |_| {
        s.load();
        // Profiles start and stop by themselves too (idle, the CLI).
        let weak = Rc::downgrade(&s);
        let timer = glib::timeout_add_local(Duration::from_secs(2), move || {
            if let Some(s) = weak.upgrade() {
                s.load();
            }
            glib::ControlFlow::Continue
        });
        if let Some(old) = s.timer.replace(Some(timer)) {
            old.remove();
        }
    });
    page.connect_hidden(move |_| {
        if let Some(t) = state.timer.take() {
            t.remove();
        }
    });
    page
}

impl Page {
    fn error(&self, e: &anyhow::Error) {
        if let Some(w) = self.window.upgrade() {
            w.show_error(Op::Profiles, None, e);
        }
    }

    fn load(self: &Rc<Self>) {
        let s = self.clone();
        glib::spawn_future_local(async move {
            match worker::run_quiet(|| {
                Ok::<_, anyhow::Error>((
                    profiles::list()?,
                    innkeeper_core::paths::profile().unwrap_or_default(),
                ))
            })
            .await
            {
                Ok((list, current)) => {
                    s.current.replace(current);
                    s.show(&list);
                }
                Err(e) => s.error(&e),
            }
        });
    }

    fn show(self: &Rc<Self>, list: &[Profile]) {
        for r in self.rows.take() {
            self.group.remove(&r);
        }
        let busy = self.busy.borrow().clone();
        let mut rows = vec![];
        for p in list {
            let row = self.row(p, busy.as_deref());
            self.group.add(&row);
            rows.push(row.upcast());
        }
        self.rows.replace(rows);
    }

    fn row(self: &Rc<Self>, p: &Profile, busy: Option<&str>) -> adw::ExpanderRow {
        let current = p.name == *self.current.borrow();
        let mut subtitle = if busy == Some(&p.name) {
            "Working…".to_string()
        } else if p.running {
            "Running".into()
        } else {
            "Stopped".into()
        };
        if current {
            subtitle += " · Apps shown here";
        }
        let dot = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["status-dot"])
            .build();
        if p.running {
            dot.add_css_class("running");
        }
        let row = adw::ExpanderRow::builder()
            .title(&p.name)
            .subtitle(subtitle)
            .expanded(self.expanded.borrow().contains(&p.name))
            .build();
        let (s, name) = (Rc::downgrade(self), p.name.clone());
        row.connect_expanded_notify(move |r| {
            if let Some(s) = s.upgrade() {
                let mut e = s.expanded.borrow_mut();
                if r.is_expanded() {
                    e.insert(name.clone());
                } else {
                    e.remove(&name);
                }
            }
        });
        row.add_prefix(&dot);
        let idle = busy.is_none();
        let toggle = gtk::Button::builder()
            .label(if p.running { "Stop" } else { "Start" })
            .valign(gtk::Align::Center)
            .sensitive(idle)
            .build();
        let (s, name, running) = (Rc::downgrade(self), p.name.clone(), p.running);
        toggle.connect_clicked(move |_| {
            if let Some(s) = s.upgrade() {
                s.toggle(&name, running);
            }
        });
        row.add_suffix(&toggle);
        let delete = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Delete Profile")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .sensitive(idle && !p.running)
            .build();
        let (s, name) = (Rc::downgrade(self), p.name.clone());
        delete.connect_clicked(move |_| {
            if let Some(s) = s.upgrade() {
                s.confirm_delete(&name);
            }
        });
        row.add_suffix(&delete);

        if !current {
            let show = adw::ActionRow::builder()
                .title("Show Apps")
                .subtitle("Show this profile’s apps in the window, and use it when none is named")
                .activatable(true)
                .build();
            show.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let (s, name) = (Rc::downgrade(self), p.name.clone());
            show.connect_activated(move |_| {
                if let Some(w) = s.upgrade().and_then(|s| s.window.upgrade()) {
                    match w.switch_profile(&name) {
                        Ok(()) => {
                            w.nav.pop_to_tag("main");
                        }
                        Err(e) => w.show_error(Op::Profiles, None, &e),
                    }
                }
            });
            row.add_row(&show);
        }
        let rename = adw::EntryRow::builder()
            .title(if p.running {
                "Name · Stop it to rename"
            } else {
                "Name"
            })
            .text(&p.name)
            .show_apply_button(true)
            .sensitive(idle && !p.running)
            .build();
        rename.connect_changed(|e| {
            if e.text().is_empty() || profiles::check_name(&e.text()).is_ok() {
                e.remove_css_class("error");
            } else {
                e.add_css_class("error");
            }
        });
        let (s, name) = (Rc::downgrade(self), p.name.clone());
        rename.connect_apply(move |e| {
            if let Some(s) = s.upgrade() {
                s.rename(&name, &e.text());
            }
        });
        row.add_row(&rename);
        row.add_row(&self.mode_row(p, idle));
        row.add_row(&self.adb_row(p, idle));
        row
    }

    /// Windowed or phone, with the phone's screen; for the next start.
    fn mode_row(self: &Rc<Self>, p: &Profile, idle: bool) -> adw::ComboRow {
        let choices = [
            (
                "Windowed",
                "Each app in a window of its own",
                Mode::Windowed,
                None,
            ),
            (
                "Small Phone",
                "One device window, 720 × 1280",
                Mode::Phone,
                Some(Size::SmallPhone),
            ),
            (
                "Medium Phone",
                "One device window, 1080 × 2400",
                Mode::Phone,
                Some(Size::MediumPhone),
            ),
            (
                "Medium Tablet",
                "One device window, 2560 × 1600",
                Mode::Phone,
                Some(Size::MediumTablet),
            ),
        ];
        let model = gtk::StringList::new(&choices.map(|c| c.0));
        let current = choices
            .iter()
            .position(|c| {
                c.2 == p.settings.mode && (c.2 == Mode::Windowed || c.3 == Some(p.settings.size))
            })
            .unwrap_or(0);
        let row = adw::ComboRow::builder()
            .title("Display")
            .subtitle(if p.running {
                format!("{} · Applies the next time it starts", choices[current].1)
            } else {
                choices[current].1.to_string()
            })
            .model(&model)
            .selected(current as u32)
            .sensitive(idle)
            .build();
        let (s, name) = (Rc::downgrade(self), p.name.clone());
        row.connect_selected_notify(move |r| {
            let Some(s) = s.upgrade() else {
                return;
            };
            let Some(&(_, _, mode, size)) = choices.get(r.selected() as usize) else {
                return;
            };
            let n = name.clone();
            s.work(
                &name,
                move || profiles::set_mode(&n, Some(mode), size).map(drop),
                None,
            );
        });
        row
    }

    fn adb_row(self: &Rc<Self>, p: &Profile, idle: bool) -> adw::SwitchRow {
        let on = p.settings.debug_bridge;
        let row = adw::SwitchRow::builder()
            .title("adb")
            .subtitle(if on {
                format!(
                    "On · <tt>{}</tt> in adb and Android Studio",
                    innkeeper_core::adb::serial(&p.settings)
                )
            } else {
                "For Android Studio, Gradle and adb on this computer".into()
            })
            .use_markup(true)
            .active(on)
            .sensitive(idle)
            .build();
        let (s, name) = (Rc::downgrade(self), p.name.clone());
        row.connect_active_notify(move |r| {
            let Some(s) = s.upgrade() else {
                return;
            };
            let (n, on) = (name.clone(), r.is_active());
            s.work(
                &name,
                move || {
                    innkeeper_core::adb::set(&n, on, &innkeeper_core::ops::quiet)?;
                    match innkeeper_core::adb::missing_key().filter(|_| on) {
                        Some(why) => Err(anyhow::anyhow!("adb is on, but {why}")),
                        None => Ok(()),
                    }
                },
                None,
            );
        });
        row
    }

    fn rename(self: &Rc<Self>, from: &str, to: &str) {
        if from == to {
            return;
        }
        if let Err(e) = profiles::check_name(to) {
            return self.error(&e);
        }
        let current = *self.current.borrow() == from;
        let (f, t) = (from.to_string(), to.to_string());
        let (s, to) = (self.clone(), to.to_string());
        self.work_then(
            from,
            move || profiles::rename(&f, &t),
            move || {
                if current && let Some(w) = s.window.upgrade() {
                    let _ = w.switch_profile(&to);
                }
            },
        );
    }

    /// Runs `f` for profile `name` off the GTK thread, then shows the list.
    fn work(
        self: &Rc<Self>,
        name: &str,
        f: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        done: Option<String>,
    ) {
        let s = self.clone();
        self.work_then(name, f, move || {
            if let (Some(text), Some(w)) = (done, s.window.upgrade()) {
                _ = w.say(&text);
            }
        });
    }

    /// [`Self::work`], calling `then` on the GTK thread if `f` succeeds.
    fn work_then(
        self: &Rc<Self>,
        name: &str,
        f: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
        then: impl FnOnce() + 'static,
    ) {
        if self.busy.borrow().is_some() {
            return;
        }
        self.busy.replace(Some(name.into()));
        self.load();
        let s = self.clone();
        glib::spawn_future_local(async move {
            let r = worker::run_quiet(f).await;
            s.busy.replace(None);
            match r {
                Ok(()) => then(),
                Err(e) => s.error(&e),
            }
            s.load();
        });
    }

    fn toggle(self: &Rc<Self>, name: &str, running: bool) {
        let n = name.to_string();
        if running {
            self.work(name, move || profiles::stop(&n), None);
        } else {
            self.work(name, move || profiles::start(&n, &|_| {}), None);
        }
    }

    fn create(self: &Rc<Self>) {
        let d = adw::AlertDialog::new(
            Some("New Profile"),
            Some("A separate Android with its own apps, data and settings."),
        );
        let entry = adw::EntryRow::builder()
            .title("Name")
            .activates_default(true)
            .build();
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        list.append(&entry);
        d.set_extra_child(Some(&list));
        d.add_responses(&[("cancel", "_Cancel"), ("create", "C_reate")]);
        d.set_response_appearance("create", adw::ResponseAppearance::Suggested);
        d.set_response_enabled("create", false);
        d.set_default_response(Some("create"));
        d.set_close_response("cancel");
        let d2 = d.clone();
        entry.connect_changed(move |e| {
            let ok = profiles::check_name(&e.text()).is_ok();
            d2.set_response_enabled("create", ok);
            if ok || e.text().is_empty() {
                e.remove_css_class("error");
            } else {
                e.add_css_class("error");
            }
        });
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let s = self.clone();
        glib::spawn_future_local(async move {
            if d.choose_future(Some(&w.win)).await != "create" {
                return;
            }
            let name = entry.text().to_string();
            let n = name.clone();
            s.work(
                &name,
                move || profiles::create(&n, &Settings::default()),
                Some(format!("Profile {name} created")),
            );
        });
    }

    fn confirm_delete(self: &Rc<Self>, name: &str) {
        let d = adw::AlertDialog::new(
            Some(&format!("Delete Profile {name}?")),
            Some(
                "Its apps, their data and its settings will be deleted, and its apps removed from the app menu. This can’t be undone.",
            ),
        );
        d.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete")]);
        d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let Some(w) = self.window.upgrade() else {
            return;
        };
        let (s, name) = (self.clone(), name.to_string());
        glib::spawn_future_local(async move {
            if d.choose_future(Some(&w.win)).await != "delete" {
                return;
            }
            let n = name.clone();
            let current = *s.current.borrow() == name;
            let s2 = s.clone();
            let busy = name.clone();
            s.work_then(
                &busy,
                move || profiles::delete(&n),
                move || {
                    let Some(w) = s2.window.upgrade() else {
                        return;
                    };
                    _ = w.say(&format!("Profile {name} deleted"));
                    // The window shows another profile's apps, or a new one's.
                    if current {
                        glib::spawn_future_local(async move {
                            match worker::run_quiet(|| profiles::fallback(true)).await {
                                Ok(next) => {
                                    if let Err(e) = w.switch_profile(&next) {
                                        w.show_error(Op::Profiles, None, &e);
                                    }
                                }
                                Err(e) => w.show_error(Op::Profiles, None, &e),
                            }
                        });
                    }
                },
            );
        });
    }
}
