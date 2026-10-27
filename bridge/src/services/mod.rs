//! Desktop-integration services. Today there is one: the system service
//! (id 3), the channel to the Linux.apk System app. Its features each live in
//! their own module and talk to the desktop over D-Bus or Wayland:
//!
//! - `settings`: portal Settings (dark mode, accent, contrast, clock format,
//!   text scale), the time zone and the locale
//! - `notify`: `org.freedesktop.Notifications`
//! - `clipboard`: the Wayland clipboard through `ext-data-control-v1`
//! - `openuri`: the OpenURI portal (http and https only)
//! - `mpris`: MediaSessions as MPRIS players
//!
//! See `docs/bridge.md`, "System service".

pub mod blob;
pub mod clipboard;
pub mod mpris;
pub mod notify;
pub mod openuri;
pub mod settings;
pub mod system;

use std::os::fd::BorrowedFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::server::Events;

/// Connections that want one kind of event. Sends that fail (the client is
/// gone) drop the subscriber.
#[derive(Default)]
pub struct Subscribers {
    next: AtomicU64,
    list: Mutex<Vec<(u64, Events)>>,
}

impl Subscribers {
    pub fn add(&self, e: Events) -> u64 {
        let k = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.list.lock().unwrap().push((k, e));
        k
    }

    pub fn remove(&self, k: u64) {
        self.list.lock().unwrap().retain(|(i, _)| *i != k);
    }

    pub fn is_empty(&self) -> bool {
        self.list.lock().unwrap().is_empty()
    }

    /// Sends to everyone except `skip` (the connection that caused it).
    pub fn broadcast(&self, ty: u16, body: &[u8], fds: &[BorrowedFd<'_>], skip: Option<u64>) {
        self.list
            .lock()
            .unwrap()
            .retain(|(k, e)| Some(*k) == skip || e.send(ty, body, fds).is_ok());
    }
}

/// Truncates `s` to at most `max` bytes on a char boundary.
pub fn truncate(s: &str, max: usize) -> &str {
    let mut n = s.len().min(max);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

/// An Android package name: `[A-Za-z0-9_.]`, 1 to 255 bytes. These end up in
/// D-Bus names and file names, so nothing else gets through.
pub fn valid_package(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 255
        && !p.starts_with('.')
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}
