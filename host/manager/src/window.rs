//! The main window. It shows one profile's apps. The header switches profiles
//! and installs apps, and its menu opens the profile's settings. Dropped files
//! open the install page. A profile whose image isn't downloaded shows the
//! download in place of its apps.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use innkeeper_core::desktop::{MANAGER_ID, PREFIX};
use innkeeper_core::launcher::{Session, Status};
use innkeeper_core::ops::{self, Event};
use innkeeper_core::profiles;
use innkeeper_core::xdg::{DesktopApp, Dirs};
use innkeeper_profile::profile::ProfileName;
use innkeeper_profile::profile::System;

use crate::errors::{self, Op};
use crate::{download, install, worker};

#[derive(Clone)]
pub struct Window(Rc<Inner>);

#[derive(Clone)]
pub struct WeakWindow(Weak<Inner>);

impl WeakWindow {
    pub fn upgrade(&self) -> Option<Window> {
        self.0.upgrade().map(Window)
    }
}

/// A window action's handler.
type Action = fn(&Window);

/// Shows the session's state and, while the window starts or stops it, the
/// progress.
pub type SessionView = Rc<dyn Fn(Option<Status>, Option<&str>)>;

pub struct Inner {
    /// The profile whose apps and session show.
    profile: RefCell<ProfileName>,
    /// The profile's system, after the window has read it.
    system: Cell<System>,
    pub win: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    pub nav: adw::NavigationView,
    /// Switches between the "empty", "list" and "download" pages.
    stack: gtk::Stack,
    empty: adw::StatusPage,
    download: download::Page,
    apps: adw::PreferencesGroup,
    rows: RefCell<Vec<adw::ActionRow>>,
    installed: RefCell<Vec<DesktopApp>>,
    /// The profile switcher in the header and its menu's list of profiles.
    switcher: gtk::MenuButton,
    profiles: gio::Menu,
    profile_action: gio::SimpleAction,
    /// What shows the session's state, while the profile's settings are open.
    session_view: RefCell<Option<SessionView>>,
    /// The profiles whose sessions this window is starting or stopping.
    session_busy: RefCell<HashSet<ProfileName>>,
    session_proxy: RefCell<Option<gio::DBusProxy>>,
    state_read: Cell<u64>,
    last_status: Cell<Option<Status>>,
    /// Watches the desktop entries, so installs from the CLI show up.
    monitor: RefCell<Option<gio::FileMonitor>>,
    refresh_pending: Cell<bool>,
}

impl std::ops::Deref for Window {
    type Target = Inner;
    fn deref(&self) -> &Inner {
        &self.0
    }
}

impl Window {
    pub fn new(app: &adw::Application, profile: ProfileName) -> Self {
        let empty = adw::StatusPage::builder()
            .icon_name(MANAGER_ID)
            .title("No Apps")
            .vexpand(true)
            .build();
        let apps = adw::PreferencesGroup::new();
        let list = adw::PreferencesPage::new();
        list.add(&apps);
        let download = download::Page::new();
        let stack = gtk::Stack::new();
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&list, Some("list"));
        stack.add_named(&download.page, Some("download"));

        let profiles = gio::Menu::new();
        let switcher_menu = gio::Menu::new();
        switcher_menu.append_section(None, &profiles);
        let section = gio::Menu::new();
        section.append(Some("_New Profile…"), Some("win.new-profile"));
        switcher_menu.append_section(None, &section);
        let switcher = gtk::MenuButton::builder()
            .label(profile.as_str())
            .always_show_arrow(true)
            .tooltip_text("Profiles")
            .menu_model(&switcher_menu)
            .css_classes(["flat"])
            .build();

