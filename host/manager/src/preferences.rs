//! Linux.apk preferences. Reads and writes run off the GTK thread.
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use linuxapk_core::preferences::{self, Preferences};

use crate::{
    errors::Op,
    window::{WeakWindow, Window},
    worker,
};

struct Page {
    window: WeakWindow,
    content: gtk::Box,
    prefs: RefCell<Preferences>,
    busy: Cell<bool>,
    addon: Cell<bool>,
    main: Cell<bool>,
}

pub fn page(window: &Window) -> adw::NavigationPage {
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&content));
    let page = adw::NavigationPage::builder()
        .title("Preferences")
        .child(&toolbar)
        .build();
    let state = Rc::new(Page {
        window: window.downgrade(),
        content,
        prefs: RefCell::new(Preferences::default()),
        busy: Cell::new(false),
        addon: Cell::new(false),
        main: Cell::new(false),
    });
    page.connect_showing(move |_| state.load());
    page
}

impl Page {
    fn load(self: &Rc<Self>) {
        let s = self.clone();
        glib::spawn_future_local(async move {
            let result = worker::run_quiet(|| {
                Ok::<_, anyhow::Error>((
                    Preferences::load()?,
                    preferences::microg_available(),
                    linuxapk_core::paths::profile()?.is_empty(),
                ))
            })
            .await;
            match result {
                Ok((p, addon, main)) => {
                    s.prefs.replace(p);
                    s.addon.set(addon);
                    s.main.set(main);
                    s.show();
                }
                Err(e) => {
                    if let Some(w) = s.window.upgrade() {
                        w.show_error(Op::Preferences, None, &e);
                    }
                }
            }
        });
    }

    fn save(self: &Rc<Self>, change: impl FnOnce(&mut Preferences)) {
        if self.busy.replace(true) {
            return;
        }
        let old = self.prefs.borrow().clone();
        change(&mut self.prefs.borrow_mut());
        let next = self.prefs.borrow().clone();
        let restart = next.microg != old.microg;
        self.content.set_sensitive(false);
        let s = self.clone();
        glib::spawn_future_local(async move {
            let result = worker::run_quiet(move || preferences::save(next)).await;
            s.busy.set(false);
            s.content.set_sensitive(true);
            if let Err(e) = result {
                if let Some(w) = s.window.upgrade() {
                    w.show_error(Op::Preferences, None, &e);
                }
                s.load();
            } else if restart && let Some(w) = s.window.upgrade() {
                _ = w.say("microG will change the next time Android starts");
            }
        });
    }

    fn switch(
        self: &Rc<Self>,
        group: &adw::PreferencesGroup,
        title: &str,
        subtitle: &str,
        active: bool,
        set: fn(&mut Preferences, bool),
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
        let p = self.prefs.borrow().clone();
        let page = adw::PreferencesPage::new();
        page.set_vexpand(true);
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

        let lifecycle = adw::PreferencesGroup::builder().title("When No Android Apps Are Open")
            .description("Foreground services, alarms and apps allowed to run in the background still keep Android available.").build();
        let stop = gtk::CheckButton::builder()
            .active(!p.keep_running)
            .valign(gtk::Align::Center)
            .build();
        let keep = gtk::CheckButton::builder()
            .group(&stop)
            .active(p.keep_running)
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
        minutes.set_sensitive(!p.keep_running);
        row.add_suffix(&minutes);
        lifecycle.add(&row);
        let row = adw::ActionRow::builder()
            .title("Keep Running (Starts at Login)")
            .subtitle(if self.main.get() {
                "Requires desktop permission · about 540 MB while idle"
            } else {
                "Start at login is available only for the main session"
            })
            .activatable_widget(&keep)
            .sensitive(self.main.get())
            .build();
        row.add_prefix(&keep);
        lifecycle.add(&row);
        let s = Rc::downgrade(self);
        let spin = minutes.clone();
        keep.connect_toggled(move |r| {
            spin.set_sensitive(!r.is_active());
            if let Some(s) = s.upgrade() {
                s.save(|p| p.keep_running = r.is_active());
            }
        });
        let s = Rc::downgrade(self);
        minutes.connect_value_changed(move |r| {
            if let Some(s) = s.upgrade() {
                s.save(|p| p.idle_minutes = r.value_as_int() as u32);
            }
        });
        page.add(&lifecycle);

        let apps = adw::PreferencesGroup::builder()
            .title("Android Integration")
            .build();
        self.switch(&apps, "Mirror Apps to the App Menu", "Add launchers for apps installed inside Android. Existing launchers are kept when turned off.", p.mirror_apps, |p,v| p.mirror_apps = v);
        let microg = self.switch(
            &apps,
            "microG",
            if self.addon.get() {
                "Use the installed microG addon · Requires an Android restart"
            } else {
                "The optional microG addon is not installed"
            },
            p.microg && self.addon.get(),
            |p, v| p.microg = v,
        );
        microg.set_sensitive(self.addon.get());
        let caveat = adw::ActionRow::builder().title("microG Compatibility")
            .subtitle("Google Play Services client libraries remain unsupported, including Firebase push, Google sign-in, Maps API and licensing. Linux.apk does not support signature spoofing.").build();
        apps.add(&caveat);
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
            .description(linuxapk_core::ops::SINGLE_UID_NOTICE)
            .build();
        page.add(&privacy);
        self.content.append(&page);
    }
}
