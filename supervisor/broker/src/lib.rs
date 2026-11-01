//! innkeeper-broker serves the session's seccomp user notifications
//! (supervisor/README.md).
//!
//!   * [`filter`] is the seccomp filter, installed on Android's init.
//!   * [`notify`] is the serve loop, which dispatches every notification.
//!   * [`binder`] is the driver's state machine (`drivers/android/binder.c`)
//!     and its UAPI. It makes no syscalls. Process memory, receive buffers,
//!     fds and answering the caller go through the
//!     [`binder::driver::Userspace`] trait, so it is unit tested on the host
//!     against an in-memory fake.
//!   * `creds` holds virtual credentials for Android's uids.
//!   * `fs` does what the Flatpak sandbox's missing mounts would do.
//!   * `bpf` holds eBPF objects and the rootless calls netd needs.
//!   * [`input`] holds the session-local evdev devices that the bridge feeds.
//!   * [`ohos`] and [`ashmem`] emulate OpenHarmony's kernel extensions.
//!
//! The environment is read once, into [`Config`].

pub mod ashmem;
pub mod binder;
mod bpf;
mod creds;
pub mod filter;
mod fs;
pub mod input;
pub mod notify;
pub mod ohos;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// The guest userspace the session runs. Android's behaviour is the default.
/// OpenHarmony (`ohos/`) adds its kernel extensions on top of the same
/// emulation and changes nothing for Android.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guest {
    Android,
    Ohos,
}

/// The session as the launcher describes it to the broker.
#[derive(Clone, Debug)]
pub struct Config {
    /// `INNKEEPER_GUEST`: `android` or `ohos`.
    pub guest: Guest,
    /// `INNKEEPER_ROOT_MAP`, lines of `<host dir>=<guest path>`. These are the
    /// host dirs behind the guest's paths, longest host dir first.
    pub root_map: Vec<(Vec<u8>, Vec<u8>)>,
    /// `INNKEEPER_RENDER_NODE`, the DRM node the guest may open. It is unset
    /// or empty for none, which means software rendering.
    pub render_node: Option<PathBuf>,
    /// Init's pid, which SO_PEERCRED shows as 1.
    pub init: i32,
}

impl Config {
    pub fn from_env() -> Config {
        let var = |k| std::env::var_os(k).filter(|v| !v.is_empty());
        let guest = match std::env::var("INNKEEPER_GUEST")
            .unwrap_or_default()
            .as_str()
        {
            "android" => Guest::Android,
            "ohos" => Guest::Ohos,
            v => panic!("INNKEEPER_GUEST must be android or ohos, not {v:?}"),
        };
        let map = std::env::var("INNKEEPER_ROOT_MAP").unwrap_or_default();
        // Tracing is off when INNKEEPER_BROKER_DEBUG is unset, empty or "0".
        let debug = var("INNKEEPER_BROKER_DEBUG").is_some_and(|v| v != "0");
        DEBUG.store(debug, Ordering::Relaxed);
        Config {
            guest,
            root_map: parse_root_map(&map),
            render_node: var("INNKEEPER_RENDER_NODE").map(PathBuf::from),
            init: 0,
        }
    }
}

pub(crate) fn parse_root_map(spec: &str) -> Vec<(Vec<u8>, Vec<u8>)> {
    let mut map: Vec<(Vec<u8>, Vec<u8>)> = spec
        .lines()
        .filter_map(|l| l.rsplit_once('='))
        .map(|(h, a)| {
            (
                h.trim_end_matches('/').as_bytes().to_vec(),
                a.as_bytes().to_vec(),
            )
        })
        .filter(|(h, _)| !h.is_empty())
        .collect();
    map.sort_by_key(|(h, _)| std::cmp::Reverse(h.len()));
    map
}

/// Tracing (`INNKEEPER_BROKER_DEBUG`), set by [`Config::from_env`].
static DEBUG: AtomicBool = AtomicBool::new(false);

pub(crate) fn debug() -> bool {
    DEBUG.load(Ordering::Relaxed)
}

/// Writes a trace line to stderr, which is the session log, when tracing is
/// on.
macro_rules! trace {
    ($($a:tt)*) => {
        if $crate::debug() {
            eprintln!("broker: {}", format_args!($($a)*));
        }
    };
}
pub(crate) use trace;
