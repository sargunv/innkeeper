//! The main window: installed apps, the session's status, drag and drop, the
//! navigation to the install page, and the first run's image download.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use linuxapk_core::desktop::{MANAGER_ID, PREFIX};
use linuxapk_core::launcher::{Session, Status};
use linuxapk_core::ops::{self, Event};
use linuxapk_core::xdg::{DesktopApp, Dirs};

use crate::errors::{self, Op};
use crate::{install, worker};

/// How often the session row refreshes while the window is open.
const POLL_SECS: u32 = 3;

#[derive(Clone)]
pub struct Window(Rc<Inner>);

pub struct WeakWindow(Weak<Inner>);

impl WeakWindow {
    pub fn upgrade(&self) -> Option<Window> {
        self.0.upgrade().map(Window)
    }
}

pub struct Inner {
    pub win: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    pub nav: adw::NavigationView,
    /// "empty", "list", or "preparing" while the image downloads.
    stack: gtk::Stack,
    preparing: Preparing,
    /// The one-time single-uid notice.
    banner: adw::Banner,
    apps: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
    installed: RefCell<Vec<DesktopApp>>,
    sessions: [SessionRow; 2],
    /// The session is being started or stopped from here.
    session_busy: Cell<bool>,
    polling: Cell<bool>,
    last_status: Cell<Option<Status>>,
    /// Watches the desktop entries, so installs from the CLI show up.
    monitor: RefCell<Option<gio::FileMonitor>>,
    refresh_pending: Cell<bool>,
}

/// The first run's "Preparing Android" page, shown while the image downloads.
struct Preparing {
    page: adw::StatusPage,
    spinner: adw::Spinner,
    retry: gtk::Button,
    active: Cell<bool>,
    /// Files opened meanwhile, shown once the image is ready.
    opened: RefCell<Vec<gio::File>>,
}

impl Preparing {
    fn new() -> Self {
        let spinner = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .build();
        let retry = gtk::Button::builder()
            .label("_Try Again")
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .visible(false)
            .build();
        let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
        content.append(&spinner);
        content.append(&retry);
        let page = adw::StatusPage::builder()
            .icon_name("folder-download-symbolic")
            .title("Preparing Android")
            .description("Downloading Android (about 1.2 GB). This happens only once.")
            .child(&content)
            .vexpand(true)
            .build();
        Self {
            page,
            spinner,
            retry,
            active: Cell::new(false),
            opened: RefCell::default(),
        }
    }
}

impl std::ops::Deref for Window {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

/// The "Android" row: state, memory, start/stop. The window has one in the
/// empty state and one above the app list.
struct SessionRow {
    group: adw::PreferencesGroup,
    row: adw::ActionRow,
    dot: gtk::Box,
    button: gtk::Button,
    spinner: adw::Spinner,
}

impl SessionRow {
    fn new() -> Self {
        let dot = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["status-dot"])
            .build();
        let spinner = adw::Spinner::builder().visible(false).build();
        let button = gtk::Button::builder()
            .label("Start")
            .valign(gtk::Align::Center)
            .build();
        let row = adw::ActionRow::builder()
            .title("Android")
            .subtitle("Checking…")
            .build();
        row.add_prefix(&dot);
        row.add_suffix(&spinner);
        row.add_suffix(&button);
        let group = adw::PreferencesGroup::builder().title("Session").build();
        group.add(&row);
        Self {
            group,
            row,
            dot,
            button,
            spinner,
        }
    }

    fn show(&self, st: Option<Status>, busy: Option<&str>) {
        self.dot.remove_css_class("running");
        self.dot.remove_css_class("starting");
        if let Some(b) = busy {
            self.row.set_subtitle(b);
            self.spinner.set_visible(true);
            self.button.set_sensitive(false);
            self.dot.add_css_class("starting");
            return;
        }
        self.spinner.set_visible(false);
        self.button.set_sensitive(true);
        match st {
            None => {
                self.row
                    .set_subtitle("Not set up (the session launcher wasn’t found)");
                self.button.set_sensitive(false);
            }
            Some(s) if !s.running => {
                self.row
                    .set_subtitle("Stopped · Starts by itself when you open an app");
                self.button.set_label("Start");
            }
            Some(s) => {
                let mem = s
                    .memory
                    .map(|m| format!(" · {} in use", glib::format_size(m)))
                    .unwrap_or_default();
                if s.booted {
                    self.row.set_subtitle(&format!("Running{mem}"));
                    self.dot.add_css_class("running");
                } else {
                    self.row.set_subtitle(&format!("Starting{mem}"));
                    self.dot.add_css_class("starting");
                }
                self.button.set_label("Stop");
            }
        }
    }
}

