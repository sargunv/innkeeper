//! The profile dialogs. Profile Settings holds everything about the window's
//! profile: its name, whether it runs, its settings, and deleting it. New
//! Profile asks for a name and a system. Reads and writes run off the GTK
//! thread.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use innkeeper_core::launcher::Status;
use innkeeper_core::profiles;
use innkeeper_profile::profile::{Display, Settings, System, check_name};

use crate::{
    errors::Op,
    window::{WeakWindow, Window},
    worker,
};

/// The display choices. A device window of a fixed size lists its size.
const DISPLAYS: [(Display, &str, &str); 4] = [
    (Display::Windowed, "Windowed", ""),
    (Display::Device, "Device", "Resizable"),
    (Display::SmallPhone, "Small Phone", "720 × 1280"),
    (Display::MediumPhone, "Medium Phone", "1080 × 2400"),
];

/// Asks for a new profile's name and system, creates it and switches the
/// window to it.
pub fn create(w: &Window) {
    let d = adw::AlertDialog::new(Some("New Profile"), None);
    let entry = adw::EntryRow::builder()
        .title("Name")
        .activates_default(true)
        .build();
    let titles: Vec<_> = System::ALL.iter().map(|s| s.title()).collect();
    let system = adw::ComboRow::builder()
        .title("System")
        .model(&gtk::StringList::new(&titles))
        .build();
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["boxed-list"])
        .build();
    list.append(&entry);
    list.append(&system);
    d.set_extra_child(Some(&list));
    d.add_responses(&[("cancel", "_Cancel"), ("create", "C_reate")]);
    d.set_response_appearance("create", adw::ResponseAppearance::Suggested);
    d.set_response_enabled("create", false);
    d.set_default_response(Some("create"));
    d.set_close_response("cancel");
    let d2 = d.clone();
    entry.connect_changed(move |e| {
        let ok = check_name(&e.text()).is_ok();
        d2.set_response_enabled("create", ok);
        mark_name(e);
    });
    let w = w.clone();
    glib::spawn_future_local(async move {
        if d.choose_future(Some(&w.win)).await != "create" {
            return;
        }
        let name = entry.text().to_string();
        let sys = System::ALL[system.selected() as usize];
        let n = name.clone();
        match worker::run_quiet(move || profiles::create(&n, &Settings::new(sys))).await {
            Ok(()) => {
                if let Err(e) = w.switch_profile(&name) {
                    w.show_error(Op::Profiles, None, &e);
                }
            }
            Err(e) => w.show_error(Op::Profiles, None, &e),
        }
    });
}

/// Marks an entry whose text isn't a valid profile name.
fn mark_name(e: &adw::EntryRow) {
    if e.text().is_empty() || check_name(&e.text()).is_ok() {
        e.remove_css_class("error");
    } else {
        e.add_css_class("error");
    }
}

/// Opens Profile Settings for the window's profile.
pub fn settings(w: &Window) {
    let w = w.clone();
    glib::spawn_future_local(async move {
        let name = w.profile().to_string();
        let n = name.clone();
        let settings = match worker::run_quiet(move || profiles::settings(&n)).await {
            Ok(v) => v,
            Err(e) => return w.show_error(Op::Preferences, None, &e),
        };
        let dialog = adw::PreferencesDialog::builder()
            .title("Profile Settings")
            .search_enabled(false)
            .build();
        let page = Rc::new(Page {
            window: w.downgrade(),
            dialog: dialog.clone(),
            prefs: RefCell::default(),
            profile: RefCell::new(name),
            settings: RefCell::new(settings),
            running: Cell::new(false),
            busy: Cell::new(false),
        });
        page.show();
        // The dialog owns the page until it closes. The page's rows hold weak
        // references to it.
        let page = RefCell::new(Some(page));
        let weak = w.downgrade();
        dialog.connect_closed(move |_| {
            page.take();
            if let Some(w) = weak.upgrade() {
                w.unwatch_session();
            }
        });
        dialog.present(Some(&w.win));
    });
}

