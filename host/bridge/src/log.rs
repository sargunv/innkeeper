//! Minimal stderr logging (the launcher redirects it to a file).

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(v: bool) {
    VERBOSE.store(v, Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
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
        if $crate::log::verbose() {
            eprintln!("innkeeper-bridge: debug: {}", format_args!($($t)*))
        }
    };
}
