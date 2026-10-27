//! The bridge's IPC layer, shared by every service:
//!
//! - `wire`: framing and untrusted-input helpers (every service decodes with
//!   these)
//! - `transport`: the socket, fd passing, peer credentials
//! - `proto`: versions, HELLO, service ids
//! - `server`: accept loop, HELLO, `Service` trait and `Registry`
//! - `blob`: sealed-memfd payloads too big for one message
//! - [`Subscribers`]: event fan-out to the connections that asked for it

pub mod blob;
pub mod proto;
pub mod server;
mod subscribers;
pub mod transport;
pub mod wire;

pub use subscribers::Subscribers;
