//! The core protocol, covering versions, the HELLO handshake and service ids.
//! Each service defines its own message types in its module.
//! `docs/bridge-protocol.md` describes framing, type ranges and the
//! conventions for a new service.

use crate::ipc::wire::{DecodeError, Reader, Writer};

/// Protocol major version. A client with another major is refused.
pub const MAJOR: u16 = 1;
/// Protocol minor version. Minor version changes are additive.
pub const MINOR: u16 = 0;

/// The first message on every connection.
pub const HELLO: u16 = 0x0001;

/// Services a connection can bind to in HELLO. Each connection binds one
/// service.
pub mod service {
    /// Audio output to PipeWire (`audio::proto`, types 0x0100..=0x01ff).
    pub const AUDIO: u32 = 1;
    /// The composer's displays in host windows (`display::proto`, types
    /// 0x0200..=0x02ff, events 0x4200..=0x42ff).
    pub const DISPLAY: u32 = 2;
    /// The Innkeeper System app's channel for settings, notifications,
    /// clipboard, links and media (`system`, types 0x0300..=0x03ff,
    /// events 0x4300..=0x43ff).
    pub const SYSTEM: u32 = 3;
    /// Host files the user picks for the system app's document picker and
    /// provider (`documents`, types 0x0400..=0x04ff, events
    /// 0x4400..=0x44ff).
    pub const DOCUMENTS: u32 = 4;
    /// Apps installed inside Android, mirrored to desktop entries
    /// (`apps`, types 0x0500..=0x05ff).
    pub const APPS: u32 = 5;
    /// The system app's window requests, the host seat and input methods
    /// (`input::proto`, types 0x0600..=0x06ff, events 0x4600..=0x46ff).
    pub const INPUT: u32 = 6;
    /// Android apps' accessibility trees on AT-SPI, from the system app
    /// (`a11y::proto`, types 0x0700..=0x07ff, events 0x4700..=0x47ff).
    pub const A11Y: u32 = 7;
    /// The host's battery, power saver, network and location, for the
    /// Health HAL and the system app (`hoststate::proto`, types
    /// 0x0800..=0x08ff, events 0x4800..=0x48ff).
    pub const HOSTSTATE: u32 = 8;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub major: u16,
    pub minor: u16,
    pub service: u32,
    /// Free-form client name, for logs.
    pub name: String,
}

impl Hello {
    pub fn decode(body: &[u8]) -> Result<Hello, DecodeError> {
        let mut r = Reader::new(body);
        let h = Hello {
            major: r.u16()?,
            minor: r.u16()?,
            service: r.u32()?,
            name: r.name()?,
        };
        r.end()?;
        Ok(h)
    }

    pub fn encode(&self, tag: u32) -> Vec<u8> {
        let mut w = Writer::message(HELLO, tag);
        w.u16(self.major)
            .u16(self.minor)
            .u32(self.service)
            .name(&self.name);
        w.into_vec()
    }
}

/// Body of a successful HELLO reply, which carries the bridge's version.
pub fn hello_reply_body() -> Vec<u8> {
    let mut w = Writer::default();
    w.u16(MAJOR).u16(MINOR);
    w.into_vec()
}
