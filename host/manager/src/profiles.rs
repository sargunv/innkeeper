//! The Profiles page lists every profile and lets the user create, rename,
//! delete, start and stop them. Each profile is a separate Android or
//! OpenHarmony setup with its own apps, data and settings. Core calls run off
//! the GTK thread.
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use innkeeper_core::profiles::{self, Profile};
use innkeeper_profile::profile::{Settings, System};

use crate::{
    errors::Op,
    window::{WeakWindow, Window},
    worker,
};

struct Page {
    window: WeakWindow,
    group: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
    /// The profile the window shows, if any.
    current: RefCell<Option<String>>,
    /// The profile this page is starting, stopping, renaming or deleting.
    busy: RefCell<Option<String>>,
    /// The profiles whose rows are expanded.
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
        // Profiles also start and stop from the CLI or when idle, so poll the
        // list.
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
        let shown = self.window.upgrade().map(|w| w.profile().to_string());
        glib::spawn_future_local(async move {
            match worker::run_quiet(move || Ok::<_, anyhow::Error>((profiles::list()?, shown)))
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
        let current = self.current.borrow().as_deref() == Some(p.name.as_str());
        let mut subtitle = if busy == Some(&p.name) {
            "Working…".to_string()
        } else if p.running {
            "Running".into()
        } else {
            "Stopped".into()
        };
        if p.settings.system != System::Android {
            subtitle = format!("{} · {subtitle}", p.settings.system.title());
        }
        if current {
            subtitle += " · Current";
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
            .title("Name")
            .text(&p.name)
            .show_apply_button(true)
            .sensitive(idle && !p.running)
            .build();
        rename.connect_changed(|e| {
            if e.text().is_empty() || innkeeper_profile::profile::check_name(&e.text()).is_ok() {
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
        row
    }

    fn rename(self: &Rc<Self>, from: &str, to: &str) {
        if from == to {
            return;
        }
        if let Err(e) = innkeeper_profile::profile::check_name(to) {
            return self.error(&e);
        }
        let current = self.current.borrow().as_deref() == Some(from);
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

    /// Runs `f` for profile `name` off the GTK thread and reloads the list. If
    /// `f` succeeds, shows `done` as a toast.
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

    /// Like [`Self::work`], but calls `then` on the GTK thread if `f` succeeds.
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
        let body = |system: Option<&System>| match system {
            Some(System::Ohos) => {
                "OpenHarmony profiles can’t install apps. The system can’t be changed later."
            }
            _ => "The system can’t be changed later.",
        };
        let d = adw::AlertDialog::new(Some("New Profile"), Some(body(None)));
        let entry = adw::EntryRow::builder()
            .title("Name")
            .activates_default(true)
            .build();
        let titles: Vec<_> = System::ALL.iter().map(|s| s.title()).collect();
        let system = adw::ComboRow::builder()
            .title("System")
            .model(&gtk::StringList::new(&titles))
            .build();
        let d2 = d.clone();
        system.connect_selected_notify(move |r| {
            d2.set_body(body(System::ALL.get(r.selected() as usize)));
        });
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
            let ok = innkeeper_profile::profile::check_name(&e.text()).is_ok();
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
            let sys = System::ALL[system.selected() as usize];
            s.work(
                &name,
                move || profiles::create(&n, &Settings::new(sys)),
                Some(format!("Profile {name} created")),
            );
        });
    }

    fn confirm_delete(self: &Rc<Self>, name: &str) {
        let d = adw::AlertDialog::new(
            Some(&format!("Delete Profile {name}?")),
            Some("Its apps, data and settings will be permanently deleted."),
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
            let current = s.current.borrow().as_deref() == Some(name.as_str());
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
                    // The window showed the deleted profile. Switch to the first
                    // remaining profile, which `first_profile` creates if none
                    // are left.
                    if current {
                        glib::spawn_future_local(async move {
                            match worker::run_quiet(crate::first_profile).await {
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
