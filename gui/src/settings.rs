//! The preferences dialog: the APK handler, and placeholders for the
//! desktop-integration settings that arrive with M3.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use linuxapk_core::ops;
use linuxapk_core::session;
use linuxapk_core::xdg::{self, Dirs};

use crate::errors::Op;
use crate::window::Window;
use crate::worker;

fn handler_subtitle() -> &'static str {
    match xdg::handler_registered(&Dirs::from_env()) {
        Some(false) => {
            "APK files now install with the linuxapk command; turn off and on to use this app"
        }
        _ => "Double-click an APK in your file manager to install it here",
    }
}

pub fn present(w: &Window) {
    let dialog = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();

    // Desktop integration.
    let integration = adw::PreferencesGroup::builder()
        .title("Desktop Integration")
        .build();
    let handler = adw::SwitchRow::builder()
        .title("Open APK Files With Linux.apk")
        .subtitle(handler_subtitle())
        .active(xdg::handler_registered(&Dirs::from_env()) == Some(true))
        .build();
    let w2 = w.clone();
    let reverting = Rc::new(Cell::new(false));
    handler.connect_active_notify(move |row| {
        if reverting.get() {
            return;
        }
        let on = row.is_active();
        row.set_sensitive(false);
        let (w, row, reverting) = (w2.clone(), row.clone(), reverting.clone());
        glib::spawn_future_local(async move {
            let r = worker::run_quiet(move || ops::set_gui_handler(on)).await;
            row.set_sensitive(true);
            match r {
                Ok(()) => {
                    _ = w.say(if on {
                        "APK files now open in Linux.apk"
                    } else {
                        "APK files no longer open in Linux.apk"
                    })
                }
                Err(e) => {
                    w.show_error(Op::Settings, None, &e);
                    reverting.set(true);
                    row.set_active(!on);
                    reverting.set(false);
                }
            }
            row.set_subtitle(handler_subtitle());
        });
    });
    integration.add(&handler);
    page.add(&integration);

    // Coming with M3: the system app follows desktop settings.
    let look = adw::PreferencesGroup::builder()
        .title("Appearance")
        .description("Coming soon: Android apps will follow these desktop settings")
        .build();
    for (title, subtitle) in [
        (
            "Follow Dark Style",
            "Apps switch to their dark theme with the desktop",
        ),
        (
            "Use the Desktop Accent Color",
            "Apps with dynamic color pick up the accent color",
        ),
        ("Match Text Size", "Apps use the desktop’s font scaling"),
    ] {
        look.add(
            &adw::SwitchRow::builder()
                .title(title)
                .subtitle(subtitle)
                .sensitive(false)
                .build(),
        );
    }
    page.add(&look);

    // Session.
    let sess = adw::PreferencesGroup::builder().title("Session").build();
    let inst = session::instance().unwrap_or_default();
    sess.add(
        &adw::ActionRow::builder()
            .title("Instance")
            .subtitle(if inst.is_empty() {
                "Default".to_string()
            } else {
                inst
            })
            .subtitle_selectable(true)
            .use_markup(false)
            .css_classes(["property"])
            .build(),
    );
    sess.add(
        &adw::ActionRow::builder()
            .title("Launcher")
            .subtitle(
                session::find_launcher()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|_| "Not found".into()),
            )
            .subtitle_selectable(true)
            .use_markup(false)
            .css_classes(["property"])
            .build(),
    );
    sess.add(
        &adw::SwitchRow::builder()
            .title("Stop When Idle")
            .subtitle("Coming soon: free the memory when no Android app is open")
            .sensitive(false)
            .build(),
    );
    page.add(&sess);

    dialog.add(&page);
    dialog.present(Some(&w.win));
}
