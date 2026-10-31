//! Opt-in, bounded benchmark trace. Normal sessions never read frame pixels.
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("INNKEEPER_BENCH").is_ok_and(|v| v == "1"))
}

pub fn now() -> u64 {
    let mut t = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // CLOCK_MONOTONIC is also the Android probe's System.nanoTime clock.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

pub fn trace(message: std::fmt::Arguments<'_>) {
    static COUNT: AtomicUsize = AtomicUsize::new(0);
    if enabled() && COUNT.fetch_add(1, Ordering::Relaxed) < 20000 {
        eprintln!("BENCH {message}");
    }
}

pub fn input(line: &str) {
    if enabled() {
        trace(format_args!("input ns={} {line}", now()));
    }
}
