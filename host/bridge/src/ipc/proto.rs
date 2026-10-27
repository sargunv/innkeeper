//! The core protocol: versions, the HELLO handshake and service ids. Each
//! service defines its own message types in its module (see `docs/bridge.md`
//! for the conventions and the type ranges).

use crate::ipc::wire::{DecodeError, Reader, Writer};

/// Protocol major version. A client with another major is refused.
pub const MAJOR: u16 = 1;
/// Protocol minor version (additive changes only).
pub const MINOR: u16 = 0;

/// The first message on every connection.
pub const HELLO: u16 = 0x0001;

/// Services a connection binds to in HELLO (one service per connection).
pub mod service {
    /// Audio output to PipeWire (`audio::proto`, types 0x0100..=0x01ff).
    pub const AUDIO: u32 = 1;
    /// Host windows for the composer (`display::proto`, types
    /// 0x0200..=0x02ff).
    pub const DISPLAY: u32 = 2;
    /// The Linux.apk System app's channel: settings, notifications,
    /// clipboard, links, media (`system`, types 0x0300..=0x03ff,
    /// events 0x4300..=0x43ff).
    pub const SYSTEM: u32 = 3;
    /// Host files the user picks for the system app's document picker and
    /// provider (`documents`, types 0x0400..=0x04ff, events
    /// 0x4400..=0x44ff).
    pub const DOCUMENTS: u32 = 4;
    /// Apps installed inside Android, mirrored to desktop entries
    /// (`apps`, types 0x0500..=0x05ff).
    pub const APPS: u32 = 5;
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

/// Body of a successful HELLO reply: the bridge's version.
pub fn hello_reply_body() -> Vec<u8> {
    let mut w = Writer::default();
    w.u16(MAJOR).u16(MINOR);
    w.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::wire::{HDR, decode_header};

    #[test]
    fn hello_roundtrip() {
        let h = Hello {
            major: 1,
            minor: 0,
            service: service::AUDIO,
            name: "audio HAL".into(),
        };
        let b = h.encode(5);
        assert_eq!(decode_header(&b).unwrap().tag, 5);
        assert_eq!(Hello::decode(&b[HDR..]), Ok(h));
        assert!(Hello::decode(&b[HDR..b.len() - 1]).is_err());
    }
}
