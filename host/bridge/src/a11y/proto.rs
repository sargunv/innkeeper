//! Accessibility service messages (id 7, types 0x0700..=0x07ff, events
//! 0x4700..=0x47ff). The client is the Linux.apk System app's accessibility
//! service; only the system app's uid may open the service, since its trees
//! are the screen contents of every app and its actions click for the user.
//!
//! | Type     | Request | Body (fd)                                                     | Reply                                                                  |
//! | -------- | ------- | ------------------------------------------------------------- | ---------------------------------------------------------------------- |
//! | `0x0701` | START   |                                                               | `u8 active`: an assistive technology is on (ACTIVE follows as events)  |
//! | `0x0702` | TREE    | `u32 window`, `u32 len` ≤ 16 MiB (fd: a snapshot, [`super::wire`]) | `-EINVAL` for a bad snapshot, `-ENOENT` for a partial one of a window without a full one, `-EBUSY` over 32 windows |
//! | `0x0703` | REMOVE  | `u32 window`                                                  | The window's tree is gone from the desktop                             |
//!
//! | Event    | Message | Body                                                                                                      |
//! | -------- | ------- | --------------------------------------------------------------------------------------------------------- |
//! | `0x4701` | ACTIVE  | `u8 active`                                                                                               |
//! | `0x4702` | ACTION  | `u32 window`, `u64 node`, `u8 action` ([`super::wire::action`] bit index, or 31 custom), `u8 0`, `u16 0`, `i32 a`, `i32 b`, `f32 value`, `u16 len` + UTF-8 text (≤ 400 bytes) |
//!
//! `window` is the Android display id, which is also the host window's id
//! (`input::proto`). ACTIVE says whether the desktop's AT-SPI bus is
//! enabled (`org.a11y.Status` `IsEnabled` or `ScreenReaderEnabled`): the
//! client keeps its Android accessibility service off otherwise, since
//! enabling one changes how apps behave and costs CPU. Test sessions on the
//! user's own bus are never active.
//!
//! ACTION arguments: SET_SELECTION has `a` and `b` (UTF-16 offsets),
//! SET_PROGRESS has `value`, SET_TEXT has the text, CUSTOM has `a` (the
//! action id); the others have none (all 0).

use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const START: u16 = 0x0701;
    pub const TREE: u16 = 0x0702;
    pub const REMOVE: u16 = 0x0703;
}

pub mod ev {
    pub const ACTIVE: u16 = 0x4701;
    pub const ACTION: u16 = 0x4702;
}

pub mod limits {
    /// Longest snapshot blob.
    pub const TREE_BLOB: u32 = 16 << 20;
    /// Windows with a tree, per connection.
    pub const WINDOWS: usize = 32;
    /// Longest text in an ACTION event.
    pub const ACTION_TEXT: usize = 400;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Start,
    /// `u32 window`, `u32 len`; fd: the snapshot.
    Tree {
        window: u32,
        len: u32,
    },
    Remove {
        window: u32,
    },
}

pub fn decode(ty: u16, body: &[u8]) -> Result<Request, DecodeError> {
    let mut r = Reader::new(body);
    let req = match ty {
        ty::START => Request::Start,
        ty::TREE => {
            let window = r.u32()?;
            let len = r.u32()?;
            if len == 0 || len > limits::TREE_BLOB {
                return Err(DecodeError::Invalid("len"));
            }
            Request::Tree { window, len }
        }
        ty::REMOVE => Request::Remove { window: r.u32()? },
        t => return Err(DecodeError::UnknownType(t)),
    };
    r.end()?;
    Ok(req)
}

pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let mut w;
    match *req {
        Request::Start => w = Writer::message(ty::START, tag),
        Request::Tree { window, len } => {
            w = Writer::message(ty::TREE, tag);
            w.u32(window).u32(len);
        }
        Request::Remove { window } => {
            w = Writer::message(ty::REMOVE, tag);
            w.u32(window);
        }
    }
    w.into_vec()
}

/// What an assistive technology asked a node to do.
#[derive(Debug, Clone, PartialEq)]
pub struct ActionEvent {
    pub window: u32,
    pub node: u64,
    pub action: u8,
    pub a: i32,
    pub b: i32,
    pub value: f32,
    pub text: String,
}

impl ActionEvent {
    /// The event body; text longer than [`limits::ACTION_TEXT`] is cut.
    pub fn encode(&self) -> Vec<u8> {
        let text = crate::ipc::wire::truncate(&self.text, limits::ACTION_TEXT);
        let mut w = Writer::default();
        w.u32(self.window)
            .u64(self.node)
            .u8(self.action)
            .u8(0)
            .u16(0)
            .i32(self.a)
            .i32(self.b)
            .f32(self.value)
            .u16(text.len() as u16);
        w.0.extend_from_slice(text.as_bytes());
        w.into_vec()
    }
}

pub fn active_event(active: bool) -> Vec<u8> {
    vec![active as u8]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::wire::{HDR, MAX_MSG, decode_header};

    #[test]
    fn examples() {
        for r in [
            Request::Start,
            Request::Tree {
                window: 3,
                len: 900,
            },
            Request::Remove { window: 3 },
        ] {
            let b = encode(1, &r);
            let h = decode_header(&b).unwrap();
            assert_eq!(decode(h.ty, &b[HDR..]), Ok(r));
        }
        assert!(decode(ty::TREE, &[1, 0, 0, 0, 0, 0, 0, 0]).is_err()); // len 0
        assert!(decode(ty::TREE, &[1, 0, 0, 0, 1, 0, 0, 2]).is_err()); // over the cap
        assert!(decode(ty::START, &[0]).is_err());
        assert_eq!(decode(0x0799, &[]), Err(DecodeError::UnknownType(0x0799)));
    }

    #[test]
    fn action_events_fit_one_message() {
        let e = ActionEvent {
            window: 1,
            node: 2,
            action: 14,
            a: 0,
            b: 0,
            value: 0.0,
            text: "é".repeat(400),
        };
        let b = e.encode();
        assert!(HDR + b.len() <= MAX_MSG);
        let n = u16::from_le_bytes([b[28], b[29]]) as usize;
        assert_eq!(n, b.len() - 30);
        assert!(std::str::from_utf8(&b[30..]).is_ok());
    }
}
