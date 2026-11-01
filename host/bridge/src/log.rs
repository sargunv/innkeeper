//! Minimal stderr logging. The launcher redirects stderr to bridge.log.
//!
//! `INNKEEPER_BRIDGE_DEBUG=1` turns on [`debug!`] lines and the frame and
//! input [`trace`] that tests/bench reads. Without it, the bridge does not
//! read frame pixels for tracing.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn debug() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| std::env::var("INNKEEPER_BRIDGE_DEBUG").is_ok_and(|v| v == "1"))
}

/// CLOCK_MONOTONIC in ns. The Android probe's System.nanoTime uses the same
/// clock.
pub fn now() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Logs a `trace: <event> key=value...` line. Each process logs at most
/// 20000 of them so that frame traces do not fill the log.
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
