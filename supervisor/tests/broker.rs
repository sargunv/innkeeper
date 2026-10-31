//! C ABI for the syscall probes in supervisor/tests. No policy lives here.
use std::os::fd::{FromRawFd, OwnedFd};

use innkeeper_broker::Config;

#[unsafe(no_mangle)]
extern "C" fn innkeeper_filter_install() -> i32 {
    innkeeper_broker::filter::install(Config::from_env().guest).unwrap_or(-1)
}

#[unsafe(no_mangle)]
extern "C" fn innkeeper_broker_start(listener: i32, pins: i32) {
    let pins = (pins >= 0).then(|| unsafe { OwnedFd::from_raw_fd(pins) });
    std::thread::spawn(move || innkeeper_broker::notify::serve(listener, pins, Config::from_env()));
}
