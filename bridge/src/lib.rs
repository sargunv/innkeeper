//! Linux.apk desktop bridge: a host-side daemon that Android-side clients in
//! the session reach over a `SOCK_SEQPACKET` socket. Audio is the first
//! service; see `docs/bridge.md` for the protocol.

pub mod audio;
pub mod log;
pub mod proto;
pub mod server;
