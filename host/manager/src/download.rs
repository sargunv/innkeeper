//! The page that a profile shows until its system's image is downloaded. It
//! offers the download, since a user of one system may never need the other's,
//! and then shows its progress, speed and time left.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use innkeeper_core::launcher::Preparing;
use innkeeper_profile::profile::System;

pub struct Page {
    pub page: adw::StatusPage,
    pub button: gtk::Button,
    bar: gtk::ProgressBar,
    detail: gtk::Label,
    spinner: adw::Spinner,
    choices: gtk::Box,
    system: Cell<System>,
    size: Cell<Option<u64>>,
    speed: RefCell<Speed>,
    /// Whether the profile's image is ready, once the window has read it.
    pub ready: Cell<Option<bool>>,
    /// True while the download runs.
    pub active: Cell<bool>,
    /// Files opened before the image was ready. Their install pages open once
    /// it is.
    pub opened: RefCell<Vec<gio::File>>,
}

impl Page {
    pub fn new() -> Self {
        let spinner = adw::Spinner::builder()
            .width_request(32)
            .height_request(32)
            .visible(false)
            .build();
        let bar = gtk::ProgressBar::builder().visible(false).build();
        let detail = gtk::Label::builder()
            .css_classes(["numeric", "dim-label"])
            .wrap(true)
            .justify(gtk::Justification::Center)
            .visible(false)
            .build();
        let button = gtk::Button::builder()
            .label("_Download")
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "suggested-action"])
            .build();
        // A user of the other system may want another profile instead.
        let profiles = gtk::Button::builder()
            .label("Manage _Profiles")
            .use_underline(true)
            .halign(gtk::Align::Center)
            .css_classes(["pill", "flat"])
            .action_name("win.profiles")
            .build();
        let choices = gtk::Box::new(gtk::Orientation::Vertical, 12);
        choices.append(&button);
        choices.append(&profiles);
        let progress = gtk::Box::new(gtk::Orientation::Vertical, 12);
        progress.append(&bar);
        progress.append(&detail);
        let content = gtk::Box::new(gtk::Orientation::Vertical, 24);
        content.append(&spinner);
        content.append(
            &adw::Clamp::builder()
                .maximum_size(420)
                .child(&progress)
                .build(),
        );
        content.append(&choices);
        let page = adw::StatusPage::builder()
            .icon_name("folder-download-symbolic")
            .child(&content)
            .vexpand(true)
            .build();
        Self {
            page,
            button,
            bar,
            detail,
            spinner,
            choices,
            system: Cell::new(System::Android),
            size: Cell::new(None),
            speed: RefCell::default(),
            ready: Cell::new(None),
            active: Cell::new(false),
            opened: RefCell::default(),
        }
    }

    /// Whether the window shows this page instead of the app list.
    pub fn shown(&self) -> bool {
        self.active.get() || self.ready.get() == Some(false)
    }

    /// Offers to download `system`, `size` bytes if known. It does nothing
    /// while a download runs.
    pub fn offer(&self, system: System, size: Option<u64>) {
        if self.active.get() {
            return;
        }
        self.system.set(system);
        self.size.set(size);
        let s = system.title();
        self.page.set_title(&format!("Download {s}"));
        self.page.set_description(Some(&match size {
            Some(n) => format!(
                "Innkeeper needs a {} download of {s} to run {s} apps. Every {s} profile shares it.",
                glib::format_size(n)
            ),
            None => format!(
                "Innkeeper needs to download {s} to run {s} apps. Every {s} profile shares it."
            ),
        }));
        self.spinner.set_visible(false);
        self.bar.set_visible(false);
        self.detail.set_visible(false);
        self.choices.set_visible(true);
    }

    /// Shows the download as started. Progress shows once the first
    /// [`Preparing`] arrives.
    pub fn start(&self) {
        self.active.set(true);
        *self.speed.borrow_mut() = Speed::default();
        self.page
            .set_title(&format!("Downloading {}", self.system.get().title()));
        self.page.set_description(None);
        self.choices.set_visible(false);
        self.spinner.set_visible(true);
    }

    pub fn update(&self, p: Preparing) {
        match p {
            Preparing::Download { done, total } => {
                let rate = self.speed.borrow_mut().sample(Instant::now(), done);
                self.spinner.set_visible(false);
                self.bar.set_visible(true);
                self.bar.set_fraction(done as f64 / total.max(1) as f64);
                self.detail.set_visible(true);
                self.detail.set_text(&progress_text(done, total, rate));
            }
            Preparing::Unpack => {
                self.page
                    .set_title(&format!("Unpacking {}", self.system.get().title()));
                self.bar.set_visible(false);
                self.detail.set_visible(false);
                self.spinner.set_visible(true);
            }
        }
    }

    /// Shows the offer again after the download failed.
    pub fn failed(&self) {
        self.active.set(false);
        self.offer(self.system.get(), self.size.get());
        self.button.set_label("_Try Again");
    }

    pub fn finished(&self) {
        self.active.set(false);
        self.button.set_label("_Download");
    }
}

