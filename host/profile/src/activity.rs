//! The system app's last report of a session's activity, which decides
//! whether the session stays up. The bridge writes it to `<run>/lifecycle`.

use std::fs;

use crate::Paths;

/// The system app's last report, read from the `key=value` lines of
/// `<run>/lifecycle`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// App windows, shown or starting.
    pub windows: u32,
    /// Running foreground services.
    pub services: u32,
    /// Installed apps the user lets run in the background.
    pub background: Vec<String>,
    /// The next alarm clock or exact wakeup alarm, in ms since the epoch.
    pub wakeup_ms: Option<u64>,
}

impl Activity {
    pub fn parse(text: &str) -> Activity {
        let mut a = Activity::default();
        for l in text.lines() {
            let Some((k, v)) = l.split_once('=') else {
                continue;
            };
            match k {
                "windows" => a.windows = v.parse().unwrap_or(0),
                "services" => a.services = v.parse().unwrap_or(0),
                "background" => {
                    a.background = v
                        .split(',')
                        .filter(|p| !p.is_empty())
                        .map(str::to_string)
                        .collect()
                }
                "wakeup" => a.wakeup_ms = v.parse().ok().filter(|&t| t > 0),
                _ => {}
            }
        }
        a
    }

    pub fn read(paths: &Paths) -> Option<Activity> {
        fs::read_to_string(paths.lifecycle())
            .ok()
            .map(|t| Activity::parse(&t))
    }
}