        let menu = gio::Menu::new();
        let section = gio::Menu::new();
        section.append(Some("Profile _Settings"), Some("win.profile-settings"));
        section.append(Some("_About Innkeeper"), Some("app.about"));
        menu.append_section(None, &section);
        let header = adw::HeaderBar::builder().title_widget(&switcher).build();
        header.pack_start(
            &gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .tooltip_text("Install App")
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
        main.set_content(Some(&stack));
        let main_page = adw::NavigationPage::builder()
            .title("Innkeeper")
            .tag("main")
            .child(&main)
            .build();
        let nav = adw::NavigationView::new();
        nav.add(&main_page);

        // The drop hint covers the whole window.
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
            .title("Innkeeper")
            .icon_name(MANAGER_ID)
            .default_width(640)
            .default_height(720)
            .width_request(360)
            .height_request(400)
            .content(&toasts)
            .build();

        let profile_action = gio::SimpleAction::new_stateful(
            "profile",
            Some(glib::VariantTy::STRING),
            &profile.as_str().to_variant(),
        );
        let w = Window(Rc::new(Inner {
            system: Cell::new(System::Android),
            win,
            toasts,
            nav,
            stack,
            empty,
            download,
            apps,
            rows: RefCell::default(),
            installed: RefCell::default(),
            switcher,
            profiles,
            profile_action,
            session_view: RefCell::default(),
            session_busy: RefCell::default(),
            session_proxy: RefCell::default(),
            state_read: Cell::new(0),
            last_status: Cell::new(None),
            monitor: RefCell::default(),
            refresh_pending: Cell::new(false),
            profile: RefCell::new(profile),
        }));

        let weak = w.downgrade();
        w.profile_action.connect_activate(move |_, v| {
            let (Some(w), Some(name)) = (weak.upgrade(), v.and_then(|v| v.str())) else {
                return;
            };
            if name != w.profile().as_str()
                && let Err(e) = w.switch_profile(name)
            {
                w.show_error(Op::Profiles, None, &e);
            }
        });
        w.win.add_action(&w.profile_action);
        // Profiles also come and go through the CLI, so read them again
        // whenever the menu opens.
        let weak = w.downgrade();
        w.switcher.connect_active_notify(move |b| {
            if let Some(w) = weak.upgrade().filter(|_| b.is_active()) {
                w.read_profiles();
            }
        });
        let actions: [(&str, Action); 3] = [
            ("open", Window::choose_file),
            ("new-profile", crate::profile::create),
            ("profile-settings", crate::profile::settings),
        ];
        for (name, f) in actions {
            let action = gio::SimpleAction::new(name, None);
            let weak = w.downgrade();
            action.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            });
            w.win.add_action(&action);
        }
        let w2 = w.clone();
        w.download.button.connect_clicked(move |_| w2.prepare());

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
        w.refresh();
        w.watch_session();
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

    pub fn show_error(&self, op: Op, name: Option<&str>, err: &anyhow::Error) {
        let h = errors::humane(op, name, err);
        eprintln!("innkeeper-manager: {}", h.details);
        let d = adw::AlertDialog::new(Some(&h.heading), h.body.as_deref());
        d.add_response("close", "_Close");
        d.set_default_response(Some("close"));
        d.set_close_response("close");
        d.set_extra_child(Some(&details_expander(&h.details)));
        d.present(Some(&self.win));
    }

    fn choose_file(&self) {
        let filter = gtk::FileFilter::new();
        let system = self.system.get();
        filter.set_name(Some(&format!("{} Apps", system.title())));
        if system == System::Android {
            for m in innkeeper_core::desktop::PACKAGE_MIME_TYPES {
                filter.add_mime_type(m);
            }
            for p in ["*.apk", "*.apks", "*.xapk", "*.apkm"] {
                filter.add_pattern(p);
            }
        } else {
            filter.add_pattern("*.hap");
        }
        let filters = gio::ListStore::new::<gtk::FileFilter>();
        filters.append(&filter);
        let dialog = gtk::FileDialog::builder()
            .title("Install App")
            .accept_label("_Open")
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

    /// Shows the install page for `file`, which comes from drag and drop, the
    /// file chooser or the desktop's Open With. If the profile's image isn't
    /// ready, the page opens once it is.
    pub fn open_file(&self, file: &gio::File) {
        let d = &self.download;
        if d.active.get() || d.ready.get() != Some(true) {
            d.opened.borrow_mut().push(file.clone());
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

    /// The system that the profile runs.
    pub fn system(&self) -> System {
        self.system.get()
    }

    /// The installed app for `pkg` in this profile, if any.
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

    /// Reloads the profile's system, whether its image is ready, its app list
    /// and the list of profiles. The app list comes from the desktop entries
    /// and, if an Android session is running, its package list.
    pub fn refresh(&self) {
        self.read_profiles();
        let w = self.clone();
        glib::spawn_future_local(async move {
            let p = w.profile();
            let shown = p.clone();
            let (image, apps) = worker::run_quiet(move || {
                let image = Session::of_profile(&p)
                    .ok()
                    .map(|s| (s.system(), s.image_ready(), s.download_size()));
                let apps = (|| -> anyhow::Result<_> {
                    let system = profiles::settings(&p)?.system;
                    let packages = match system {
                        System::Android => ops::android_packages(&p),
                        System::Ohos => None,
                    };
                    Ok((system, ops::installed_apps(&p, packages.as_deref())?))
                })();
                (image, apps)
            })
            .await;
            // The window may have switched profiles meanwhile.
            if w.profile() != shown {
                return;
            }
            let d = &w.download;
            match image {
                Some((system, false, size)) => {
                    d.ready.set(Some(false));
                    d.offer(system, size);
                }
                _ => d.ready.set(Some(true)),
            }
            match apps {
                Ok((system, apps)) => {
                    w.set_system(system);
                    w.show_apps(apps);
                }
                Err(e) => eprintln!("innkeeper-manager: {e:#}"),
            }
            if d.ready.get() == Some(true) && !d.active.get() {
                for f in d.opened.take() {
                    w.open_file(&f);
                }
            }
        });
    }

    /// Lists the profiles in the switcher's menu.
    fn read_profiles(&self) {
        let w = self.downgrade();
        glib::spawn_future_local(async move {
            let names = worker::run_quiet(|| {
                profiles::list().map(|l| l.into_iter().map(|p| p.name).collect::<Vec<_>>())
            })
            .await;
            let Some(w) = w.upgrade() else {
                return;
            };
            match names {
                Ok(names) => {
                    w.profiles.remove_all();
                    for n in names {
                        // Underscores in a name aren't mnemonics.
                        let item = gio::MenuItem::new(Some(&n.replace('_', "__")), None);
                        item.set_action_and_target_value(
                            Some("win.profile"),
                            Some(&n.to_variant()),
                        );
                        w.profiles.append_item(&item);
                    }
                }
                Err(e) => eprintln!("innkeeper-manager: {e:#}"),
            }
        });
    }

    /// Adjusts the window to the profile's system.
    fn set_system(&self, system: System) {
        self.system.set(system);
        self.empty.set_description(Some(match system {
            System::Android => "Drop an APK here",
            System::Ohos => "Drop a HAP here",
        }));
    }

    fn show_apps(&self, apps: Vec<DesktopApp>) {
        for r in self.rows.borrow_mut().drain(..) {
            self.apps.remove(&r);
        }
        self.stack.set_visible_child_name(if self.download.shown() {
            "download"
        } else if apps.is_empty() {
            "empty"
        } else {
            "list"
        });
        let mut rows = self.rows.borrow_mut();
        for a in &apps {
            let row = self.app_row(a);
            self.apps.add(&row);
            rows.push(row);
        }
        *self.installed.borrow_mut() = apps;
    }

    fn app_row(&self, a: &DesktopApp) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title(&a.name)
            .subtitle(a.version.as_deref().unwrap_or_default())
            .use_markup(false)
            .activatable(true)
            .build();
        row.add_prefix(&app_icon(a.icon.as_ref(), 48));
        row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let (w, app) = (self.downgrade(), a.clone());
        row.connect_activated(move |_| {
            if let Some(w) = w.upgrade() {
                w.nav.push(&crate::app::page(&w, &app));
            }
        });
        row
    }

    /// Opens an app in its own window, starting the session if needed. Shows
    /// the progress with `progress`, and an error in a dialog.
    pub async fn launch(&self, pkg: &str, name: &str, progress: impl Fn(&str) + 'static) {
        // Get an activation token from this window, which has focus, so the
        // app's window can take focus.
        let token = gio::prelude::AppLaunchContextExt::startup_notify_id(
            &gtk::prelude::WidgetExt::display(&self.0.win).app_launch_context(),
            None::<&gio::AppInfo>,
            &[],
        )
        .and_then(|t| innkeeper_core::validate::activation_token(&t).map(str::to_string));
        let opts = ops::LaunchOptions {
            token,
            wait: false,
            new_window: false,
            size: None,
        };
        let (p, profile) = (pkg.to_string(), self.profile());
        let r = worker::run(
            move |rep| ops::run(&profile, &p, None, &opts, rep),
            move |e| {
                if let Event::Status(text) = e {
                    progress(&install::status_text(&text));
                }
            },
        )
        .await;
        if let Err(e) = r {
            self.show_error(Op::Open, Some(name), &e);
        }
        self.read_session();
    }

    /// Downloads the profile's image, when the user asks for it.
    fn prepare(&self) {
        let d = &self.download;
        if d.active.get() {
            return;
        }
        d.start();
        self.stack.set_visible_child_name("download");
        let w = self.clone();
        glib::spawn_future_local(async move {
            let p = w.profile();
            let progress = w.clone();
            let r = worker::run(
                move |rep| Session::of_profile(&p)?.prepare(rep),
                move |e| progress.download.update(e),
            )
            .await;
            match r {
                Ok(()) => {
                    w.download.finished();
                    w.refresh();
                }
                Err(e) => {
                    w.download.failed();
                    w.show_error(Op::Prepare, Some(w.system.get().title()), &e);
                }
            }
        });
    }

    /// Shows the session's state with `view` until [`Self::unwatch_session`].
    pub fn watch_session_with(&self, view: SessionView) {
        let busy = self.session_busy_here().then_some("");
        view(self.last_status.get(), busy);
        self.session_view.replace(Some(view));
    }

    pub fn unwatch_session(&self) {
        self.session_view.replace(None);
    }

    fn show_session(&self, st: Option<Status>, busy: Option<&str>) {
        let view = self.session_view.borrow().clone();
        if let Some(v) = view {
            v(st, busy);
        }
    }

    /// Watches the bridge's bus name and its Changed signal. The proxy doesn't
    /// auto-start the bridge, so showing the state doesn't start a session.
    fn watch_session(&self) {
        let weak = self.downgrade();
        glib::spawn_future_local(async move {
            let Some(profile) = weak.upgrade().map(|w| w.profile()) else {
                return;
            };
            let name = innkeeper_core::app_settings::bus_name(&profile);
            let proxy = gio::DBusProxy::for_bus_future(
                gio::BusType::Session,
                gio::DBusProxyFlags::DO_NOT_AUTO_START
                    | gio::DBusProxyFlags::DO_NOT_LOAD_PROPERTIES,
                None,
                &name,
                innkeeper_core::app_settings::PATH,
                innkeeper_core::app_settings::INTERFACE,
            )
            .await;
            let Some(w) = weak.upgrade() else {
                return;
            };
            // The window may have switched profiles meanwhile.
            if w.profile() != profile {
                return;
            }
            match proxy {
                Ok(proxy) => {
                    let weak = w.downgrade();
                    proxy.connect_notify_local(Some("g-name-owner"), move |_, _| {
                        if let Some(w) = weak.upgrade() {
                            w.read_session();
                        }
                    });
                    let weak = w.downgrade();
                    proxy.connect_local("g-signal", false, move |args| {
                        if args[2].get::<String>().ok().as_deref() == Some("Changed")
                            && let Some(w) = weak.upgrade()
                        {
                            w.read_session();
                        }
                        None
                    });
                    *w.session_proxy.borrow_mut() = Some(proxy);
                    w.read_session();
                }
                Err(e) => {
                    eprintln!("session bus: {e}");
                    w.show_session(None, None);
                }
            }
        });
    }

    /// The profile whose apps and session show.
    pub fn profile(&self) -> ProfileName {
        self.profile.borrow().clone()
    }

    /// Switches the window to profile `name` and remembers it for the next
    /// time the manager opens.
    pub fn switch_profile(&self, name: &str) -> anyhow::Result<()> {
        let name = ProfileName::new(name)?;
        crate::remember_profile(&name);
        self.switcher.set_label(&name);
        self.profile_action.set_state(&name.as_str().to_variant());
        self.profile.replace(name);
        self.nav.pop_to_tag("main");
        self.session_proxy.replace(None);
        self.last_status.set(None);
        self.show_session(None, None);
        // Files queued for the old profile's download don't go to this one.
        self.download.opened.borrow_mut().clear();
        self.download.ready.set(None);
        self.watch_session();
        self.refresh();
        Ok(())
    }

    pub fn read_session(&self) {
        let Some(proxy) = self.session_proxy.borrow().clone() else {
            return;
        };
        let generation = self.state_read.get() + 1;
        self.state_read.set(generation);
        let w = self.clone();
        glib::spawn_future_local(async move {
            let running = proxy.name_owner().is_some();
            let booted = if running {
                proxy
                    .call_future("State", None, gio::DBusCallFlags::NO_AUTO_START, 5000)
                    .await
                    .ok()
                    .and_then(|v| v.get::<(bool, Vec<String>)>())
                    .is_some_and(|(booted, _)| booted)
            } else {
                false
            };
            if w.state_read.get() != generation {
                return;
            }
            let st = Status { running, booted };
            let was_booted = w.last_status.replace(Some(st)).is_some_and(|s| s.booted);
            if !w.session_busy_here() {
                w.show_session(Some(st), None);
            }
            // A booted Android lists its packages, which repairs the app list.
            if booted && !was_booted {
                w.refresh();
            }
        });
    }

    /// Whether this window is starting or stopping the shown profile's
    /// session.
    fn session_busy_here(&self) -> bool {
        self.session_busy.borrow().contains(&self.profile())
    }

    /// Starts the shown profile's session if it is stopped, or stops it. The
    /// progress shows while the window shows that profile.
    pub fn toggle_session(&self) {
        let p = self.profile();
        if !self.session_busy.borrow_mut().insert(p.clone()) {
            return;
        }
        let running = self.last_status.get().is_some_and(|s| s.running);
        let system = self.system.get().title();
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
            let (w2, p2, p3) = (w.clone(), p.clone(), p.clone());
            let r = worker::run(
                move |rep| {
                    let s = Session::of_profile(&p2)?;
                    if running {
                        s.stop()
                    } else {
                        s.ensure(&|m| rep(Event::Status(m.to_string())))
                    }
                },
                move |e| {
                    if let Event::Status(m) = e
                        && w2.profile() == p3
                    {
                        w2.show_session(w2.last_status.get(), Some(&install::status_text(&m)));
                    }
                },
            )
            .await;
            w.session_busy.borrow_mut().remove(&p);
            if let Err(e) = r {
                let op = if running { Op::Stop } else { Op::Start };
                w.show_error(op, Some(system), &e);
            }
            if w.profile() == p {
                w.show_session(w.last_status.get(), None);
                w.read_session();
            }
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
