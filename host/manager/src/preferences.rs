//! The Profile Settings page: the shown profile's settings, and Keep Running,
//! which every profile shares. An OpenHarmony profile has only its display:
//! the rest is Android's. Reads and writes run off the GTK thread.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use innkeeper_core::preferences;
use innkeeper_core::profiles;
use innkeeper_profile::preferences::Preferences;
use innkeeper_profile::profile::{Display, Settings, System};

use crate::{
    errors::Op,
    window::{WeakWindow, Window},
    worker,
};

const ISOLATION_DOCS: &str =
    "https://github.com/sargunv/innkeeper/blob/main/docs/security.md#app-isolation";

struct Page {
    window: WeakWindow,
    content: gtk::Box,
    /// The profile whose settings show.
    profile: String,
    settings: RefCell<Settings>,
    keep_running: Cell<bool>,
    busy: Cell<bool>,
}

/// The display choices, with their descriptions.
const DISPLAYS: [(Display, &str, &str); 4] = [
    (
        Display::Windowed,
        "Windowed",
        "Each app in a window of its own",
    ),
    (
        Display::SmallPhone,
        "Small Phone",
        "One device window, 720 × 1280",
    ),
    (
        Display::MediumPhone,
        "Medium Phone",
        "One device window, 1080 × 2400",
    ),
    (
        Display::MediumTablet,
        "Medium Tablet",
        "One device window, 2560 × 1600",
    ),
];

pub fn page(window: &Window) -> adw::NavigationPage {
    let profile = window.profile().to_string();
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let toolbar = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new("Profile Settings", &profile)));
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&content));
    let page = adw::NavigationPage::builder()
        .title("Profile Settings")
        .child(&toolbar)
        .build();
    let state = Rc::new(Page {
        window: window.downgrade(),
        content,
        profile,
        settings: RefCell::new(Settings::default()),
        keep_running: Cell::new(false),
        busy: Cell::new(false),
    });
    page.connect_showing(move |_| state.load());
    page
}

impl Page {
    fn load(self: &Rc<Self>) {
        let s = self.clone();
        let name = self.profile.clone();
        glib::spawn_future_local(async move {
            let r = worker::run_quiet(move || {
                Ok::<_, anyhow::Error>((profiles::settings(&name)?, Preferences::load()?))
            })
            .await;
            match r {
                Ok((settings, prefs)) => {
                    s.settings.replace(settings);
                    s.keep_running.set(prefs.keep_running);
                    s.show();
                }
                Err(e) => s.error(&e),
            }
        });
    }

    fn error(&self, e: &anyhow::Error) {
        if let Some(w) = self.window.upgrade() {
            w.show_error(Op::Preferences, None, e);
        }
    }

    /// Runs `f` off the GTK thread with the page insensitive; reloads after
    /// a failure, or always with `reload`.
    fn work(
        self: &Rc<Self>,
        reload: bool,
        f: impl FnOnce() -> anyhow::Result<()> + Send + 'static,
    ) {
        if self.busy.replace(true) {
            return;
        }
        self.content.set_sensitive(false);
        let s = self.clone();
        glib::spawn_future_local(async move {
            let result = worker::run_quiet(f).await;
            s.busy.set(false);
            s.content.set_sensitive(true);
            if let Err(e) = &result {
                s.error(e);
            }
            if reload || result.is_err() {
                s.load();
            }
        });
    }

