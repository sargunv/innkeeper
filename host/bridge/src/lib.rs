//! Linux.apk desktop bridge: a host-side daemon that clients in the Android
//! session reach over a `SOCK_SEQPACKET` socket. Services plug in through
//! `ipc::server::Registry`. See `docs/bridge-protocol.md` and `host/bridge/README.md`.
//!
//! - `ipc`: framing, transport, HELLO, the server and blobs
//! - `audio` (id 1): the audio service (PipeWire output)
//! - `display` (id 2): the composer's displays in host windows
//! - `system` (id 3): desktop settings, notifications, the clipboard, web
//!   links and media sessions, for the Linux.apk System app
//! - `documents` (id 4): host files the user picks in the FileChooser portal
//! - `apps` (id 5): desktop entries for apps installed inside Android
//! - `input` (id 6): window requests, the host seat and input methods, for
//!   the Linux.apk System app
//! - `a11y` (id 7): Android apps' accessibility trees on AT-SPI, for the
//!   Linux.apk System app
//! - `hoststate` (id 8): battery, power saver, network and location, for the
//!   Health HAL and the Linux.apk System app
//! - `log`: stderr logging

pub mod a11y;
pub mod apps;
pub mod audio;
pub mod display;
pub mod documents;
pub mod hoststate;
pub mod input;
pub mod ipc;
pub mod log;
pub mod system;

/// A fresh, empty directory for one test's files:
/// `<repo>/out/test/bridge/<name>`.
#[cfg(test)]
fn test_dir(name: &str) -> std::path::PathBuf {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../out/test/bridge")
        .join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub mod bench;