struct Page {
    window: WeakWindow,
    dialog: adw::PreferencesDialog,
    prefs: RefCell<Option<adw::PreferencesPage>>,
    /// The profile whose settings the dialog shows.
    profile: RefCell<String>,
    settings: RefCell<Settings>,
    /// Whether the profile runs, as the window last saw.
    running: Cell<bool>,
    /// True while a change is being saved.
    busy: Cell<bool>,
}

impl Page {
    fn error(&self, op: Op, e: &anyhow::Error) {
        if let Some(w) = self.window.upgrade() {
            w.show_error(op, None, e);
        }
    }

    fn name(&self) -> String {
        self.profile.borrow().clone()
    }

    /// Reads the settings again and shows them.
    fn load(self: &Rc<Self>) {
        let s = self.clone();
        let name = self.name();
        glib::spawn_future_local(async move {
            match worker::run_quiet(move || profiles::settings(&name)).await {
                Ok(settings) => {
                    s.settings.replace(settings);
                    s.show();
                }
                Err(e) => s.error(Op::Preferences, &e),
            }
        });
    }

    /// Runs `f` off the GTK thread while the page is insensitive. Reloads the
    /// page afterwards if `f` fails or `reload` is set.
    fn work(
        self: &Rc<Self>,
        reload: bool,
        f: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) {
        if self.busy.replace(true) {
            return;
        }
        let page = self.prefs.borrow().clone();
        if let Some(p) = &page {
            p.set_sensitive(false);
        }
        let s = self.clone();
        glib::spawn_future_local(async move {
            let result = worker::run_quiet(f).await;
            s.busy.set(false);
            if let Some(p) = &page {
                p.set_sensitive(true);
            }
            if let Err(e) = &result {
                s.error(Op::Preferences, e);
            }
            if reload || result.is_err() {
                s.load();
            }
        });
    }

