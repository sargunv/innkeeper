//! Minimal stderr logging (the launcher redirects it to bridge.log).
//!
//! `INNKEEPER_BRIDGE_DEBUG=1` turns on [`debug!`] lines and the frame and
//! input [`trace`] that tests/bench reads; otherwise the bridge never reads
//! frame pixels for it.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn debug() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("INNKEEPER_BRIDGE_DEBUG").is_ok_and(|v| v == "1"))
}

/// CLOCK_MONOTONIC in ns, also the Android probe's System.nanoTime clock.
pub fn now() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// A `trace: <event> key=value...` line, at most 20000 per process (frames
/// would otherwise fill the log).
pub fn trace(message: std::fmt::Arguments<'_>) {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if debug() && COUNT.fetch_add(1, Ordering::Relaxed) < 20000 {
        eprintln!("innkeeper-bridge: trace: {message}");
    }
}

#[macro_export]
macro_rules! info {
    ($($t:tt)*) => { eprintln!("innkeeper-bridge: {}", format_args!($($t)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)*) => { eprintln!("innkeeper-bridge: warning: {}", format_args!($($t)*)) };
}

#[macro_export]
macro_rules! debug {
    ($($t:tt)*) => {
        if $crate::log::debug() {
            eprintln!("innkeeper-bridge: debug: {}", format_args!($($t)*))
        }
    };
}
