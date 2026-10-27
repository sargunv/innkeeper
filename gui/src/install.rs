//! The install confirmation page: what the APK is and asks for, then the
//! install itself with its progress.

use std::path::PathBuf;
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gdk, glib};
use linuxapk_core::desktop::MANAGER_ID;
use linuxapk_core::ops::{self, Event, Package};

use crate::errors::{self, Op};
use crate::permissions;
use crate::window::{Window, details_expander};
use crate::worker;

/// Permissions listed before the rest fold into "N more".
const SHOWN_PERMISSIONS: usize = 6;

/// Progress text for a core status message.
pub fn status_text(m: &str) -> String {
    if m.starts_with("starting the Android session") {
        "Starting Android…".into()
    } else if m.starts_with("waiting for Android to boot") {
        "Waiting for Android to start…".into()
    } else if m.starts_with("installing") {
        "Installing…".into()
    } else if m.starts_with("adding it to the app menu") {
        "Adding to the app menu…".into()
    } else {
        let mut c = m.chars();
        c.next()
            .map(|f| f.to_uppercase().collect::<String>() + c.as_str() + "…")
            .unwrap_or_default()
    }
}

/// A navigation page that reads `file` (off the UI thread) and then shows
/// the confirmation.
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
        .title("Reading the App…")
        .description(glib::markup_escape_text(&name))
        .build();
    stack.add_named(&loading, Some("loading"));
    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&adw::HeaderBar::new());
    toolbar.set_content(Some(&stack));
    let page = adw::NavigationPage::builder()
        .title("Install App")
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
                eprintln!("linuxapk-gui: {}", h.details);
                let err = adw::StatusPage::builder()
                    .icon_name("dialog-error-symbolic")
                    .title(&h.heading)
                    .description(glib::markup_escape_text(&h.body))
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
    img.set_pixel_size(128);
    img.add_css_class("icon-dropshadow");
    img
}

fn details(
    w: &Window,
    page: &adw::NavigationPage,
    toolbar: &adw::ToolbarView,
    p: Arc<Package>,
) -> gtk::Widget {
    page.set_title(&format!("Install {}", p.label));
    let installed = w.installed(&p.package);

    if !p.runs_here() {
        let b = adw::Banner::new(&format!(
            "Built only for {} processors, which this computer can’t run",
            p.abis.join(", ")
        ));
        b.set_revealed(true);
        toolbar.add_top_bar(&b);
    }

    // Header: icon, name, package, the button.
    let head = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .spacing(6)
        .halign(gtk::Align::Center)
        .build();
    head.append(&icon_widget(&p));
    head.append(
        &gtk::Label::builder()
            .label(&p.label)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["title-1"])
            .margin_top(12)
            .build(),
    );
    let sub = if p.version.is_empty() {
        p.package.clone()
    } else {
        format!("{} · {}", p.package, p.version)
    };
    head.append(
        &gtk::Label::builder()
            .label(&sub)
            .wrap(true)
            .justify(gtk::Justification::Center)
            .css_classes(["dim-label"])
            .build(),
    );
    let label = match &installed {
        Some(a) if a.version.as_deref() == Some(p.version.as_str()) => "_Reinstall",
        Some(_) => "_Replace",
        None => "_Install",
    };
    let button = gtk::Button::builder()
        .label(label)
        .use_underline(true)
        .halign(gtk::Align::Center)
        .css_classes(["pill", "suggested-action"])
        .sensitive(p.runs_here())
        .build();
    let progress_label = gtk::Label::new(Some("Preparing…"));
    let progress = gtk::Box::builder()
        .spacing(12)
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .vexpand(true)
        .visible(false)
        .build();
    progress.append(&adw::Spinner::new());
    progress.append(&progress_label);
    // Fixed height, so the page doesn't shift when the button turns into
    // progress.
    let actions = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .valign(gtk::Align::Center)
        .height_request(48)
        .margin_top(18)
        .build();
    actions.append(&button);
    actions.append(&progress);
    head.append(&actions);
    let head_group = adw::PreferencesGroup::new();
    head_group.add(&head);

    // Details.
    let info = adw::PreferencesGroup::builder().title("Details").build();
    let mut version = if p.version.is_empty() {
        "Unknown".to_string()
    } else {
        p.version.clone()
    };
    if !p.version_code.is_empty() {
        version.push_str(&format!(" (build {})", p.version_code));
    }
    info.add(&property_row("Version", &version));
    if let Some(a) = &installed {
        info.add(&property_row(
            "Installed Version",
            a.version.as_deref().unwrap_or("Unknown"),
        ));
    }
    info.add(&property_row("Package", &p.package));
    let mut size = glib::format_size(p.size).to_string();
    if p.apk_count > 1 {
        size.push_str(&format!(" · bundle of {} APKs", p.apk_count));
    }
    info.add(&property_row("Size", &size));
    let abis = if p.abis.is_empty() {
        "Any (no native code)".to_string()
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

    // Permissions.
    let perms = adw::PreferencesGroup::builder()
        .title("Permissions")
        .description("Android asks before an app uses personal data such as the camera")
        .build();
    let shown = permissions::shown(&p.permissions);
    if shown.is_empty() {
        perms.add(
            &adw::ActionRow::builder()
                .title("No special permissions")
                .build(),
        );
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
            icon.set_tooltip_text(Some("Personal data: Android asks you first"));
            row.add_suffix(&icon);
        }
        match &more {
            Some(m) if i >= SHOWN_PERMISSIONS => m.add_row(&row),
            _ => perms.add(&row),
        }
    }
    if let Some(m) = &more {
        perms.add(m);
    }

    let pp = adw::PreferencesPage::new();
    pp.add(&head_group);
    pp.add(&info);
    pp.add(&perms);

    let (w, page) = (w.clone(), page.clone());
    button.connect_clicked(move |b| {
        b.set_visible(false);
        progress.set_visible(true);
        progress_label.set_text("Preparing…");
        page.set_can_pop(false);
        let (w, page, b, progress, pl, p) = (
            w.clone(),
            page.clone(),
            b.clone(),
            progress.clone(),
            progress_label.clone(),
            p.clone(),
        );
        glib::spawn_future_local(async move {
            let p2 = p.clone();
            let pl2 = pl.clone();
            let r = worker::run(
                move |rep| ops::install(&p2, rep),
                move |e| match e {
                    Event::Status(m) => pl2.set_text(&status_text(&m)),
                    Event::Warning(m) => eprintln!("linuxapk-gui: {m}"),
                },
            )
            .await;
            page.set_can_pop(true);
            w.poll_session();
            match r {
                Ok(i) => {
                    w.refresh();
                    if w.nav.visible_page().as_ref() == Some(&page) {
                        w.nav.pop();
                    }
                    let t = adw::Toast::builder()
                        .title(format!("{} installed", i.name))
                        .use_markup(false)
                        .button_label("Open")
                        .timeout(8)
                        .build();
                    let w2 = w.clone();
                    t.connect_button_clicked(move |_| w2.launch(&i.package, &i.name));
                    w.toast(t);
                }
                Err(e) => {
                    progress.set_visible(false);
                    b.set_visible(true);
                    w.show_error(Op::Install, Some(&p.label), &e);
                }
            }
        });
    });
    pp.upcast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_texts() {
        assert_eq!(
            status_text("starting the Android session"),
            "Starting Android…"
        );
        assert_eq!(status_text("installing org.x 1.0 (1 APK)"), "Installing…");
        assert_eq!(status_text("something else"), "Something else…");
    }
}
