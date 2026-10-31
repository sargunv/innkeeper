//! `innkeeper-manager`: the Innkeeper manager. Install Android apps by drag and
//! drop or from the file manager, open and remove them, and watch the
//! session. A GTK 4 / libadwaita front end over `innkeeper-core`; see
//! host/manager/README.md.

mod app;
mod errors;
mod install;
mod permissions;
mod preferences;
mod profiles;
mod window;
mod worker;

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use innkeeper_core::desktop::MANAGER_ID;

use innkeeper_core::paths::ProfileName;

use crate::window::Window;

/// Where the manager remembers the profile it last showed.
fn remembered_file() -> std::path::PathBuf {
    innkeeper_core::paths::xdg_home("XDG_STATE_HOME", ".local/state")
        .join("innkeeper/manager-profile")
}

/// Remembers `name` as the profile to show next time.
pub fn remember_profile(name: &ProfileName) {
    let f = remembered_file();
    let _ = std::fs::create_dir_all(f.parent().unwrap());
    let _ = std::fs::write(f, format!("{name}\n"));
}

/// The first profile by name, made if there is none.
pub fn first_profile() -> anyhow::Result<String> {
    match innkeeper_core::profiles::list()?.into_iter().next() {
        Some(p) => Ok(p.name),
        None => {
            innkeeper_core::profiles::existing(innkeeper_core::profiles::FIRST).map(String::from)
        }
    }
}

/// The profile to show: `named` (`--profile`), else the one shown last if it
/// still exists, else the first.
fn initial_profile(named: Option<&str>) -> anyhow::Result<ProfileName> {
    if let Some(n) = named {
        return innkeeper_core::profiles::existing(n);
    }
    let last = std::fs::read_to_string(remembered_file()).unwrap_or_default();
    match innkeeper_core::profiles::existing(last.trim()) {
        Ok(p) => Ok(p),
        Err(_) => ProfileName::new(&first_profile()?),
    }
}

/// `--profile NAME` taken out of the arguments GApplication parses.
fn split_profile(mut args: Vec<String>) -> (Option<String>, Vec<String>) {
    match args.iter().position(|a| a == "--profile") {
        Some(i) if i + 1 < args.len() => {
            let name = args.remove(i + 1);
            args.remove(i);
            (Some(name), args)
        }
        _ => (None, args),
    }
}

const CSS: &str = "
.status-dot { min-width: 10px; min-height: 10px; border-radius: 5px;
  background-color: alpha(currentColor, 0.25); }
.status-dot.running { background-color: var(--success-color); }
.status-dot.starting { background-color: var(--warning-color); }
.drop-hint { margin: 12px; border-radius: 18px;
  border: 2px dashed var(--accent-color);
  background-color: color-mix(in srgb, var(--accent-bg-color) 12%, var(--window-bg-color) 88%); }
";

fn main() -> glib::ExitCode {
    let (named, args) = split_profile(std::env::args().collect());
    let profile = match initial_profile(named.as_deref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("innkeeper-manager: {e:#}");
            return glib::ExitCode::FAILURE;
        }
    };
    let app = adw::Application::builder()
        .application_id(MANAGER_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN)
        .build();
    let window: Rc<RefCell<Option<Window>>> = Rc::default();

    app.connect_startup(|app| {
        let display = gtk::gdk::Display::default().expect("no display");
        gtk::Window::set_default_icon_name(MANAGER_ID);
        let css = gtk::CssProvider::new();
        css.load_from_string(CSS);
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        app.set_accels_for_action("win.preferences", &["<Control>comma"]);
        app.set_accels_for_action("win.open", &["<Control>o"]);
        app.set_accels_for_action("app.quit", &["<Control>q"]);
        app.set_accels_for_action("window.close", &["<Control>w"]);
        let quit = gio::SimpleAction::new("quit", None);
        let a = app.clone();
        quit.connect_activate(move |_, _| a.quit());
        app.add_action(&quit);
    });

    let get = {
        let window = window.clone();
        move |app: &adw::Application| -> Window {
            let mut slot = window.borrow_mut();
            if let Some(w) = slot.as_ref()
                && w.win.application().is_some()
            {
                return w.clone();
            }
            let w = Window::new(app, profile.clone());
            let about = gio::SimpleAction::new("about", None);
            let w2 = w.clone();
            about.connect_activate(move |_, _| about_dialog(&w2));
            app.add_action(&about);
            *slot = Some(w.clone());
            w
        }
    };

    let g = get.clone();
    app.connect_activate(move |app| g(app).present());
    app.connect_open(move |app, files, _| {
        let w = get(app);
        w.present();
        for f in files {
            w.open_file(f);
        }
    });
    app.run_with_args(&args)
}

fn about_dialog(w: &Window) {
    adw::AboutDialog::builder()
        .application_name("Innkeeper")
        .application_icon(MANAGER_ID)
        .developer_name("Innkeeper contributors")
        .version(env!("CARGO_PKG_VERSION"))
        .comments("Run Android apps on Linux")
        .website("https://github.com/sargunv/innkeeper")
        .issue_url("https://github.com/sargunv/innkeeper/issues")
        .license_type(gtk::License::Agpl30)
        .build()
        .present(Some(&w.win));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_comes_out_of_the_arguments() {
        let v = |s: &str| s.split_whitespace().map(String::from).collect::<Vec<_>>();
        assert_eq!(
            split_profile(v("m --profile work a.apk")),
            (Some("work".into()), v("m a.apk"))
        );
        assert_eq!(split_profile(v("m a.apk")), (None, v("m a.apk")));
        assert_eq!(split_profile(v("m --profile")), (None, v("m --profile")));
    }
}
