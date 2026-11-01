//! Installed app details. Blocking session calls always run on the worker.
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use innkeeper_core::app_settings::{self, AppInfo, Command};
use innkeeper_core::xdg::DesktopApp;

use crate::{
    errors::Op,
    permissions,
    window::{WeakWindow, Window},
    worker,
};

struct Page {
    window: WeakWindow,
    app: DesktopApp,
    content: gtk::Box,
    busy: Cell<bool>,
}

pub fn page(window: &Window, app: &DesktopApp) -> adw::NavigationPage {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let page = adw::NavigationPage::builder()
        .title(&app.name)
        .child(&toolbar)
        .build();
    let state = Rc::new(Page {
        window: window.downgrade(),
        app: app.clone(),
        content,
        busy: Cell::new(false),
    });
    // The navigation page owns the state. Its rows use weak references, so
    // leaving the page releases its controls and doesn't retain the main window.
    page.connect_showing(move |_| state.load(Command::new(&state.app.package, "info")));
    page
}

impl Page {
    fn load(self: &Rc<Self>, command: Command) {
        if self.busy.replace(true) {
            return;
        }
        self.content.set_sensitive(false);
        if self.content.first_child().is_none() {
            self.content.append(
                &adw::StatusPage::builder()
                    .title("Loading…")
                    .child(&adw::Spinner::new())
                    .vexpand(true)
                    .build(),
            );
        }
        let state = self.clone();
        let Some(profile) = self.window.upgrade().map(|w| w.profile()) else {
            return;
        };
        glib::spawn_future_local(async move {
            let result = worker::run(
                move |report| app_settings::request(&profile, command, report),
                |_| {},
            )
            .await;
            state.busy.set(false);
            state.content.set_sensitive(true);
            match result {
                Ok(info) => state.show(info),
                Err(e) => {
                    if let Some(window) = state.window.upgrade() {
                        window.show_error(Op::AppSettings, Some(&state.app.name), &e);
                    }
                    // Re-read after an uncertain or rejected mutation. Preserve the previous
                    // controls until the user retries, but don't leave a toggle claiming success.
                    state.clear();
                    let retry = gtk::Button::with_label("Try Again");
                    let s = Rc::downgrade(&state);
                    retry.connect_clicked(move |_| {
                        if let Some(s) = s.upgrade() {
                            s.load(Command::new(&s.app.package, "info"));
                        }
                    });
                    state.content.append(
                        &adw::StatusPage::builder()
                            .title("App Settings Unavailable")
                            .child(&retry)
                            .vexpand(true)
                            .build(),
                    );
                }
            }
        });
    }

    fn clear(&self) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
    }

    fn show(self: &Rc<Self>, info: AppInfo) {
        self.clear();
        let prefs = adw::PreferencesPage::new();
        prefs.set_vexpand(true);
        let app = adw::PreferencesGroup::new();
        let identity = adw::ActionRow::builder()
            .title(&self.app.name)
            .subtitle(&self.app.package)
            .use_markup(false)
            .build();
        identity.add_prefix(&crate::window::app_icon(self.app.icon.as_ref(), 48));
        let open = gtk::Button::builder()
            .label("Open")
            .valign(gtk::Align::Center)
            .build();
        let s = Rc::downgrade(self);
        open.connect_clicked(move |_| {
            if let Some(s) = s.upgrade()
                && let Some(window) = s.window.upgrade()
            {
                window.launch(&s.app.package, &s.app.name);
            }
        });
        identity.add_suffix(&open);
        app.add(&identity);
        prefs.add(&app);

        let storage = adw::PreferencesGroup::builder().title("Storage").build();
        for (title, bytes) in [
            ("App", info.app_bytes),
            ("Data", info.data_bytes.saturating_sub(info.cache_bytes)),
            ("Cache", info.cache_bytes),
        ] {
            let row = adw::ActionRow::builder().title(title).build();
            row.add_suffix(&gtk::Label::new(Some(&glib::format_size(bytes))));
            storage.add(&row);
        }
        self.action(&storage, "Clear Cache", "clear-cache", false);
        self.action(&storage, "Clear Data…", "clear-data", true);
        prefs.add(&storage);

        let settings = adw::PreferencesGroup::builder().title("Behaviour").build();
        self.switch(
            &settings,
            "Escape as Back",
            "",
            "escape-back",
            "",
            info.escape_back,
            true,
        );
        self.switch(
            &settings,
            "Run in Background",
            "",
            "background",
            "",
            info.background,
            true,
        );
        prefs.add(&settings);

        let permissions = adw::PreferencesGroup::builder()
            .title("Permissions")
            .build();
        if info.permissions.is_empty() {
            permissions.set_description(Some("None"));
        }
        let titles = permissions::titles(info.permissions.iter().map(|p| p.name.as_str()));
        for (p, (title, subtitle)) in info.permissions.iter().zip(titles) {
            self.switch(
                &permissions,
                &title,
                &subtitle,
                "permission",
                &p.name,
                p.granted,
                p.editable,
            );
        }
        prefs.add(&permissions);

        let actions = adw::PreferencesGroup::new();
        self.action(&actions, "Force Stop", "force-stop", false);
        let row = adw::ActionRow::builder()
            .title("Uninstall…")
            .activatable(true)
            .build();
        row.add_css_class("error");
        let s = Rc::downgrade(self);
        row.connect_activated(move |_| {
            if let Some(s) = s.upgrade()
                && let Some(window) = s.window.upgrade()
            {
                window.confirm_uninstall(&s.app);
            }
        });
        actions.add(&row);
        prefs.add(&actions);
        self.content.append(&prefs);
    }

    #[allow(clippy::too_many_arguments)]
    fn switch(
        self: &Rc<Self>,
        group: &adw::PreferencesGroup,
        title: &str,
        subtitle: &str,
        action: &str,
        permission: &str,
        active: bool,
        editable: bool,
    ) {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .use_markup(false)
            .active(active)
            .sensitive(editable)
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
        group.add(&row);
    }

    fn action(
        self: &Rc<Self>,
        group: &adw::PreferencesGroup,
        title: &str,
        action: &str,
        confirm: bool,
    ) {
        let row = adw::ActionRow::builder()
            .title(title)
            .activatable(true)
            .build();
        if confirm {
            row.add_css_class("error");
        }
        let (s, action) = (Rc::downgrade(self), action.to_string());
        row.connect_activated(move |_| {
            let Some(s) = s.upgrade() else {
                return;
            };
            let action = action.clone();
            glib::spawn_future_local(async move {
                if confirm {
                    let d = adw::AlertDialog::new(
                        Some(&format!("Clear {}’s Data?", s.app.name)),
                        Some("Its settings, accounts and files will be permanently deleted."),
                    );
                    d.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear Data")]);
                    d.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
                    d.set_default_response(Some("cancel"));
                    d.set_close_response("cancel");
                    let Some(window) = s.window.upgrade() else {
                        return;
                    };
                    if d.choose_future(Some(&window.win)).await != "clear" {
                        return;
                    }
                }
                s.load(Command::new(&s.app.package, &action));
            });
        });
        group.add(&row);
    }
}
