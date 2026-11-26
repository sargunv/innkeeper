//! Camera service messages (id 9, types 0x0901..=0x09ff, events
//! 0x4901..=0x49ff). The client is the camera HAL.
//!
//! | Type     | Request | Body                                     | Reply                                                                      |
//! | -------- | ------- | ---------------------------------------- | -------------------------------------------------------------------------- |
//! | `0x0901` | INFO    |                                          | `u8 present`, `u8 0`, `u16 0`                                              |
//! | `0x0902` | START   | `u16 width`, `u16 height` (preferred), `u32 0` | `u32 slots`, `u32 slot_bytes`, 1 fd: the frame ring; status follows |
//! | `0x0903` | STOP    |                                          |                                                                            |
//!
//! | Event    | Message | Body                                                                     |
//! | -------- | ------- | ------------------------------------------------------------------------ |
//! | `0x4901` | STATUS  | `u8 status` (1 running, 2 denied, 4 unavailable), `u8 0`, `u16 0`         |
//! | `0x4902` | FRAME   | `u32 slot`, `u32 0`, `u64 frame`                                         |
//!
//! `present` is the Camera portal's `IsCameraPresent`, which needs no
//! consent. `START` asks the desktop for the camera the first time and then
//! streams it. The client sends it only when an Android app that holds the
//! `CAMERA` permission opens the camera. A denial (status 2) holds for the
//! bridge's lifetime, so the camera stays off for the session. Status 4 means
//! the stream ended or never started, and the client may start again.
//! `frame::Ring` describes the frame ring. One connection streams at a time,
//! and a second `START` gets `-EBUSY`.

use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const INFO: u16 = 0x0901;
    pub const START: u16 = 0x0902;
    pub const STOP: u16 = 0x0903;
}

pub mod ev {
    pub const STATUS: u16 = 0x4901;
    pub const FRAME: u16 = 0x4902;
}

/// `STATUS` values, as the host state service's `LOCATION_STATUS` uses.
pub mod status {
    pub const RUNNING: u8 = 1;
    pub const DENIED: u8 = 2;
    pub const UNAVAILABLE: u8 = 4;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Info,
    Start { width: u16, height: u16 },
    Stop,
}

pub fn decode(t: u16, body: &[u8]) -> Result<Request, DecodeError> {
    let mut r = Reader::new(body);
    let req = match t {
        ty::INFO => Request::Info,
        ty::START => {
            let width = r.u16()?;
            let height = r.u16()?;
            if r.u32()? != 0 {
                return Err(DecodeError::Invalid("reserved"));
            }
            if width == 0 || height == 0 {
                return Err(DecodeError::Invalid("size"));
            }
            Request::Start { width, height }
        }
        ty::STOP => Request::Stop,
        t => return Err(DecodeError::UnknownType(t)),
    };
    r.end()?;
    Ok(req)
}

pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let w = match *req {
        Request::Info => Writer::message(ty::INFO, tag),
        Request::Start { width, height } => {
            let mut w = Writer::message(ty::START, tag);
            w.u16(width).u16(height).u32(0);
            w
        }
        Request::Stop => Writer::message(ty::STOP, tag),
    };
    w.into_vec()
}

pub fn encode_status(st: u8) -> Vec<u8> {
    vec![st, 0, 0, 0]
}

pub fn encode_frame(slot: u32, frame: u64) -> Vec<u8> {
    let mut w = Writer::default();
    w.u32(slot).u32(0).u64(frame);
    w.into_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::wire::{HDR, decode_header};

    #[test]
    fn requests_round_trip() {
        for r in [
            Request::Info,
            Request::Start {
                width: 640,
                height: 480,
            },
            Request::Stop,
        ] {
            let b = encode(5, &r);
            let h = decode_header(&b).unwrap();
            assert_eq!(decode(h.ty, &b[HDR..]), Ok(r));
        }
    }

    #[test]
    fn malformed_requests() {
        assert!(decode(ty::START, &[0x80, 2, 0xe0, 1, 1, 0, 0, 0]).is_err());
        assert!(decode(ty::START, &[0, 0, 0xe0, 1, 0, 0, 0, 0]).is_err());
        assert!(decode(ty::START, &[0x80, 2, 0xe0, 1]).is_err());
        assert!(decode(ty::INFO, &[0]).is_err());
        assert_eq!(decode(0x0904, &[]), Err(DecodeError::UnknownType(0x0904)));
    }

    #[test]
    fn events() {
        assert_eq!(encode_status(status::DENIED), [2, 0, 0, 0]);
        let f = encode_frame(2, 0x1_0000_0001);
        assert_eq!(f, [2, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0]);
    }
}
