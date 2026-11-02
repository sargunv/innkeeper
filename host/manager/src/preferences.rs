//! The Profile Settings page. It shows the current profile's settings. An Android
//! profile has an adb switch, and an OpenHarmony profile has an hdc switch.
//! Reads and writes run off the GTK thread.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use innkeeper_core::profiles;
use innkeeper_profile::profile::{Display, Settings, System};

use crate::{
    errors::Op,
    window::{WeakWindow, Window},
    worker,
};

struct Page {
    window: WeakWindow,
    content: gtk::Box,
    /// The profile whose settings the page shows.
    profile: String,
    settings: RefCell<Settings>,
    busy: Cell<bool>,
}

/// The display choices. A device window of a fixed size lists its size.
const DISPLAYS: [(Display, &str, &str); 4] = [
    (Display::Windowed, "Windowed", ""),
    (Display::Device, "Device", "Resizable"),
    (Display::SmallPhone, "Small Phone", "720 × 1280"),
    (Display::MediumPhone, "Medium Phone", "1080 × 2400"),
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
            match worker::run_quiet(move || profiles::settings(&name)).await {
                Ok(settings) => {
                    s.settings.replace(settings);
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
        subtitle: &str,
        active: bool,
        set: fn(&mut Settings, bool),
    ) {
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

        let desktop = adw::PreferencesGroup::builder()
            .title("Follow the Desktop")
            .build();
        self.switch(&desktop, "Dark Mode", "", p.follow_dark, |p, v| {
            p.follow_dark = v
        });
        // OpenHarmony has no accent color.
        if p.system != System::Ohos {
            self.switch(&desktop, "Accent Color", "", p.follow_accent, |p, v| {
                p.follow_accent = v
            });
        }
        self.switch(
            &desktop,
            "Language and Region",
            "",
            p.follow_locale,
            |p, v| p.follow_locale = v,
        );
        self.switch(&desktop, "Time Format", "", p.follow_clock, |p, v| {
            p.follow_clock = v
        });
        page.add(&desktop);
        page.add(&self.lifecycle_group(&p));
        if p.system == System::Ohos {
            let apps = adw::PreferencesGroup::builder()
                .title("OpenHarmony Integration")
                .build();
            apps.add(&self.debug_row(&p));
            page.add(&apps);
            self.content.append(&page);
            return;
        }

        let apps = adw::PreferencesGroup::builder()
            .title("Android Integration")
            .build();
        apps.add(&self.debug_row(&p));
        let settings = adw::ActionRow::builder()
            .title("Android Settings")
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
        page.add(
            &adw::PreferencesGroup::builder()
                .description("Like desktop apps, Android apps aren’t isolated from each other.")
                .build(),
        );
        self.content.append(&page);
    }

    /// The Display row. It lists the display modes the profile's system
    /// supports, and a change applies at the next start.
    fn display_row(self: &Rc<Self>, p: &Settings) -> adw::ComboRow {
        let choices: Vec<_> = DISPLAYS
            .into_iter()
            .filter(|c| p.system.displays().contains(&c.0))
            .collect();
        let model = gtk::StringList::new(&choices.iter().map(|c| c.1).collect::<Vec<_>>());
        let current = choices.iter().position(|c| c.0 == p.display).unwrap_or(0);
        let row = adw::ComboRow::builder()
            .title("Display")
            .subtitle(match choices[current].2 {
                "" => "Applies at the next start".to_string(),
                size => format!("{size} · Applies at the next start"),
            })
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
                let (name, on) = (s.profile.clone(), r.is_active());
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
        let (title, on, connect) = if ohos {
            ("hdc", p.hdc, innkeeper_core::hdc::connect_command(p))
        } else {
            ("adb", p.adb, innkeeper_core::adb::serial(p))
        };
        let row = adw::SwitchRow::builder()
            .title(title)
            .subtitle(if on {
                format!("<tt>{}</tt>", glib::markup_escape_text(&connect))
            } else {
                String::new()
            })
            .use_markup(true)
            .active(on)
            .build();
        let s = Rc::downgrade(self);
        row.connect_active_notify(move |r| {
            let Some(s) = s.upgrade() else {
                return;
            };
            let (n, on) = (s.profile.clone(), r.is_active());
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
}
