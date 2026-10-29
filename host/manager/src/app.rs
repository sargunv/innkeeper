//! Installed app details. Blocking session calls always run on the worker.
use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use linuxapk_core::app_settings::{self, AppInfo, Command};
use linuxapk_core::xdg::DesktopApp;

use crate::{errors::Op, permissions, window::Window, worker};

struct Page {
    window: Window,
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
        window: window.clone(),
        app: app.clone(),
        content,
        busy: Cell::new(false),
    });
    state.load(Command::new(&app.package, "info"));
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
                    .title("Loading App Settings…")
                    .child(&adw::Spinner::new())
                    .vexpand(true)
                    .build(),
            );
        }
        let state = self.clone();
        glib::spawn_future_local(async move {
            let result =
                worker::run(move |report| app_settings::request(command, report), |_| {}).await;
            state.busy.set(false);
            state.content.set_sensitive(true);
            match result {
                Ok(info) => state.show(info),
                Err(e) => {
                    state
                        .window
                        .show_error(Op::AppSettings, Some(&state.app.name), &e);
                    // Re-read after an uncertain or rejected mutation. Preserve the previous
                    // controls until the user retries, but don't leave a toggle claiming success.
                    state.clear();
                    let retry = gtk::Button::with_label("Try Again");
                    let s = state.clone();
                    retry.connect_clicked(move |_| s.load(Command::new(&s.app.package, "info")));
                    state.content.append(
                        &adw::StatusPage::builder()
                            .title("App Settings Unavailable")
                            .description("Refresh to read the current settings from Android.")
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
        let s = self.clone();
        open.connect_clicked(move |_| s.window.launch(&s.app.package, &s.app.name));
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
        self.action(
            &storage,
            "Clear Cache",
            "Remove temporary files",
            "clear-cache",
            false,
        );
        self.action(
            &storage,
            "Clear Data…",
            "Delete this app’s settings, accounts and files",
            "clear-data",
            true,
        );
        prefs.add(&storage);

        let settings = adw::PreferencesGroup::builder().title("Behaviour").build();
        self.switch(
            &settings,
            "Escape as Back",
            "Send Android’s Back button when you press Escape",
            "escape-back",
            "",
            info.escape_back,
            true,
        );
        self.switch(
            &settings,
            "Run in Background",
            "Keep Android running when this app has no window",
            "background",
            "",
            info.background,
            true,
        );
        prefs.add(&settings);

        let permissions = adw::PreferencesGroup::builder()
            .title("Runtime Permissions")
            .description("Permissions marked as fixed are controlled by Android.")
            .build();
        if info.permissions.is_empty() {
            permissions.set_description(Some("This app requests no runtime permissions."));
        }
        for p in info.permissions {
            let title = permissions::describe(&p.name).map_or_else(|| p.name.clone(), |p| p.title);
            self.switch(
                &permissions,
                &title,
                &p.name,
                "permission",
                &p.name,
                p.granted,
                p.editable,
            );
        }
        prefs.add(&permissions);

        let actions = adw::PreferencesGroup::new();
        self.action(
            &actions,
            "Force Stop",
            "Stop the app until it is opened again",
            "force-stop",
            false,
        );
        let row = adw::ActionRow::builder()
            .title("Uninstall…")
            .activatable(true)
            .build();
        row.add_css_class("error");
        let s = self.clone();
        row.connect_activated(move |_| s.window.confirm_uninstall(&s.app));
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
        let (s, action, permission) = (self.clone(), action.to_string(), permission.to_string());
        row.connect_active_notify(move |row| {
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
        subtitle: &str,
        action: &str,
        confirm: bool,
    ) {
        let row = adw::ActionRow::builder()
            .title(title)
            .subtitle(subtitle)
            .activatable(true)
            .build();
        if confirm {
            row.add_css_class("error");
        }
        let (s, action) = (self.clone(), action.to_string());
        row.connect_activated(move |_| {
            let (s, action) = (s.clone(), action.clone());
            glib::spawn_future_local(async move {
                if confirm {
                    let d = adw::AlertDialog::new(Some(&format!("Clear {}’s Data?", s.app.name)), Some("All settings, accounts and files stored by this app will be permanently deleted."));
                    d.add_responses(&[("cancel", "_Cancel"), ("clear", "_Clear Data")]);
                    d.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
                    d.set_default_response(Some("cancel"));
                    d.set_close_response("cancel");
                    if d.choose_future(Some(&s.window.win)).await != "clear" { return; }
                }
                s.load(Command::new(&s.app.package, &action));
            });
        });
        group.add(&row);
    }
}