    fn save(self: &Rc<Self>, change: impl FnOnce(&mut Settings)) {
        change(&mut self.settings.borrow_mut());
        let (name, next) = (self.profile.clone(), self.settings.borrow().clone());
        self.work(false, move || {
            // adb has its own path (it applies live and needs a key).
            profiles::update(&name, |s| {
                *s = Settings {
                    adb: s.adb,
                    adb_port: s.adb_port,
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
        subtitle: &str,
        active: bool,
        set: fn(&mut Settings, bool),
    ) -> adw::SwitchRow {
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(subtitle)
            .active(active)
            .build();
        let s = Rc::downgrade(self);
        row.connect_active_notify(move |r| {
            if let Some(s) = s.upgrade() {
                s.save(|p| set(p, r.is_active()));
            }
        });
        group.add(&row);
        row
    }

    fn show(self: &Rc<Self>) {
        while let Some(child) = self.content.first_child() {
            self.content.remove(&child);
        }
        let p = self.settings.borrow().clone();
        let page = adw::PreferencesPage::new();
        page.set_vexpand(true);

        let display = adw::PreferencesGroup::new();
        display.add(&self.display_row(&p));
        page.add(&display);
        if p.system == System::Ohos {
            page.add(
                &adw::PreferencesGroup::builder()
                    .title("OpenHarmony")
                    .description(
                        "This profile runs OpenHarmony’s built-in apps in one phone window. \
                         Installing apps and desktop input aren’t available yet.",
                    )
                    .build(),
            );
            self.content.append(&page);
            return;
        }

        let desktop = adw::PreferencesGroup::builder().title("Follow the Desktop")
            .description("Changes apply live. Turn a setting off to choose it in Android Settings; its current value is kept.").build();
        self.switch(
            &desktop,
            "Dark Mode",
            "Use the desktop’s light or dark appearance",
            p.follow_dark,
            |p, v| p.follow_dark = v,
        );
        self.switch(
            &desktop,
            "Accent Color",
            "Use the desktop accent for Android’s theme",
            p.follow_accent,
            |p, v| p.follow_accent = v,
        );
        self.switch(
            &desktop,
            "Language and Region",
            "Use the desktop’s languages and regional formats",
            p.follow_locale,
            |p, v| p.follow_locale = v,
        );
        self.switch(
            &desktop,
            "Time Format",
            "Use the desktop’s 12- or 24-hour clock",
            p.follow_clock,
            |p, v| p.follow_clock = v,
        );
        page.add(&desktop);

        page.add(&self.lifecycle_group(&p));

        let apps = adw::PreferencesGroup::builder()
            .title("Android Integration")
            .build();
        self.switch(&apps, "Mirror Apps to the App Menu", "Add launchers for apps installed inside Android. Existing launchers are kept when turned off.", p.mirror_apps, |p,v| p.mirror_apps = v);
        apps.add(&self.adb_row(&p));
        let settings = adw::ActionRow::builder()
            .title("Android Settings…")
            .subtitle("Open Android Settings in its own window")
            .activatable(true)
            .build();
        settings.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
        let w = self.window.upgrade().map(|w| w.downgrade());
        settings.connect_activated(move |_| {
            if let Some(w) = w.as_ref().and_then(|w| w.upgrade()) {
                w.launch("com.android.settings", "Android Settings");
            }
        });
        apps.add(&settings);
        page.add(&apps);
        let privacy = adw::PreferencesGroup::builder()
            .title("App Isolation")
            .description(
                "Android apps are not isolated from each other: they all run as your user, \
                 as desktop apps do.",
            )
            .build();
        let docs = adw::ActionRow::builder()
            .title("Learn More")
            .activatable(true)
            .build();
        docs.add_suffix(&gtk::Image::from_icon_name("adw-external-link-symbolic"));
        let w = self.window.upgrade().map(|w| w.downgrade());
        docs.connect_activated(move |_| {
            gtk::UriLauncher::new(ISOLATION_DOCS).launch(
                w.as_ref()
                    .and_then(|w| w.upgrade())
                    .map(|w| w.win.clone())
                    .as_ref(),
                gtk::gio::Cancellable::NONE,
                |_| {},
            );
        });
        privacy.add(&docs);
        page.add(&privacy);
        self.content.append(&page);
    }

    /// Windowed or a device window, of those the profile's system has; for
    /// the next start.
    fn display_row(self: &Rc<Self>, p: &Settings) -> adw::ComboRow {
        let choices: Vec<_> = DISPLAYS
            .into_iter()
            .filter(|c| p.system.displays().contains(&c.0))
            .collect();
        let model = gtk::StringList::new(&choices.iter().map(|c| c.1).collect::<Vec<_>>());
        let current = choices.iter().position(|c| c.0 == p.display).unwrap_or(0);
        let row = adw::ComboRow::builder()
            .title("Display")
            .subtitle(format!(
                "{} · Applies the next time it starts",
                choices[current].2
            ))
            .model(&model)
            .selected(current as u32)
            .build();
        let s = Rc::downgrade(self);
        row.connect_selected_notify(move |r| {
            if let (Some(s), Some(&(d, _, _))) = (s.upgrade(), choices.get(r.selected() as usize)) {
                s.save(|p| p.display = d);
            }
        });
        row
    }

    fn lifecycle_group(self: &Rc<Self>, p: &Settings) -> adw::PreferencesGroup {
        let keep_running = self.keep_running.get();
        let group = adw::PreferencesGroup::builder().title("When No Android Apps Are Open")
            .description("Foreground services, alarms and apps allowed to run in the background still keep Android available.").build();
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
            .subtitle("Minutes without activity")
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
            .title("Keep Running (Starts at Login)")
            .subtitle("Every profile · Requires desktop permission · about 540 MB each while idle")
            .activatable_widget(&keep)
            .build();
        row.add_prefix(&keep);
        group.add(&row);
        let s = Rc::downgrade(self);
        let spin = minutes.clone();
        keep.connect_toggled(move |r| {
            spin.set_sensitive(!r.is_active());
            if let Some(s) = s.upgrade() {
                let on = r.is_active();
                s.keep_running.set(on);
                s.work(false, move || {
                    preferences::save(Preferences { keep_running: on })
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

    fn adb_row(self: &Rc<Self>, p: &Settings) -> adw::SwitchRow {
        let row = adw::SwitchRow::builder()
            .title("adb")
            .subtitle(if p.adb {
                format!(
                    "On · <tt>{}</tt> in adb and Android Studio",
                    innkeeper_core::adb::serial(p)
                )
            } else {
                "For Android Studio, Gradle and adb on this computer".into()
            })
            .use_markup(true)
            .active(p.adb)
            .build();
        let s = Rc::downgrade(self);
        row.connect_active_notify(move |r| {
            let Some(s) = s.upgrade() else {
                return;
            };
            let (n, on) = (s.profile.clone(), r.is_active());
            // Reloaded after, so the subtitle shows the port it got.
            s.work(true, move || {
                innkeeper_core::adb::set(&n, on, &innkeeper_core::ops::quiet)?;
                match innkeeper_core::adb::missing_key().filter(|_| on) {
                    Some(why) => Err(anyhow::anyhow!("adb is on, but {why}")),
                    None => Ok(()),
                }
            });
        });
        row
    }
}
