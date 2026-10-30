//! Display service messages (id 2, types 0x0200..=0x02ff, events
//! 0x4200..=0x42ff). The client is the composer (the Android HWC HAL): it
//! names its HWC virtual displays by its own ids, and buffers by ids it
//! picks. Which display shows which window is the input service's business
//! (`input::proto`): a display of the size the window manager asked for gets
//! that window.
//!
//! | Type     | Request        | Body                                                                                                                                                                                 | Reply                                                                                       |
//! | -------- | -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------- |
//! | `0x0201` | DISPLAY_ADD    | `u32 display`, `u32 width`, `u32 height` (pixels)                                                                                                                                    | `u32 window`: shown in that window; `-ENOENT`: no window asked for it (it stays headless)   |
//! | `0x0202` | DISPLAY_REMOVE | `u32 display`                                                                                                                                                                        | Closes its window and drops its buffers                                                     |
//! | `0x0203` | BUFFER_ADD     | `u32 buffer`, `u32 display`, `u32 width`, `u32 height`, `u32 fourcc` (XBGR/ABGR/XRGB/ARGB8888), `u32 stride` (≥ 4×width), `u32 offset`, `u64 modifier` (0, LINEAR); fd: the dma-buf | The fd's size must cover `offset + stride × height`                                        |
//! | `0x0204` | BUFFER_REMOVE  | `u32 buffer`                                                                                                                                                                         |                                                                                             |
//! | `0x0205` | FRAME          | `u32 display`, `u32 buffer`, `i32 x, y, w, h` damage (zero: everything); optional fd: acquire fence (ignored)                                                                       | The buffer is the composer's again at RELEASE; one the window can't show yet is released at once |
//!
//! IMAGE_RELEASE (0x0207): u32 display, u64 presentation token: release a
//! leased ImageReader image after HWC observes RELEASE. Forwarded to that window on the input service.
//!
//! CURSOR (0x0206): u32 display, u32 width, u32 height (each <=512),
//! i32 x, i32 y (layer origin in display pixels). Nonzero dimensions carry
//! one sealed memfd with width*height*4 premultiplied BGRA bytes (wl_shm
//! ARGB8888); 0x0, without an fd, hides the cursor. The layer is excluded
//! from FRAME. The input service supplies the last pointer position to
//! derive the hotspot; HWC3 has no style-name or hotspot metadata.
//!
//! | Event    | Message | Body                                         |
//! | -------- | ------- | -------------------------------------------- |
//! | `0x4201` | RELEASE | `u32 buffer`, `u32 invalid` (0: reusable, 1: remove cached import)   |

use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const DISPLAY_ADD: u16 = 0x0201;
    pub const DISPLAY_REMOVE: u16 = 0x0202;
    pub const BUFFER_ADD: u16 = 0x0203;
    pub const BUFFER_REMOVE: u16 = 0x0204;
    pub const FRAME: u16 = 0x0205;
    pub const CURSOR: u16 = 0x0206;
    pub const IMAGE_RELEASE: u16 = 0x0207;

    /// Events (bridge to composer, tag 0).
    pub const RELEASE: u16 = 0x4201;
}

