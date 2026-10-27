//! Display service messages (types 0x0200..=0x02ff; spec in `docs/bridge.md`,
//! "Display service").
//!
//! The client is the composer (the Android HWC HAL). Windows are named by the
//! Android display id they show; buffers by ids the composer picks.

use super::ime;
use crate::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const WINDOW_CREATE: u16 = 0x0201;
    pub const WINDOW_DESTROY: u16 = 0x0202;
    pub const WINDOW_SET_TITLE: u16 = 0x0203;
    pub const WINDOW_SET_MIN_SIZE: u16 = 0x0204;
    pub const WINDOW_ACTIVATE: u16 = 0x0205;
    pub const BUFFER_ADD: u16 = 0x0206;
    pub const BUFFER_REMOVE: u16 = 0x0207;
    pub const FRAME: u16 = 0x0208;
    pub const TEXT_INPUT: u16 = 0x0209;

    /// Events (bridge to composer, tag 0).
    pub const CONFIGURE: u16 = 0x4201;
    pub const CLOSE: u16 = 0x4202;
    pub const RELEASE: u16 = 0x4203;
    pub const INPUT: u16 = 0x4204;
}

/// Bounds on what a client may ask for.
pub mod limits {
    /// Window and buffer sizes, in pixels or logical pixels.
    pub const MAX_DIM: u32 = 16384;
    /// Bytes per row, as a multiple of the width's minimum.
    pub const MAX_STRIDE: u32 = 4 * MAX_DIM + 4096;
    /// Longest input line in an INPUT event.
    pub const MAX_LINE: usize = 400;
}