impl Window {
    pub fn new(app: &adw::Application) -> Self {
        let sessions = [SessionRow::new(), SessionRow::new()];

        // Empty state.
        let open_btn = gtk::Button::builder()
            .label("_Open APK…")
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .action_name("win.open")
            .build();
        let empty_box = gtk::Box::new(gtk::Orientation::Vertical, 36);
        empty_box.append(&open_btn);
        empty_box.append(
            &adw::Clamp::builder()
                .maximum_size(420)
                .child(&sessions[0].group)
                .build(),
        );
        let empty = adw::StatusPage::builder()
            .icon_name(MANAGER_ID)
            .title("No Android Apps")
            .description("Drag an APK here or open one to install it")
            .child(&empty_box)
            .vexpand(true)
            .build();

        // List.
        let apps = adw::PreferencesGroup::builder()
            .title("Installed Apps")
            .build();
        let list = adw::PreferencesPage::new();
        list.add(&apps);
        list.add(&sessions[1].group);

        let preparing = Preparing::new();
        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&list, Some("list"));
        stack.add_named(&preparing.page, Some("preparing"));

        let banner = adw::Banner::builder()
            .button_label("_Learn More")
            .use_markup(false)
            .build();

        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some("_About Linux.apk"), Some("app.about"));
        menu.append_section(None, &section);
        let header = adw::HeaderBar::new();
        header.pack_start(
            &gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .tooltip_text("Install an App")
                .action_name("win.open")
                .build(),
        );
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .tooltip_text("Main Menu")
                .menu_model(&menu)
                .primary(true)
                .build(),
        );
        let main = adw::ToolbarView::new();
        main.add_top_bar(&header);
        main.add_top_bar(&banner);
        main.set_content(Some(&stack));
        let main_page = adw::NavigationPage::builder()
            .title("Linux.apk")
            .tag("main")
            .child(&main)
            .build();
        let nav = adw::NavigationView::new();
        nav.add(&main_page);

        // Drop hint over everything.
        let hint = adw::StatusPage::builder()
            .icon_name("document-send-symbolic")
            .title("Drop to Install")
            .css_classes(["drop-hint"])
            .build();
        let reveal = gtk::Revealer::builder()
            .transition_type(gtk::RevealerTransitionType::Crossfade)
            .can_target(false)
            .child(&hint)
            .build();
        let overlay = gtk::Overlay::builder().child(&nav).build();
        overlay.add_overlay(&reveal);
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&overlay));

        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("Linux.apk")
            .icon_name(MANAGER_ID)
            .default_width(640)
            .default_height(720)
            .width_request(360)
            .height_request(400)
            .content(&toasts)
            .build();

        let w = Window(Rc::new(Inner {
            win,
            toasts,
            nav,
            stack,
            preparing,
            banner,
            apps,
            rows: RefCell::default(),
            installed: RefCell::default(),
            sessions,
            session_busy: Cell::new(false),
            polling: Cell::new(false),
            last_status: Cell::new(None),
            monitor: RefCell::default(),
            refresh_pending: Cell::new(false),
        }));

        let open = gio::SimpleAction::new("open", None);
        let w2 = w.clone();
        open.connect_activate(move |_, _| w2.choose_file());
        w.win.add_action(&open);

        for s in &w.sessions {
            let w2 = w.clone();
            s.button.connect_clicked(move |_| w2.toggle_session());
        }
        let w2 = w.clone();
        w.preparing.retry.connect_clicked(move |_| w2.prepare());
        let w2 = w.clone();
        w.banner.connect_button_clicked(move |b| {
            b.set_revealed(false);
            gtk::UriLauncher::new(ops::SINGLE_UID_DOCS).launch(
                Some(&w2.win),
                gio::Cancellable::NONE,
                |_| {},
            );
        });

        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let r = reveal.clone();
        drop.connect_enter(move |_, _, _| {
            r.set_reveal_child(true);
            gdk::DragAction::COPY
        });
        let r = reveal.clone();
        drop.connect_leave(move |_| r.set_reveal_child(false));
        let w2 = w.clone();
        drop.connect_drop(move |_, value, _, _| {
            reveal.set_reveal_child(false);
            let Ok(list) = value.get::<gdk::FileList>() else {
                return false;
            };
            for f in list.files() {
                w2.open_file(&f);
            }
            true
        });
        w.win.add_controller(drop);

        w.watch_entries();
        let w2 = w.clone();
        glib::spawn_future_local(async move {
            worker::run_quiet(ops::refresh_entries).await;
            w2.refresh();
        });
        w.prepare_if_needed();
        w.poll_session();
        let weak = Rc::downgrade(&w.0);
        glib::timeout_add_seconds_local(POLL_SECS, move || match weak.upgrade() {
            Some(inner) => {
                Window(inner).poll_session();
                glib::ControlFlow::Continue
            }
            None => glib::ControlFlow::Break,
        });
        w
    }

    pub fn downgrade(&self) -> WeakWindow {
        WeakWindow(Rc::downgrade(&self.0))
    }

    pub fn present(&self) {
        self.win.present();
    }

    pub fn toast(&self, t: adw::Toast) {
        self.toasts.add_toast(t);
    }

    /// A plain toast; `text` isn't markup. Progress toasts are dismissed
    /// when their operation ends.
    pub fn say(&self, text: &str) -> adw::Toast {
        let t = adw::Toast::builder().title(text).use_markup(false).build();
        self.toast(t.clone());
        t
    }

    pub fn show_error(&self, op: Op, name: Option<&str>, err: &anyhow::Error) {
        let h = errors::humane(op, name, err);
        eprintln!("linuxapk-manager: {}", h.details);
        let d = adw::AlertDialog::new(Some(&h.heading), Some(&h.body));
        d.add_response("close", "_Close");
        d.set_default_response(Some("close"));
        d.set_close_response("close");
        d.set_extra_child(Some(&details_expander(&h.details)));
        d.present(Some(&self.win));
    }

    fn choose_file(&self) {
        let filter = gtk::FileFilter::new();
        filter.set_name(Some("Android apps"));
        for m in linuxapk_core::desktop::PACKAGE_MIME_TYPES {
            filter.add_mime_type(m);
        }
        for p in ["*.apk", "*.apks", "*.xapk", "*.apkm"] {
            filter.add_pattern(p);
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Install an Android App")
            .accept_label("_Install")
            .filters(&filters)
            .default_filter(&filter)
            .modal(true)
            .build();
        let w = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(f) = dialog.open_future(Some(&w.win)).await {
                w.open_file(&f);
            }
        });
    }

    /// Shows the install page for `file` (drag and drop, the file chooser, or
    /// the desktop's "open with"); after the first run's download, if that is
    /// still going.
    pub fn open_file(&self, file: &gio::File) {
        if self.preparing.active.get() {
            self.preparing.opened.borrow_mut().push(file.clone());
            return;
        }
        match file.path() {
            Some(p) => {
                let page = install::page(self, p);
                self.nav.push(&page);
            }
            None => self.show_error(
                Op::Read,
                None,
                &anyhow::anyhow!("{} isn't a local file", file.uri()),
            ),
        }
    }

    /// The installed app for `pkg` in this instance, if any.
    pub fn installed(&self, pkg: &str) -> Option<DesktopApp> {
        self.installed
            .borrow()
            .iter()
            .find(|a| a.package == pkg)
            .cloned()
    }

    fn watch_entries(&self) {
        let dir = gio::File::for_path(Dirs::from_env().applications());
        let Ok(m) = dir.monitor_directory(gio::FileMonitorFlags::NONE, gio::Cancellable::NONE)
        else {
            return;
        };
        let weak = Rc::downgrade(&self.0);
        m.connect_changed(move |_, f, _, _| {
            let ours = f
                .basename()
                .is_some_and(|n| n.to_string_lossy().starts_with(PREFIX));
            let Some(inner) = weak.upgrade() else { return };
            if !ours || inner.refresh_pending.replace(true) {
                return;
            }
            // Coalesce the burst of events an install makes.
            let weak = Rc::downgrade(&inner);
            glib::timeout_add_local_once(std::time::Duration::from_millis(500), move || {
                if let Some(inner) = weak.upgrade() {
                    inner.refresh_pending.set(false);
                    Window(inner).refresh();
                }
            });
        });
        *self.monitor.borrow_mut() = Some(m);
    }

    /// Reloads the app list (desktop entries, plus the session's own list if
    /// it is running).
    pub fn refresh(&self) {
        let w = self.clone();
        glib::spawn_future_local(async move {
            let apps =
                worker::run_quiet(|| ops::installed_apps(ops::android_packages().as_deref())).await;
            w.show_apps(apps);
        });
    }

    fn show_apps(&self, apps: Vec<DesktopApp>) {
        for r in self.rows.borrow_mut().drain(..) {
            self.apps.remove(&r);
        }
        if !self.preparing.active.get() {
            self.stack
                .set_visible_child_name(if apps.is_empty() { "empty" } else { "list" });
        }
        let mut rows = self.rows.borrow_mut();
        for a in &apps {
            let row = self.app_row(a);
            self.apps.add(&row);
            rows.push(row);
        }
        *self.installed.borrow_mut() = apps;
    }

    fn app_row(&self, a: &DesktopApp) -> adw::ActionRow {
        let subtitle = match &a.version {
            Some(v) => format!("{} · {v}", a.package),
            None => a.package.clone(),
        };
        let row = adw::ActionRow::builder()
            .title(&a.name)
            .subtitle(&subtitle)
            .use_markup(false)
            .build();
        row.add_prefix(&app_icon(a.icon.as_ref(), 48));
        let launch = gtk::Button::builder()
            .label("Open")
            .valign(gtk::Align::Center)
            .build();
        let remove = gtk::Button::builder()
            .icon_name("user-trash-symbolic")
            .tooltip_text("Uninstall")
            .valign(gtk::Align::Center)
            .css_classes(["flat"])
            .build();
        row.add_suffix(&launch);
        row.add_suffix(&remove);
        row.set_activatable(true);
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let (w, app) = (self.clone(), a.clone());
        row.connect_activated(move |_| w.nav.push(&crate::app::page(&w, &app)));
        let (w, pkg, name) = (self.clone(), a.package.clone(), a.name.clone());
        launch.connect_clicked(move |_| w.launch(&pkg, &name));
        let (w, a2) = (self.clone(), a.clone());
        remove.connect_clicked(move |_| w.confirm_uninstall(&a2));
        row
    }

    /// Opens an app in its own window (starting the session if needed).
    pub fn launch(&self, pkg: &str, name: &str) {
        let toast = self.say(&format!("Opening {name}…"));
        // An activation token from our window (which has focus), so the
        // app's window may take it.
        let token = gio::prelude::AppLaunchContextExt::startup_notify_id(
            &gtk::prelude::WidgetExt::display(&self.0.win).app_launch_context(),
            None::<&gio::AppInfo>,
            &[],
        )
        .and_then(|t| linuxapk_core::validate::activation_token(&t).map(str::to_string));
        let opts = ops::LaunchOptions { token, wait: false };
        let (w, pkg, name) = (self.clone(), pkg.to_string(), name.to_string());
        glib::spawn_future_local(async move {
            let p = pkg.clone();
            let r = worker::run(move |rep| ops::run(&p, None, &opts, rep), |_| {}).await;
            toast.dismiss();
            if let Err(e) = r {
                w.show_error(Op::Open, Some(&name), &e);
            }
            w.poll_session();
        });
    }

    pub fn confirm_uninstall(&self, a: &DesktopApp) {
        let d = adw::AlertDialog::new(
            Some(&format!("Uninstall {}?", a.name)),
            Some("It will be removed from Android and from the app menu."),
        );
        let keep = adw::SwitchRow::builder()
            .title("Keep App Data")
            .subtitle("Installing it again restores its settings and files")
            .build();
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
        let (w, a) = (self.clone(), a.clone());
        glib::spawn_future_local(async move {
            if d.choose_future(Some(&w.win)).await != "uninstall" {
                return;
            }
            let keep_data = keep.is_active();
            let toast = w.say(&format!("Uninstalling {}…", a.name));
            let pkg = a.package.clone();
            let r = worker::run(move |rep| ops::uninstall(&pkg, keep_data, rep), |_| {}).await;
            toast.dismiss();
            match r {
                Ok(_) => {
                    w.nav.pop_to_tag("main");
                    _ = w.say(&format!(
                        "{} uninstalled{}",
                        a.name,
                        if keep_data { " (data kept)" } else { "" }
                    ))
                }
                Err(e) => w.show_error(Op::Uninstall, Some(&a.name), &e),
            }
            w.refresh();
        });
    }

    fn show_session(&self, st: Option<Status>, busy: Option<&str>) {
        for s in &self.sessions {
            s.show(st, busy);
        }
    }

    /// Downloads the image if it isn't there yet (the first run).
    fn prepare_if_needed(&self) {
        let w = self.clone();
        glib::spawn_future_local(async move {
            let ready = worker::run_quiet(|| Session::find().map(|s| s.image_ready())).await;
            // No launcher: the session row says so.
            if ready.is_ok_and(|r| !r) {
                w.prepare();
            }
        });
    }

    fn prepare(&self) {
        let p = &self.preparing;
        p.active.set(true);
        p.spinner.set_visible(true);
        p.retry.set_visible(false);
        self.stack.set_visible_child_name("preparing");
        let w = self.clone();
        glib::spawn_future_local(async move {
            let r = worker::run_quiet(|| Session::find()?.prepare()).await;
            let p = &w.preparing;
            p.spinner.set_visible(false);
            match r {
                Ok(()) => {
                    p.active.set(false);
                    w.refresh();
                    for f in p.opened.take() {
                        w.open_file(&f);
                    }
                }
                Err(e) => {
                    p.retry.set_visible(true);
                    w.show_error(Op::Prepare, None, &e);
                }
            }
        });
    }

    /// Reads the session's status off the UI thread and shows it, and the
    /// single-uid notice the first time the session runs in that mode.
    pub fn poll_session(&self) {
        if self.polling.replace(true) {
            return;
        }
        let w = self.clone();
        glib::spawn_future_local(async move {
            let (st, notice) = worker::run_quiet(|| {
                let st = Session::find().ok().map(|s| s.status());
                (st, ops::single_uid_notice(st.is_some_and(|s| s.running)))
            })
            .await;
            if let Some(n) = notice {
                w.banner.set_title(n);
                w.banner.set_revealed(true);
            }
            w.polling.set(false);
            if w.session_busy.get() {
                return;
            }
            let was_booted = w.last_status.get().is_some_and(|s| s.booted);
            w.last_status.set(st);
            w.show_session(st, None);
            // Apps installed without a desktop entry show up once it boots.
            if !was_booted && st.is_some_and(|s| s.booted) {
                w.refresh();
            }
        });
    }

    fn toggle_session(&self) {
        let running = self.last_status.get().is_some_and(|s| s.running);
        self.session_busy.set(true);
        self.show_session(
            self.last_status.get(),
            Some(if running {
                "Stopping…"
            } else {
                "Starting…"
            }),
        );
        let w = self.clone();
        glib::spawn_future_local(async move {
            let w2 = w.clone();
            let r = worker::run(
                move |rep| {
                    let s = Session::find()?;
                    if running {
                        s.stop()
                    } else {
                        s.ensure(&|m| rep(Event::Status(m.to_string())))
                    }
                },
                move |e| {
                    if let Event::Status(m) = e {
                        w2.show_session(None, Some(&install::status_text(&m)));
                    }
                },
            )
            .await;
            w.session_busy.set(false);
            if let Err(e) = r {
                w.show_error(Op::Session, None, &e);
            }
            w.poll_session();
        });
    }
}

/// An app's icon from its file, or the manager's icon.
pub fn app_icon(path: Option<&PathBuf>, size: i32) -> gtk::Image {
    let img = match path {
        Some(p) => gtk::Image::from_file(p),
        None => gtk::Image::from_icon_name(MANAGER_ID),
    };
    img.set_pixel_size(size);
    img.add_css_class("icon-dropshadow");
    img
}

/// A collapsed "Details" section with the raw error text.
pub fn details_expander(text: &str) -> gtk::Expander {
    let label = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .xalign(0.0)
        .css_classes(["monospace", "caption"])
        .build();
    gtk::Expander::builder()
        .label("Details")
        .child(&label)
        .build()
}