/// "312.5 MB of 1.2 GB — 2 minutes left (8.4 MB/s)", with the time and speed
/// only once the speed is known.
fn progress_text(done: u64, total: u64, rate: Option<f64>) -> String {
    let of = format!(
        "{} of {}",
        glib::format_size(done),
        glib::format_size(total)
    );
    match rate {
        Some(r) if r >= 1.0 => {
            let left = Duration::from_secs_f64(total.saturating_sub(done) as f64 / r);
            format!(
                "{of} — {} ({}/s)",
                time_left(left),
                glib::format_size(r as u64)
            )
        }
        _ => of,
    }
}

fn time_left(d: Duration) -> String {
    let plural = |n: u64, unit: &str| {
        if n == 1 {
            format!("1 {unit} left")
        } else {
            format!("{n} {unit}s left")
        }
    };
    match d.as_secs() {
        s if s < 60 => "less than a minute left".into(),
        s if s < 3600 => plural(s.div_ceil(60), "minute"),
        s => plural(s.div_ceil(3600), "hour"),
    }
}

/// The download's speed over the last few seconds.
#[derive(Default)]
struct Speed {
    samples: VecDeque<(Instant, u64)>,
}

impl Speed {
    const WINDOW: Duration = Duration::from_secs(5);

    /// Records that `done` bytes are downloaded at `at`, and returns the speed
    /// in bytes per second once a second of samples is in.
    fn sample(&mut self, at: Instant, done: u64) -> Option<f64> {
        // A restarted download starts over.
        if self.samples.back().is_some_and(|&(_, d)| done < d) {
            self.samples.clear();
        }
        self.samples.push_back((at, done));
        while self
            .samples
            .front()
            .is_some_and(|&(t, _)| at.duration_since(t) > Self::WINDOW)
        {
            self.samples.pop_front();
        }
        let &(t0, d0) = self.samples.front()?;
        let span = at.duration_since(t0);
        (span >= Duration::from_secs(1)).then(|| (done - d0) as f64 / span.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_needs_a_second_and_forgets_old_samples() {
        let t = Instant::now();
        let s = |secs: f64| t + Duration::from_secs_f64(secs);
        let mut speed = Speed::default();
        assert_eq!(speed.sample(s(0.0), 0), None);
        assert_eq!(speed.sample(s(0.5), 500), None);
        assert_eq!(speed.sample(s(1.0), 1000), Some(1000.0));
        // The speed covers only the last five seconds.
        assert_eq!(speed.sample(s(7.0), 1000), None);
        assert_eq!(speed.sample(s(8.0), 3000), Some(2000.0));
        // A restart drops the samples from before it.
        assert_eq!(speed.sample(s(9.0), 0), None);
    }

    #[test]
    fn progress_reads_as_text() {
        const MB: u64 = 1_000_000;
        // GLib joins a size and its unit with a no-break space.
        let progress_text = |d, t, r| progress_text(d, t, r).replace('\u{a0}', " ");
        assert_eq!(progress_text(0, 1200 * MB, None), "0 bytes of 1.2 GB");
        assert_eq!(
            progress_text(300 * MB, 1200 * MB, Some(8.0 * MB as f64)),
            "300.0 MB of 1.2 GB — 2 minutes left (8.0 MB/s)"
        );
        assert_eq!(
            progress_text(1190 * MB, 1200 * MB, Some(MB as f64)),
            "1.2 GB of 1.2 GB — less than a minute left (1.0 MB/s)"
        );
        assert_eq!(
            progress_text(0, 1200 * MB, Some(100_000.0)),
            "0 bytes of 1.2 GB — 4 hours left (100.0 kB/s)"
        );
        assert_eq!(time_left(Duration::from_secs(60)), "1 minute left");
        // A stalled download shows no speed.
        assert_eq!(progress_text(MB, 2 * MB, Some(0.0)), "1.0 MB of 2.0 MB");
    }
}