    fn save(self: &Rc<Self>, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings.borrow_mut());
        let (name, next) = (self.name(), self.settings.borrow().clone());
        self.work(false, move || {
            // Keep the fields that have their own calls. `debug_row` sets adb
            // and hdc through `adb::set` or `hdc::set`, which apply the change
            // to the running session and authorize a key.
            // `profiles::set_keep_running` also sets the login command.
            profiles::update(&name, |s| {
                *s = Settings {
                    adb: s.adb,
                    adb_port: s.adb_port,
                    hdc: s.hdc,
                    hdc_port: s.hdc_port,
                    keep_running: s.keep_running,
                    ..next
                }
            })
            .map(drop)
        });
    }

    fn switch(
        self: &Rc<Self>,
        group: &adw::PreferencesGroup,
        title: &str,
        active: bool,
        set: fn(&mut Settings, bool),
    ) {
        let row = adw::SwitchRow::builder()
            .title(title)
            .active(active)
            .build();
        let s = Rc::downgrade(self);
        row.connect_active_notify(move |r| {
            if let Some(s) = s.upgrade() {
                s.save(|p| set(p, r.is_active()));
            }
        });
        group.add(&row);
    }

    fn show(self: &Rc<Self>) {
        let p = self.settings.borrow().clone();
        let page = adw::PreferencesPage::new();

        let profile = adw::PreferencesGroup::new();
        let (name, session) = self.profile_rows(&p);
        // The Name row comes second, so the dialog doesn't open with focus in
        // it.
        profile.add(&session);
        profile.add(&name);
        page.add(&profile);

        let display = adw::PreferencesGroup::new();
        display.add(&self.display_row(&p));
        page.add(&display);

        let desktop = adw::PreferencesGroup::builder()
            .title("Follow the Desktop")
            .build();
        self.switch(&desktop, "Dark Mode", p.follow_dark, |p, v| {
            p.follow_dark = v
        });
        // OpenHarmony has no accent color.
        if p.system != System::Ohos {
            self.switch(&desktop, "Accent Color", p.follow_accent, |p, v| {
                p.follow_accent = v
            });
        }
        self.switch(&desktop, "Language and Region", p.follow_locale, |p, v| {
            p.follow_locale = v
        });
        self.switch(&desktop, "Time Format", p.follow_clock, |p, v| {
            p.follow_clock = v
        });
        page.add(&desktop);
        page.add(&self.lifecycle_group(&p));

        let integration = adw::PreferencesGroup::builder()
            .title(format!("{} Integration", p.system.title()))
            .build();
        integration.add(&self.debug_row(&p));
        if p.system == System::Android {
            integration.add(&self.android_settings_row());
            integration.set_description(Some(
                "Like desktop apps, Android apps aren’t isolated from each other.",
            ));
        }
        page.add(&integration);

        let delete = adw::ButtonRow::builder()
            .title("_Delete Profile…")
            .use_underline(true)
            .build();
        delete.add_css_class("destructive-action");
        let s = Rc::downgrade(self);
        delete.connect_activated(move |_| {
            if let Some(s) = s.upgrade() {
                s.confirm_delete();
            }
        });
        let group = adw::PreferencesGroup::new();
        group.add(&delete);
        page.add(&group);

        if let Some(old) = self.prefs.replace(Some(page.clone())) {
            self.dialog.remove(&old);
        }
        self.dialog.add(&page);
    }

    /// The Name row, which renames a stopped profile, and the row that shows
    /// whether the profile runs, which starts or stops it.
    fn profile_rows(self: &Rc<Self>, p: &Settings) -> (adw::EntryRow, adw::ActionRow) {
        let name = adw::EntryRow::builder()
            .title("Name")
            .text(self.name())
            .show_apply_button(true)
            .build();
        name.connect_changed(mark_name);
        let s = Rc::downgrade(self);
        name.connect_apply(move |e| {
            if let Some(s) = s.upgrade() {
                s.rename(&e.text());
            }
        });

        let dot = gtk::Box::builder()
            .valign(gtk::Align::Center)
            .css_classes(["status-dot"])
            .build();
        let spinner = adw::Spinner::builder().visible(false).build();
        let button = gtk::Button::builder()
            .label("Start")
            .valign(gtk::Align::Center)
            .build();
        let session = adw::ActionRow::builder().title(p.system.title()).build();
        session.add_prefix(&dot);
        session.add_suffix(&spinner);
        session.add_suffix(&button);
        let w = self.window.clone();
        button.connect_clicked(move |_| {
            if let Some(w) = w.upgrade() {
                w.toggle_session();
            }
        });
        let (s, row, entry) = (Rc::downgrade(self), session.clone(), name.clone());
        let view = move |st: Option<Status>, busy: Option<&str>| {
            let running = st.is_some_and(|s| s.running);
            if let Some(s) = s.upgrade() {
                s.running.set(running);
            }
            // A running profile can't be renamed.
            entry.set_sensitive(!running && busy.is_none());
            dot.remove_css_class("running");
            dot.remove_css_class("starting");
            spinner.set_visible(busy.is_some());
            button.set_sensitive(busy.is_none() && st.is_some());
            button.set_label(if running { "Stop" } else { "Start" });
            let subtitle = match (busy, st) {
                (Some(b), _) => {
                    dot.add_css_class("starting");
                    b
                }
                (None, None) => "",
                (None, Some(s)) if !s.running => "Stopped",
                (None, Some(s)) if s.booted => {
                    dot.add_css_class("running");
                    "Running"
                }
                (None, Some(_)) => {
                    dot.add_css_class("starting");
                    "Starting…"
                }
            };
            row.set_subtitle(subtitle);
        };
        if let Some(w) = self.window.upgrade() {
            w.watch_session_with(Rc::new(view));
        }
        (name, session)
    }

    fn rename(self: &Rc<Self>, to: &str) {
        let from = self.name();
        if from == to {
            return;
        }
        if let Err(e) = check_name(to) {
            return self.error(Op::Profiles, &e);
        }
        let (f, t) = (from.clone(), to.to_string());
        let s = self.clone();
        let to = to.to_string();
        glib::spawn_future_local(async move {
            match worker::run_quiet(move || profiles::rename(&f, &t)).await {
                Ok(()) => {
                    s.profile.replace(to.clone());
                    if let Some(w) = s.window.upgrade()
                        && let Err(e) = w.switch_profile(&to)
                    {
                        w.show_error(Op::Profiles, None, &e);
                    }
                    s.load();
                }
                Err(e) => {
                    s.error(Op::Profiles, &e);
                    s.load();
                }
            }
        });
    }

    fn confirm_delete(self: &Rc<Self>) {
        let name = self.name();
        let d = adw::AlertDialog::new(
            Some(&format!("Delete Profile “{name}”?")),
            Some("Its apps, data and settings will be permanently deleted."),
        );
        d.add_responses(&[("cancel", "_Cancel"), ("delete", "_Delete")]);
        d.set_response_appearance("delete", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let s = self.clone();
        glib::spawn_future_local(async move {
            if d.choose_future(Some(&s.dialog)).await != "delete" {
                return;
            }
            if let Some(p) = s.prefs.borrow().as_ref() {
                p.set_sensitive(false);
            }
            let running = s.running.get();
            let r = worker::run_quiet(move || {
                if running {
                    profiles::stop(&name)?;
                }
                profiles::delete(&name)?;
                // Show the first profile left, which `first_profile` creates
                // if none are.
                crate::first_profile()
            })
            .await;
            let Some(w) = s.window.upgrade() else {
                return;
            };
            match r {
                Ok(next) => {
                    s.dialog.close();
                    if let Err(e) = w.switch_profile(&next) {
                        w.show_error(Op::Profiles, None, &e);
                    }
                }
                Err(e) => {
                    w.show_error(Op::Profiles, None, &e);
                    s.load();
                }
            }
        });
    }

    /// The Display row. It lists the display modes the profile's system
    /// supports. A change applies at the next start.
    fn display_row(self: &Rc<Self>, p: &Settings) -> adw::ComboRow {
        let choices: Vec<_> = DISPLAYS
            .into_iter()
            .filter(|c| p.system.displays().contains(&c.0))
            .collect();
        let model = gtk::StringList::new(&choices.iter().map(|c| c.1).collect::<Vec<_>>());
        let current = choices.iter().position(|c| c.0 == p.display).unwrap_or(0);
        let row = adw::ComboRow::builder()
            .title("Display")
            .subtitle(choices[current].2)
            .model(&model)
            .selected(current as u32)
            .build();
        let s = Rc::downgrade(self);
        row.connect_selected_notify(move |r| {
            let (Some(s), Some(&(d, _, size))) = (s.upgrade(), choices.get(r.selected() as usize))
            else {
                return;
            };
            r.set_subtitle(size);
            s.save(|p| p.display = d);
            if s.running.get() {
                s.dialog
                    .add_toast(adw::Toast::new("Applies at the next start"));
            }
        });
        row
    }

    fn lifecycle_group(self: &Rc<Self>, p: &Settings) -> adw::PreferencesGroup {
        let keep_running = p.keep_running;
        let group = adw::PreferencesGroup::builder()
            .title("When No Apps Are Open")
            .build();
        let stop = gtk::CheckButton::builder()
            .active(!keep_running)
            .valign(gtk::Align::Center)
            .build();
        let keep = gtk::CheckButton::builder()
            .group(&stop)
            .active(keep_running)
            .valign(gtk::Align::Center)
            .build();
        let row = adw::ActionRow::builder()
            .title("Stop After")
            .subtitle("Minutes")
            .activatable_widget(&stop)
            .build();
        row.add_prefix(&stop);
        let minutes = gtk::SpinButton::with_range(1.0, 120.0, 1.0);
        minutes.set_value(f64::from(p.idle_minutes));
        minutes.set_valign(gtk::Align::Center);
        minutes.set_sensitive(!keep_running);
        row.add_suffix(&minutes);
        group.add(&row);
        let row = adw::ActionRow::builder()
            .title("Keep Running")
            .subtitle("Starts at login")
            .activatable_widget(&keep)
            .build();
        row.add_prefix(&keep);
        group.add(&row);
        let s = Rc::downgrade(self);
        let spin = minutes.clone();
        keep.connect_toggled(move |r| {
            spin.set_sensitive(!r.is_active());
            if let Some(s) = s.upgrade() {
                let (name, on) = (s.name(), r.is_active());
                s.settings.borrow_mut().keep_running = on;
                s.work(false, move || {
                    profiles::set_keep_running(&name, on).map(drop)
                });
            }
        });
        let s = Rc::downgrade(self);
        minutes.connect_value_changed(move |r| {
            if let Some(s) = s.upgrade() {
                s.save(|p| p.idle_minutes = r.value_as_int() as u32);
            }
        });
        group
    }

    /// The adb switch for Android, or the hdc switch for OpenHarmony. Its
    /// subtitle shows how to connect while it is on.
    fn debug_row(self: &Rc<Self>, p: &Settings) -> adw::SwitchRow {
        let ohos = p.system == System::Ohos;
        let (title, on) = if ohos { ("hdc", p.hdc) } else { ("adb", p.adb) };
        // The port, and so the command, exists only while the switch is on.
        let connect = match (on, ohos) {
            (false, _) => None,
            (true, true) => Some(innkeeper_core::hdc::connect_command(p)),
            (true, false) => Some(innkeeper_core::adb::serial(p)),
        };
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(
                connect
                    .map(|c| format!("<tt>{}</tt>", glib::markup_escape_text(&c)))
                    .unwrap_or_default(),
            )
            .subtitle_selectable(true)
            .use_markup(true)
            .active(on)
            .build();
        let s = Rc::downgrade(self);
        row.connect_active_notify(move |r| {
            let Some(s) = s.upgrade() else {
                return;
            };
            let (n, on) = (s.name(), r.is_active());
            // Reload afterwards so the subtitle shows the assigned port.
            s.work(true, move || {
                let missing = if ohos {
                    innkeeper_core::hdc::set(&n, on, &innkeeper_core::ops::quiet)?;
                    innkeeper_core::hdc::missing_key()
                } else {
                    innkeeper_core::adb::set(&n, on, &innkeeper_core::ops::quiet)?;
                    innkeeper_core::adb::missing_key()
                };
                match missing.filter(|_| on) {
                    Some(why) => Err(anyhow::anyhow!("{title} is on, but {why}")),
                    None => Ok(()),
                }
            });
        });
        row
    }

    /// A row that opens Android's Settings app in its own window.
    fn android_settings_row(self: &Rc<Self>) -> adw::ActionRow {
        let row = adw::ActionRow::builder()
            .title("Android Settings")
            .activatable(true)
            .build();
        let spinner = adw::Spinner::builder().visible(false).build();
        row.add_suffix(&spinner);
        row.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        let w = self.window.clone();
        row.connect_activated(move |r| {
            let Some(w) = w.upgrade() else {
                return;
            };
            let (r, spinner) = (r.clone(), spinner.clone());
            glib::spawn_future_local(async move {
                r.set_sensitive(false);
                spinner.set_visible(true);
                w.launch("com.android.settings", "Android Settings", |_| {})
                    .await;
                spinner.set_visible(false);
                r.set_sensitive(true);
            });
        });
        row
    }
}