/// DRM fourccs the bridge accepts (linear, 4 bytes per pixel).
pub mod fourcc {
    const fn cc(a: u8, b: u8, c: u8, d: u8) -> u32 {
        a as u32 | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
    }
    pub const XBGR8888: u32 = cc(b'X', b'B', b'2', b'4');
    pub const ABGR8888: u32 = cc(b'A', b'B', b'2', b'4');
    pub const XRGB8888: u32 = cc(b'X', b'R', b'2', b'4');
    pub const ARGB8888: u32 = cc(b'A', b'R', b'2', b'4');
    pub fn supported(f: u32) -> bool {
        matches!(f, XBGR8888 | ABGR8888 | XRGB8888 | ARGB8888)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowSpec {
    pub window: u32,
    /// Initial logical size.
    pub width: u32,
    pub height: u32,
    /// Minimum logical size (0: the bridge's default).
    pub min_width: u32,
    pub min_height: u32,
    pub app_id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferSpec {
    pub buffer: u32,
    pub window: u32,
    pub width: u32,
    pub height: u32,
    pub fourcc: u32,
    pub stride: u32,
    pub offset: u32,
    /// DRM format modifier; only LINEAR (0) for now.
    pub modifier: u64,
}

impl BufferSpec {
    /// Bytes the buffer occupies in its dma-buf.
    pub fn bytes(&self) -> u64 {
        self.offset as u64 + self.stride as u64 * self.height as u64
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    WindowCreate(WindowSpec),
    WindowDestroy {
        window: u32,
    },
    WindowSetTitle {
        window: u32,
        title: String,
    },
    WindowSetMinSize {
        window: u32,
        width: u32,
        height: u32,
    },
    WindowActivate {
        window: u32,
    },
    /// Carries one fd: the dma-buf.
    BufferAdd(BufferSpec),
    BufferRemove {
        buffer: u32,
    },
    /// Show `buffer` in `window`. Damage in buffer pixels (all zero: whole
    /// buffer). May carry one fd: an acquire fence.
    Frame {
        window: u32,
        buffer: u32,
        damage: [i32; 4],
    },
    /// A line from the Linux.apk IME, relayed by the composer.
    TextInput(ime::Req),
}

fn dim(r: &mut Reader<'_>, what: &'static str) -> Result<u32, DecodeError> {
    r.u32_in(1..=limits::MAX_DIM, what)
}

fn min_dim(r: &mut Reader<'_>) -> Result<u32, DecodeError> {
    r.u32_in(0..=limits::MAX_DIM, "minimum size")
}

pub fn decode(ty: u16, body: &[u8]) -> Result<Request, DecodeError> {
    let mut r = Reader::new(body);
    let req = match ty {
        ty::WINDOW_CREATE => Request::WindowCreate(WindowSpec {
            window: r.u32()?,
            width: dim(&mut r, "width")?,
            height: dim(&mut r, "height")?,
            min_width: min_dim(&mut r)?,
            min_height: min_dim(&mut r)?,
            app_id: r.name()?,
            title: r.name()?,
        }),
        ty::WINDOW_DESTROY => Request::WindowDestroy { window: r.u32()? },
        ty::WINDOW_SET_TITLE => Request::WindowSetTitle {
            window: r.u32()?,
            title: r.name()?,
        },
        ty::WINDOW_SET_MIN_SIZE => Request::WindowSetMinSize {
            window: r.u32()?,
            width: min_dim(&mut r)?,
            height: min_dim(&mut r)?,
        },
        ty::WINDOW_ACTIVATE => Request::WindowActivate { window: r.u32()? },
        ty::BUFFER_ADD => {
            let b = BufferSpec {
                buffer: r.u32()?,
                window: r.u32()?,
                width: dim(&mut r, "width")?,
                height: dim(&mut r, "height")?,
                fourcc: r.u32()?,
                stride: r.u32_in(4..=limits::MAX_STRIDE, "stride")?,
                offset: r.u32()?,
                modifier: r.u64()?,
            };
            if !fourcc::supported(b.fourcc) {
                return Err(DecodeError::Invalid("format"));
            }
            if b.stride < b.width * 4 {
                return Err(DecodeError::Invalid("stride below width"));
            }
            if b.modifier != 0 {
                return Err(DecodeError::Invalid("modifier"));
            }
            Request::BufferAdd(b)
        }
        ty::BUFFER_REMOVE => Request::BufferRemove { buffer: r.u32()? },
        ty::FRAME => Request::Frame {
            window: r.u32()?,
            buffer: r.u32()?,
            damage: [r.i32()?, r.i32()?, r.i32()?, r.i32()?],
        },
        ty::TEXT_INPUT => {
            let n = r.u16()? as usize;
            if n > ime::MAX_LINE {
                return Err(DecodeError::Invalid("text-input line too long"));
            }
            let line = std::str::from_utf8(r.take(n)?)
                .map_err(|_| DecodeError::Invalid("text-input line not UTF-8"))?;
            Request::TextInput(ime::parse(line).ok_or(DecodeError::Invalid("text-input line"))?)
        }
        t => return Err(DecodeError::UnknownType(t)),
    };
    r.end()?;
    Ok(req)
}

pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let mut w;
    match req {
        Request::WindowCreate(s) => {
            w = Writer::message(ty::WINDOW_CREATE, tag);
            w.u32(s.window)
                .u32(s.width)
                .u32(s.height)
                .u32(s.min_width)
                .u32(s.min_height)
                .name(&s.app_id)
                .name(&s.title);
        }
        Request::WindowDestroy { window } => {
            w = Writer::message(ty::WINDOW_DESTROY, tag);
            w.u32(*window);
        }
        Request::WindowSetTitle { window, title } => {
            w = Writer::message(ty::WINDOW_SET_TITLE, tag);
            w.u32(*window).name(title);
        }
        Request::WindowSetMinSize {
            window,
            width,
            height,
        } => {
            w = Writer::message(ty::WINDOW_SET_MIN_SIZE, tag);
            w.u32(*window).u32(*width).u32(*height);
        }
        Request::WindowActivate { window } => {
            w = Writer::message(ty::WINDOW_ACTIVATE, tag);
            w.u32(*window);
        }
        Request::BufferAdd(b) => {
            w = Writer::message(ty::BUFFER_ADD, tag);
            w.u32(b.buffer)
                .u32(b.window)
                .u32(b.width)
                .u32(b.height)
                .u32(b.fourcc)
                .u32(b.stride)
                .u32(b.offset)
                .u64(b.modifier);
        }
        Request::BufferRemove { buffer } => {
            w = Writer::message(ty::BUFFER_REMOVE, tag);
            w.u32(*buffer);
        }
        Request::Frame {
            window,
            buffer,
            damage,
        } => {
            w = Writer::message(ty::FRAME, tag);
            w.u32(*window).u32(*buffer);
            for d in damage {
                w.i32(*d);
            }
        }
        Request::TextInput(r) => {
            w = Writer::message(ty::TEXT_INPUT, tag);
            let l = r.to_line();
            w.u16(l.len() as u16);
            w.0.extend_from_slice(l.as_bytes());
        }
    }
    w.into_vec()
}

/// Events, as bodies (the caller adds the header).
pub mod event {
    use super::*;

    pub fn configure(window: u32, width: u32, height: u32, scale120: u32) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(window).u32(width).u32(height).u32(scale120);
        w.into_vec()
    }
    pub fn close(window: u32) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(window);
        w.into_vec()
    }
    pub fn release(buffer: u32) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(buffer);
        w.into_vec()
    }
    /// One input-protocol v2 line (docs/hal-display.md), without '\n'.
    pub fn input(line: &str) -> Vec<u8> {
        let b = &line.as_bytes()[..line.len().min(limits::MAX_LINE)];
        let mut w = Writer::default();
        w.u16(b.len() as u16);
        w.0.extend_from_slice(b);
        w.into_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{HDR, decode_header};
    use proptest::prelude::*;

    fn roundtrip(b: &[u8]) -> Result<Request, DecodeError> {
        let h = decode_header(b)?;
        decode(h.ty, &b[HDR..])
    }

    fn buffer(stride: u32) -> Request {
        Request::BufferAdd(BufferSpec {
            buffer: 7,
            window: 3,
            width: 1280,
            height: 800,
            fourcc: fourcc::XBGR8888,
            stride,
            offset: 0,
            modifier: 0,
        })
    }

    #[test]
    fn roundtrips() {
        for req in [
            Request::WindowCreate(WindowSpec {
                window: 3,
                width: 1280,
                height: 800,
                min_width: 240,
                min_height: 0,
                app_id: "dev.linuxapk.org.wikipedia".into(),
                title: "Wikipedia".into(),
            }),
            Request::WindowDestroy { window: 3 },
            Request::WindowSetTitle {
                window: 3,
                title: "Ünïcode ok".into(),
            },
            Request::WindowSetMinSize {
                window: 3,
                width: 412,
                height: 600,
            },
            Request::WindowActivate { window: 3 },
            buffer(5120),
            Request::BufferRemove { buffer: 7 },
            Request::Frame {
                window: 3,
                buffer: 7,
                damage: [0, 0, 1280, 800],
            },
            Request::TextInput(ime::Req::Surrounding {
                text: "日本語 text".into(),
                cursor: 3,
                anchor: 3,
                other: true,
            }),
            Request::TextInput(ime::Req::Apply),
        ] {
            assert_eq!(roundtrip(&encode(1, &req)), Ok(req));
        }
    }

    #[test]
    fn rejects_bad_buffers() {
        assert!(roundtrip(&encode(1, &buffer(1000))).is_err()); // stride < width*4
        let mut b = buffer(5120);
        if let Request::BufferAdd(s) = &mut b {
            s.modifier = 1;
        }
        assert!(roundtrip(&encode(1, &b)).is_err());
        if let Request::BufferAdd(s) = &mut b {
            s.modifier = 0;
            s.fourcc = 0x3231564e; // NV12
        }
        assert!(roundtrip(&encode(1, &b)).is_err());
        assert_eq!(decode(0x02ff, &[]), Err(DecodeError::UnknownType(0x02ff)));
    }

    #[test]
    fn input_lines_are_bounded() {
        let long = "x".repeat(1000);
        assert_eq!(event::input(&long).len(), 2 + limits::MAX_LINE);
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20000))]

        // The decoder never panics, and anything it accepts re-encodes to
        // the same bytes.
        #[test]
        fn decode_arbitrary_bodies(t in 0x0200u16..0x0210, body in proptest::collection::vec(any::<u8>(), 0..300)) {
            if let Ok(req) = decode(t, &body) {
                prop_assert_eq!(&encode(9, &req)[HDR..], &body[..]);
            }
        }

        // Mutating one byte of a valid message never panics.
        #[test]
        fn decode_mutated(stride in 0u32..80000, pos in 0usize..64, val in any::<u8>()) {
            let mut b = encode(3, &buffer(stride));
            let i = pos % b.len();
            b[i] = val;
            let _ = roundtrip(&b);
        }
    }
}
