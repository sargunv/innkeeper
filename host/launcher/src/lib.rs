//! The Linux.apk session launcher (`linuxapk-session`, host/launcher/README.md)
//! and what host/core reads of a session without running it: its paths, its
//! status and its cgroup. docs/session.md describes the session.

pub mod adb;
pub mod cli;
mod conf;
mod control;
mod exec;
mod flatpak;
mod graphics;
mod image;
mod init;
mod layer;
pub mod lifecycle;
pub mod paths;
mod phone;
pub mod preferences;
pub mod profile;
mod prune;
mod root;
mod selftest;
mod session;
mod sys;

pub use paths::Paths;

/// A session's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Android init's pid (in the session's sandbox in Flatpak mode), while
    /// it runs.
    pub pid: Option<i32>,
}

impl Status {
    /// `linuxapk-session status`'s output.
    pub fn text(&self) -> String {
        let head = match self.pid {
            Some(p) => format!("running: host pid {p}"),
            None => "stopped".into(),
        };
        format!("{head}\n")
    }

    /// The state in [`Status::text`]'s output.
    pub fn parse(text: &str) -> Self {
        Status {
            pid: field(text, "running")
                .and_then(|v| v.strip_prefix("host pid "))
                .and_then(|v| v.parse().ok()),
        }
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
    control::status(&paths.control()).map_or(Status { pid: None }, |text| Status::parse(&text))
}
