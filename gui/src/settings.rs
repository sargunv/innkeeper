//! The preferences dialog: whether APK files open in the manager.

use std::cell::Cell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use linuxapk_core::ops;
use linuxapk_core::xdg::{self, Dirs};

use crate::errors::Op;
use crate::window::Window;
use crate::worker;

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
        .subtitle("Double-click an APK in your file manager to install it here")
        .active(xdg::handler_registered(&Dirs::from_env()))
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
        });
    });
    integration.add(&handler);
    page.add(&integration);

    dialog.add(&page);
    dialog.present(Some(&w.win));
}
