//! The install page. It shows a package's details and permissions above an
//! Install button, then the install's progress.

use std::path::PathBuf;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, glib};
use innkeeper_core::desktop::MANAGER_ID;
use innkeeper_core::ops::{self, Event, Package};

use crate::errors::{self, Op};
use crate::hero::Hero;
use crate::permissions;
use crate::window::{Window, details_expander};
use crate::worker;

/// How many permissions show before the rest fold into an "N More" row.
const SHOWN_PERMISSIONS: usize = 6;

/// Turns a core status message such as "waiting for Android to boot" into
/// progress text.
pub fn status_text(m: &str) -> String {
    let m = match m
        .strip_prefix("waiting for ")
        .and_then(|s| s.strip_suffix(" to boot"))
    {
        Some(system) => format!("starting {system}"),
        // The page already shows the package and version.
        None if m.starts_with("installing") => "installing".into(),
        None => m.into(),
    };
    let mut c = m.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str() + "…")
        .unwrap_or_default()
}

/// Returns a navigation page that reads `file` off the UI thread and then
/// shows its details.
pub fn page(w: &Window, file: PathBuf) -> adw::NavigationPage {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();
    let loading = adw::StatusPage::builder()
        .paintable(&adw::SpinnerPaintable::new(Some(&stack)))
        .title(&name)
        .build();
    stack.add_named(&loading, Some("loading"));
    let toolbar = adw::ToolbarView::new();
    // The details show the app's name below its icon.
    toolbar.add_top_bar(&adw::HeaderBar::builder().show_title(false).build());
    toolbar.set_content(Some(&stack));
    let page = adw::NavigationPage::builder()
        .title("Install")
        .child(&toolbar)
        .build();

    let (w, page2) = (w.clone(), page.clone());
    glib::spawn_future_local(async move {
        let f = file.clone();
        match worker::run_quiet(move || ops::inspect(&f, &ops::quiet)).await {
            Ok(p) => {
                let details = details(&w, &page2, &toolbar, Arc::new(p));
                stack.add_named(&details, Some("details"));
                stack.set_visible_child_name("details");
            }
            Err(e) => {
                let h = errors::humane(Op::Read, None, &e);
                eprintln!("innkeeper-manager: {}", h.details);
                let err = adw::StatusPage::builder()
                    .icon_name("dialog-error-symbolic")
                    .title(&h.heading)
                    .description(glib::markup_escape_text(
                        h.body.as_deref().unwrap_or_default(),
                    ))
                    .child(
                        &adw::Clamp::builder()
                            .maximum_size(480)
                            .child(&details_expander(&h.details))
                            .build(),
                    )
                    .build();
                stack.add_named(&err, Some("error"));
                stack.set_visible_child_name("error");
            }
        }
    });
    page
}

fn property_row(title: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(title)
        .subtitle(value)
        .use_markup(false)
        .subtitle_selectable(true)
        .css_classes(["property"])
        .build()
}

fn icon_widget(p: &Package) -> gtk::Image {
    let tex = p
        .icon
        .as_ref()
        .and_then(|b| gdk::Texture::from_bytes(&glib::Bytes::from(b)).ok());
    let img = match tex {
        Some(t) => gtk::Image::from_paintable(Some(&t)),
        None => gtk::Image::from_icon_name(MANAGER_ID),
    };
    img.add_css_class("icon-dropshadow");
    img
}

