//! Linux.apk desktop bridge: a host-side daemon that clients in the Android
//! session reach over a `SOCK_SEQPACKET` socket. Services plug in through
//! `server::Registry`. See `docs/bridge.md`.
//!
//! - `wire`: framing and untrusted-input helpers (every service decodes with
//!   these)
//! - `transport`: the socket, fd passing, peer credentials
//! - `proto`: versions, HELLO, service ids
//! - `server`: accept loop, HELLO, `Service` trait and `Registry`
//! - `audio`: the audio service (PipeWire output)
//! - `display`: the display service (host windows and input for the composer)
//! - `services`: desktop integration for the Linux.apk System app (the
//!   system, documents and apps services)
//! - `log`: stderr logging

pub mod audio;
pub mod display;
pub mod log;
pub mod proto;
pub mod server;
pub mod services;
pub mod transport;
pub mod wire;
