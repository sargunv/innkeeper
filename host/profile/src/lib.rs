//! A profile as both sides see it: the launcher, which runs its session, and
//! host/core, which works with it without running the launcher. Its name,
//! settings and paths, Innkeeper's own preferences, its session's status and
//! activity report, the control socket client, and adb's keys and server
//! protocol. docs/session.md describes profiles and sessions.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result};

pub mod activity;
pub mod adb;
pub mod control;
pub mod paths;
pub mod preferences;
pub mod profile;

pub use paths::Paths;

/// Deletes a tree whose dirs may be read-only.
pub fn remove_tree(dir: &Path) -> Result<()> {
    if dir.symlink_metadata().is_err() {
        return Ok(());
    }
    fn writable(d: &Path) {
        if let Ok(m) = d.symlink_metadata()
            && m.is_dir()
        {
            let _ = fs::set_permissions(d, fs::Permissions::from_mode((m.mode() & 0o7777) | 0o700));
            for e in fs::read_dir(d).into_iter().flatten().flatten() {
                writable(&e.path());
            }
        }
    }
    writable(dir);
    fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))
}

/// A session's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// The guest's init's pid in the session's sandbox, while it runs.
    pub pid: Option<i32>,
}

impl Status {
    /// `innkeeper-session status`'s output.
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
pub fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines().find_map(|l| {
        l.strip_prefix(key)
            .and_then(|v| v.strip_prefix('=').or_else(|| v.strip_prefix(": ")))
    })
}

pub fn status(paths: &Paths) -> Status {
    control::status(&paths.control()).map_or(Status { pid: None }, |text| Status::parse(&text))
}