fn details(
    w: &Window,
    page: &adw::NavigationPage,
    toolbar: &adw::ToolbarView,
    p: Arc<Package>,
) -> gtk::Widget {
    page.set_title(&p.label);
    let installed = w.installed(&p.package);

    // The profile installs only apps for the system it runs.
    let other_system = (p.system() != w.system()).then(|| p.system().title());
    if let Some(title) = other_system {
        let b = adw::Banner::new(&format!("Made for {title}"));
        b.set_revealed(true);
        toolbar.add_top_bar(&b);
    } else if !p.runs_here() {
        let b = adw::Banner::new("Not built for this computer");
        b.set_revealed(true);
        toolbar.add_top_bar(&b);
    }

    let hero = Hero::new(
        &icon_widget(&p),
        &p.label,
        &subtitle(&p.package, &p.version),
    );
    let label = match &installed {
        Some(a) if a.version.as_deref() == Some(p.version.as_str()) => "_Reinstall",
        Some(_) => "_Replace",
        None => "_Install",
    };
    let button = hero.button(label, true);
    button.set_sensitive(p.runs_here() && other_system.is_none());

    let info = adw::PreferencesGroup::builder().title("Details").build();
    if !p.version_code.is_empty() {
        info.add(&property_row("Build", &p.version_code));
    }
    if let Some(a) = &installed {
        info.add(&property_row(
            "Installed Version",
            a.version.as_deref().unwrap_or("Unknown"),
        ));
    }
    info.add(&property_row("Size", &glib::format_size(p.size)));
    let abis = if p.abis.is_empty() {
        "Any".to_string()
    } else {
        p.abis.join(", ")
    };
    info.add(&property_row("Architectures", &abis));
    info.add(&property_row(
        "File",
        &p.file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    ));

    let pp = adw::PreferencesPage::new();
    pp.add(&hero.widget);
    pp.add(&info);
    pp.add(&permissions_group(&p));

    let (w, page) = (w.clone(), page.clone());
    button.connect_clicked(move |_| {
        hero.busy(Some("Preparing…"));
        page.set_can_pop(false);
        let (w, page, hero, p) = (w.clone(), page.clone(), hero.clone(), p.clone());
        let profile = w.profile();
        glib::spawn_future_local(async move {
            let p2 = p.clone();
            let h2 = hero.clone();
            let r = worker::run(
                move |rep| ops::install(&profile, &p2, rep),
                move |e| match e {
                    Event::Status(m) => h2.busy(Some(&status_text(&m))),
                    Event::Warning(m) => eprintln!("innkeeper-manager: {m}"),
                },
            )
            .await;
            page.set_can_pop(true);
            w.read_session();
            match r {
                Ok(()) => {
                    w.refresh();
                    if w.nav.visible_page().as_ref() == Some(&page) {
                        w.nav.pop();
                    }
                    let t = adw::Toast::builder()
                        .title(format!("{} installed", p.label))
                        .use_markup(false)
                        .button_label("Open")
                        .timeout(8)
                        .build();
                    let w2 = w.clone();
                    let (pkg, name) = (p.package.clone(), p.label.clone());
                    t.connect_button_clicked(move |_| {
                        let w = w2.clone();
                        let (pkg, name) = (pkg.clone(), name.clone());
                        glib::spawn_future_local(async move {
                            w.launch(&pkg, &name, |_| {}).await;
                        });
                    });
                    w.toast(t);
                }
                Err(e) => {
                    hero.busy(None);
                    w.show_error(Op::Install, Some(&p.label), &e);
                }
            }
        });
    });
    pp.upcast()
}

/// The permissions that the package asks for. Past the first few, the rest
/// fold into an "N More" row.
fn permissions_group(p: &Package) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Permissions")
        .build();
    let shown = permissions::shown(&p.permissions);
    if shown.is_empty() {
        group.set_description(Some("None"));
    }
    let more = (shown.len() > SHOWN_PERMISSIONS + 1).then(|| {
        adw::ExpanderRow::builder()
            .title(format!("{} More", shown.len() - SHOWN_PERMISSIONS))
            .build()
    });
    for (i, s) in shown.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(&s.title)
            .use_markup(false)
            .build();
        if s.sensitive {
            let icon = gtk::Image::from_icon_name("dialog-warning-symbolic");
            icon.add_css_class("warning");
            icon.set_tooltip_text(Some("Personal data"));
            row.add_suffix(&icon);
        }
        match &more {
            Some(m) if i >= SHOWN_PERMISSIONS => m.add_row(&row),
            _ => group.add(&row),
        }
    }
    if let Some(m) = &more {
        group.add(m);
    }
    group
}

/// "com.example.app · 1.2", or the package alone without a version.
pub fn subtitle(package: &str, version: &str) -> String {
    if version.is_empty() {
        package.to_string()
    } else {
        format!("{package} · {version}")
    }
}

#[cfg(test)]
mod tests {
    use super::status_text;

    #[test]
    fn status_reads_as_progress() {
        for (core, shown) in [
            ("downloading Android", "Downloading Android…"),
            ("starting OpenHarmony", "Starting OpenHarmony…"),
            ("waiting for Android to boot", "Starting Android…"),
            ("installing a.b 1.0", "Installing…"),
        ] {
            assert_eq!(status_text(core), shown);
        }
    }
}
