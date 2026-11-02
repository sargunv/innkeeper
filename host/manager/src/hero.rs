//! The top of the app and install pages: the app's icon, name and version
//! above its buttons. While an operation runs, a spinner with the operation's
//! progress takes the buttons' place.

use adw::prelude::*;

#[derive(Clone)]
pub struct Hero {
    pub widget: adw::PreferencesGroup,
    pub buttons: gtk::Box,
    progress: gtk::Box,
    label: gtk::Label,
}

impl Hero {
    pub fn new(icon: &gtk::Image, title: &str, subtitle: &str) -> Self {
        icon.set_pixel_size(128);
        let head = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(6)
            .halign(gtk::Align::Center)
            .build();
        head.append(icon);
        head.append(
            &gtk::Label::builder()
                .label(title)
                .wrap(true)
                .justify(gtk::Justification::Center)
                .css_classes(["title-1"])
                .margin_top(12)
                .build(),
        );
        head.append(
            &gtk::Label::builder()
                .label(subtitle)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .justify(gtk::Justification::Center)
                .css_classes(["dim-label"])
                .build(),
        );
        let buttons = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .build();
        let label = gtk::Label::new(None);
        let progress = gtk::Box::builder()
            .spacing(12)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .vexpand(true)
            .visible(false)
            .build();
        progress.append(&adw::Spinner::new());
        progress.append(&label);
        // A fixed height keeps the page still when the progress replaces the
        // buttons.
        let actions = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .valign(gtk::Align::Center)
            .height_request(48)
            .margin_top(18)
            .build();
        actions.append(&buttons);
        actions.append(&progress);
        head.append(&actions);
        let widget = adw::PreferencesGroup::new();
        widget.add(&head);
        Self {
            widget,
            buttons,
            progress,
            label,
        }
    }

    /// A pill button in the button row.
    pub fn button(&self, label: &str, suggested: bool) -> gtk::Button {
        let b = gtk::Button::builder()
            .label(label)
            .use_underline(true)
            .css_classes(["pill"])
            .build();
        if suggested {
            b.add_css_class("suggested-action");
        }
        self.buttons.append(&b);
        b
    }

    /// Shows `text` with a spinner in place of the buttons, or the buttons
    /// again with `None`.
    pub fn busy(&self, text: Option<&str>) {
        self.buttons.set_visible(text.is_none());
        self.progress.set_visible(text.is_some());
        self.label.set_text(text.unwrap_or_default());
    }
}
