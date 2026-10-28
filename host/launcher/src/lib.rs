//! The Linux.apk session launcher (`linuxapk-session`, host/launcher/README.md)
//! and what host/core reads of a session without running it: its paths, its
//! status and its memory. docs/session.md describes the session.

pub mod cli;
mod conf;
mod control;
mod exec;
mod flatpak;
mod host;
mod image;
mod init;
mod layer;
mod nsroot;
pub mod paths;
pub mod requirements;
mod root;
mod selftest;
mod session;
mod sys;
mod uid;

pub use paths::{Paths, Sandbox};
pub use uid::UidMode;

use std::fs;

/// A session's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Android init's pid (in the session's sandbox in Flatpak mode), while
    /// it runs.
    pub pid: Option<i32>,
    /// The running session's uid mode, or the one the next start would use.
    pub uid_mode: UidMode,
}

impl Status {
    /// `linuxapk-session status`'s output.
    pub fn text(&self) -> String {
        let head = match self.pid {
            Some(p) => format!("running: host pid {p}"),
            None => "stopped".into(),
        };
        format!("{head}\nuid-mode={}\n", self.uid_mode.name())
    }
}

/// The value of a `key=value` or `key: value` line.
fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| {
        l.strip_prefix(key)
            .and_then(|v| v.strip_prefix('=').or_else(|| v.strip_prefix(": ")))
    })
}

pub fn status(paths: &Paths) -> Status {
    let detect = || uid::detect(&paths.data, paths.sandbox);
    if paths.sandbox == Sandbox::Flatpak {
        let Some(text) = control::status(&paths.control()) else {
            return Status {
                pid: None,
                uid_mode: detect(),
            };
        };
        return Status {
            pid: field(&text, "running")
                .and_then(|v| v.strip_prefix("host pid "))
                .and_then(|v| v.parse().ok()),
            uid_mode: match field(&text, "uid-mode") {
                Some("subuid") => UidMode::Subuid,
                _ => UidMode::Single,
            },
        };
    }
    match session::ns_pid(paths) {
        Some(pid) => Status {
            pid: Some(pid),
            uid_mode: conf::Config::load(&paths.conf()).map_or_else(|_| detect(), |c| c.uids.mode),
        },
        None => Status {
            pid: None,
            uid_mode: detect(),
        },
    }
}

/// The session's memory use (its cgroup's `memory.current`), in bytes. Host
/// mode only: a Flatpak session has no cgroup of its own.
pub fn memory(paths: &Paths) -> Option<u64> {
    let cg = host::cgroup(paths)?;
    fs::read_to_string(cg.join("memory.current"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The session's cgroup dir (host mode).
pub fn cgroup(paths: &Paths) -> Option<std::path::PathBuf> {
    host::cgroup(paths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_text() {
        let s = Status {
            pid: Some(42),
            uid_mode: UidMode::Single,
        };
        assert_eq!(s.text(), "running: host pid 42\nuid-mode=single\n");
        assert_eq!(field(&s.text(), "running"), Some("host pid 42"));
        assert_eq!(field(&s.text(), "uid-mode"), Some("single"));
        let s = Status {
            pid: None,
            uid_mode: UidMode::Subuid,
        };
        assert_eq!(s.text(), "stopped\nuid-mode=subuid\n");
    }
}
