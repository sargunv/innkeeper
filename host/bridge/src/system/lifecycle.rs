//! The report of what keeps the session up, which the launcher reads from
//! `<run-dir>/lifecycle` for its idle shutdown (docs/session.md,
//! "Lifecycle"), and the indicator for apps running in the background.
//!
//! The system app reports its windows, services, wakeup and background apps.
//! The bridge adds the windows it shows itself (DISPLAY_WINDOW), such as
//! OpenHarmony's screen, and writes the report again when one opens or
//! closes.

use std::path::PathBuf;
use std::sync::Mutex;

use super::proto::Lifecycle;
use crate::ipc::server::Events;

/// The newest report and the connection that sent it.
pub(super) struct Report {
    pub client: u64,
    pub events: Events,
    pub lifecycle: Lifecycle,
    /// Background-allowed apps as (package, label).
    pub background: Vec<(String, String)>,
}

pub(super) struct Lifecycles {
    pub path: PathBuf,
    pub state: Mutex<State>,
}

#[derive(Default)]
pub(super) struct State {
    pub report: Option<Report>,
    /// The apps the background indicator names, while it shows, and the
    /// connection that posted it.
    pub indicator: Option<(u64, Vec<String>)>,
}

impl Lifecycles {
    pub fn new(path: PathBuf) -> Lifecycles {
        Lifecycles {
            path,
            state: Mutex::default(),
        }
    }
}

/// The report file's text for `r` with `own` more windows.
pub(super) fn text(r: &Report, own: usize) -> String {
    let l = &r.lifecycle;
    let background: Vec<&str> = r.background.iter().map(|(p, _)| p.as_str()).collect();
    format!(
        "windows={}\nservices={}\nwakeup={}\nbackground={}\n",
        l.windows as usize + own,
        l.services,
        l.wakeup_ms,
        background.join(",")
    )
}

/// The labels for the background indicator, which shows while only
/// background-allowed apps keep the session up.
pub(super) fn indicator(r: &Report, own: usize) -> Option<Vec<String>> {
    let l = &r.lifecycle;
    (l.windows == 0 && own == 0 && l.services == 0 && !r.background.is_empty())
        .then(|| r.background.iter().map(|(_, n)| n.clone()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn report(windows: u32, services: u32, background: &[&str]) -> Report {
        let (server, _) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::empty(),
            None,
        )
        .unwrap();
        Report {
            client: 1,
            events: Events::new(Arc::new(server)),
            lifecycle: Lifecycle {
                windows,
                services,
                wakeup_ms: 0,
                len: 0,
            },
            background: background
                .iter()
                .map(|p| (p.to_string(), format!("{p} label")))
                .collect(),
        }
    }

    #[test]
    fn own_windows_count_as_windows() {
        let r = report(0, 2, &["a.b", "c.d"]);
        assert_eq!(
            text(&r, 0),
            "windows=0\nservices=2\nwakeup=0\nbackground=a.b,c.d\n"
        );
        assert!(text(&r, 1).starts_with("windows=1\n"));
        assert!(text(&report(2, 0, &[]), 1).starts_with("windows=3\n"));
    }

    #[test]
    fn the_indicator_shows_while_only_background_apps_keep_it_up() {
        let r = report(0, 0, &["a.b"]);
        assert_eq!(indicator(&r, 0), Some(vec!["a.b label".to_string()]));
        // An open screen window hides it, as an app window does.
        assert_eq!(indicator(&r, 1), None);
        assert_eq!(indicator(&report(1, 0, &["a.b"]), 0), None);
        assert_eq!(indicator(&report(0, 1, &["a.b"]), 0), None);
        assert_eq!(indicator(&report(0, 0, &[]), 0), None);
    }
}