/// Bounds on what a client may ask for.
pub mod limits {
    /// Display and buffer sizes, in pixels.
    pub const MAX_DIM: u32 = 16384;
    /// Bytes per row, as a multiple of the width's minimum.
    pub const MAX_STRIDE: u32 = 4 * MAX_DIM + 4096;
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
pub struct BufferSpec {
    pub buffer: u32,
    pub display: u32,
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
    ImageRelease {
        display: u32,
        token: u64,
    },
    Cursor {
        display: u32,
        width: u32,
        height: u32,
        x: i32,
        y: i32,
    },
    DisplayAdd {
        display: u32,
        width: u32,
        height: u32,
    },
    DisplayRemove {
        display: u32,
    },
    /// Carries one fd: the dma-buf.
    BufferAdd(BufferSpec),
    BufferRemove {
        buffer: u32,
    },
    /// Show `buffer` on `display`. Damage in buffer pixels (all zero: whole
    /// buffer). May carry one fd: an acquire fence.
    Frame {
        display: u32,
        buffer: u32,
        damage: [i32; 4],
    },
}

fn dim(r: &mut Reader<'_>, what: &'static str) -> Result<u32, DecodeError> {
    r.u32_in(1..=limits::MAX_DIM, what)
}

pub fn decode(ty: u16, body: &[u8]) -> Result<Request, DecodeError> {
    let mut r = Reader::new(body);
    let req = match ty {
        ty::IMAGE_RELEASE => Request::ImageRelease {
            display: r.u32()?,
            token: r.u64()?,
        },
        ty::CURSOR => Request::Cursor {
            display: r.u32()?,
            width: r.u32_in(0..=512, "cursor width")?,
            height: r.u32_in(0..=512, "cursor height")?,
            x: r.i32()?,
            y: r.i32()?,
        },
        ty::DISPLAY_ADD => Request::DisplayAdd {
            display: r.u32()?,
            width: dim(&mut r, "width")?,
            height: dim(&mut r, "height")?,
        },
        ty::DISPLAY_REMOVE => Request::DisplayRemove { display: r.u32()? },
        ty::BUFFER_ADD => {
            let b = BufferSpec {
                buffer: r.u32()?,
                display: r.u32()?,
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
            display: r.u32()?,
            buffer: r.u32()?,
            damage: [r.i32()?, r.i32()?, r.i32()?, r.i32()?],
        },
        t => return Err(DecodeError::UnknownType(t)),
    };
    r.end()?;
    Ok(req)
}

pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let mut w;
    match req {
        Request::ImageRelease { display, token } => {
            w = Writer::message(ty::IMAGE_RELEASE, tag);
            w.u32(*display).u64(*token);
        }
        Request::Cursor {
            display,
            width,
            height,
            x,
            y,
        } => {
            w = Writer::message(ty::CURSOR, tag);
            w.u32(*display).u32(*width).u32(*height).i32(*x).i32(*y);
        }
        Request::DisplayAdd {
            display,
            width,
            height,
        } => {
            w = Writer::message(ty::DISPLAY_ADD, tag);
            w.u32(*display).u32(*width).u32(*height);
        }
        Request::DisplayRemove { display } => {
            w = Writer::message(ty::DISPLAY_REMOVE, tag);
            w.u32(*display);
        }
        Request::BufferAdd(b) => {
            w = Writer::message(ty::BUFFER_ADD, tag);
            w.u32(b.buffer)
                .u32(b.display)
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
            display,
            buffer,
            damage,
        } => {
            w = Writer::message(ty::FRAME, tag);
            w.u32(*display).u32(*buffer);
            for d in damage {
                w.i32(*d);
            }
        }
    }
    w.into_vec()
}

/// Events, as bodies (the caller adds the header).
pub mod event {
    use super::*;

    pub fn release(buffer: u32, invalid: bool) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(buffer).u32(u32::from(invalid));
        w.into_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::wire::{HDR, decode_header};

    fn roundtrip(b: &[u8]) -> Result<Request, DecodeError> {
        let h = decode_header(b)?;
        decode(h.ty, &b[HDR..])
    }

    fn buffer(stride: u32) -> Request {
        Request::BufferAdd(BufferSpec {
            buffer: 7,
            display: 3,
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
            Request::DisplayAdd {
                display: 3,
                width: 2176,
                height: 1360,
            },
            Request::DisplayRemove { display: 3 },
            buffer(5120),
            Request::BufferRemove { buffer: 7 },
            Request::ImageRelease {
                display: 3,
                token: 0x123456789abcdef0,
            },
            Request::Frame {
                display: 3,
                buffer: 7,
                damage: [0, 0, 1280, 800],
            },
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
}
