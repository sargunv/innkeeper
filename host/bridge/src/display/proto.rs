//! Display service messages (id 2, types 0x0200..=0x02ff, events
//! 0x4200..=0x42ff). The client is the composer, the Android HWC HAL. It
//! names its HWC virtual displays and its buffers by ids it picks. The input
//! service (`input::proto`) decides which display shows which window. A
//! display of the size the window manager asked for gets that window.
//!
//! | Type     | Request        | Body                                                                                                                                                                                 | Reply                                                                                       |
//! | -------- | -------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------- |
//! | `0x0201` | DISPLAY_ADD    | `u32 display`, `u32 width`, `u32 height` (pixels)                                                                                                                                    | `u32 window`: shown in that window; `-ENOENT`: no window asked for it (it stays headless)   |
//! | `0x0202` | DISPLAY_REMOVE | `u32 display`                                                                                                                                                                        | Closes its window and drops its buffers                                                     |
//! | `0x0203` | BUFFER_ADD     | `u32 buffer`, `u32 display`, `u32 width`, `u32 height`, `u32 fourcc` (8888, ABGR2101010, ABGR16161616F or NV12), `u32 stride` (≥ bytes-per-pixel×width), `u32 offset`, `u64 modifier` (allocator: LINEAR); fd: the dma-buf | The fd's size must cover the buffer's rows (NV12: the chroma rows follow the luma rows at the same stride) |
//! | `0x0204` | BUFFER_REMOVE  | `u32 buffer`                                                                                                                                                                         |                                                                                             |
//! | `0x0205` | FRAME          | `u32 display`, `u32 buffer`, `i32 x, y, w, h` damage (zero: everything); optional fd: acquire fence (ignored)                                                                       | The buffer is the composer's again at RELEASE; one the window can't show yet is released at once |
//!
//! DISPLAY_WINDOW (0x0208): u32 display, u32 width, u32 height (pixels),
//! u32 dpi, name app_id, name title. Like DISPLAY_ADD, but the display gets
//! a new window of its own instead of one the window manager asked for on the
//! input service. It is for a guest without a window manager on the input
//! service, such as OpenHarmony's composer. The reply is `u32 window`. The
//! window's input goes to the broker's touchscreen (`input::direct`).
//!
//! LAYERS (0x0209): u32 display, u32 below, u32 count, then for each layer,
//! bottom to top: u32 buffer, i32 x, i32 y, u32 width, u32 height (where it
//! shows, in display pixels), u32 crop_x, u32 crop_y, u32 crop_width,
//! u32 crop_height (the part of the buffer it shows). The first `below`
//! layers show under the display's frame and the rest show over it. The
//! frame's transparent pixels show the layers under it. The display shows
//! these layers until the next LAYERS, and a count of 0 removes them. Each
//! buffer that a LAYERS shows anew is the composer's again at its RELEASE,
//! and a buffer that stays at the same position in the list stays shown. It is
//! for OpenHarmony's composer, which gives video layers to the host
//! compositor.
//!
//! IMAGE_RELEASE (0x0207): u32 display, u64 presentation token. HWC sends it
//! after it observes RELEASE, to release a leased ImageReader image. The
//! bridge forwards it to that display's window on the input service.
//!
//! CURSOR (0x0206): u32 display, u32 width, u32 height (each <=512),
//! i32 x, i32 y (layer origin in display pixels), and optionally i32 hot_x,
//! i32 hot_y (the hotspot in the image). A cursor with nonzero dimensions
//! carries one sealed memfd with width*height*4 premultiplied BGRA bytes
//! (wl_shm ARGB8888). A 0x0 cursor without an fd hides the cursor. The layer
//! is excluded from FRAME. HWC3 has no style-name or hotspot metadata, so
//! Android's composer sends no hotspot, and the bridge derives one from the
//! last pointer position that the input service supplies. OpenHarmony's
//! composer sends the hotspot that MMI drew the image around.
//!
//! | Event    | Message | Body                                         |
//! | -------- | ------- | -------------------------------------------- |
//! | `0x4201` | RELEASE | `u32 buffer`, `u32 invalid` (0: reusable, 1: remove cached import)   |
//! | `0x4202` | VSYNC | `u64 monotonic ns`, `u32 period ns`; both zero return display 0 to its timer |

use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const DISPLAY_ADD: u16 = 0x0201;
    pub const DISPLAY_REMOVE: u16 = 0x0202;
    pub const BUFFER_ADD: u16 = 0x0203;
    pub const BUFFER_REMOVE: u16 = 0x0204;
    pub const FRAME: u16 = 0x0205;
    pub const CURSOR: u16 = 0x0206;
    pub const IMAGE_RELEASE: u16 = 0x0207;
    pub const DISPLAY_WINDOW: u16 = 0x0208;
    pub const LAYERS: u16 = 0x0209;

    /// Events (bridge to composer, tag 0).
    pub const RELEASE: u16 = 0x4201;
    /// Primary scheduler phase: u64 CLOCK_MONOTONIC ns, u32 refresh period ns.
    pub const VSYNC: u16 = 0x4202;
}

/// Bounds on what a client may ask for.
pub mod limits {
    /// Display and buffer sizes, in pixels.
    pub const MAX_DIM: u32 = 16384;
    /// The largest row stride in bytes. It allows the widest row at 8 bytes
    /// per pixel plus 4096 bytes of padding.
    pub const MAX_STRIDE: u32 = 8 * MAX_DIM + 4096;
    /// Layers in one LAYERS request.
    pub const MAX_LAYERS: u32 = 8;
}

/// Packed RGB formats supplied by the allocator, and NV12 for layers.
pub mod fourcc {
    const fn cc(a: u8, b: u8, c: u8, d: u8) -> u32 {
        a as u32 | (b as u32) << 8 | (c as u32) << 16 | (d as u32) << 24
    }
    pub const XBGR8888: u32 = cc(b'X', b'B', b'2', b'4');
    pub const ABGR8888: u32 = cc(b'A', b'B', b'2', b'4');
    pub const XRGB8888: u32 = cc(b'X', b'R', b'2', b'4');
    pub const ARGB8888: u32 = cc(b'A', b'R', b'2', b'4');
    pub const ABGR2101010: u32 = cc(b'A', b'B', b'3', b'0');
    pub const ABGR16161616F: u32 = cc(b'A', b'B', b'4', b'H');
    pub const NV12: u32 = cc(b'N', b'V', b'1', b'2');
    /// The bytes per pixel of the first plane.
    pub fn bytes_per_pixel(f: u32) -> Option<u32> {
        match f {
            XBGR8888 | ABGR8888 | XRGB8888 | ARGB8888 | ABGR2101010 => Some(4),
            ABGR16161616F => Some(8),
            NV12 => Some(1),
            _ => None,
        }
    }
    /// The rows of `height` pixels at the buffer's stride. NV12's chroma
    /// plane follows the luma plane with half its rows.
    pub fn rows(f: u32, height: u32) -> u64 {
        match f {
            NV12 => height as u64 + height.div_ceil(2) as u64,
            _ => height as u64,
        }
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
    /// DRM format modifier (our allocator supplies LINEAR).
    pub modifier: u64,
}

impl BufferSpec {
    /// Bytes the buffer occupies in its dma-buf.
    pub fn bytes(&self) -> u64 {
        self.offset as u64 + self.stride as u64 * fourcc::rows(self.fourcc, self.height)
    }
}

/// A layer of a LAYERS request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layer {
    pub buffer: u32,
    /// Where it shows, in display pixels: x, y, width, height.
    pub rect: [i32; 4],
    /// The part of the buffer it shows: x, y, width, height.
    pub crop: [u32; 4],
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
        hotspot: Option<(i32, i32)>,
    },
    DisplayAdd {
        display: u32,
        width: u32,
        height: u32,
    },
    DisplayRemove {
        display: u32,
    },
    DisplayWindow {
        display: u32,
        width: u32,
        height: u32,
        dpi: u32,
        app_id: String,
        title: String,
    },
    /// Carries the dma-buf as one fd.
    BufferAdd(BufferSpec),
    BufferRemove {
        buffer: u32,
    },
    /// The layers of `display`, bottom to top. The first `below` show under
    /// its frame.
    Layers {
        display: u32,
        below: u32,
        layers: Vec<Layer>,
    },
    /// Shows `buffer` on `display`. Damage is in buffer pixels, and all zero
    /// means the whole buffer. It may carry an acquire fence as one fd.
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
            hotspot: if r.is_empty() {
                None
            } else {
                Some((r.i32()?, r.i32()?))
            },
        },
        ty::DISPLAY_ADD => Request::DisplayAdd {
            display: r.u32()?,
            width: dim(&mut r, "width")?,
            height: dim(&mut r, "height")?,
        },
        ty::DISPLAY_REMOVE => Request::DisplayRemove { display: r.u32()? },
        ty::DISPLAY_WINDOW => Request::DisplayWindow {
            display: r.u32()?,
            width: dim(&mut r, "width")?,
            height: dim(&mut r, "height")?,
            dpi: r.u32_in(1..=crate::input::proto::limits::MAX_DPI, "dpi")?,
            app_id: r.name()?,
            title: r.name()?,
        },
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
            let bpp = fourcc::bytes_per_pixel(b.fourcc).ok_or(DecodeError::Invalid("format"))?;
            if b.stride < b.width * bpp {
                return Err(DecodeError::Invalid("stride below width"));
            }
            if b.modifier != 0 {
                return Err(DecodeError::Invalid("modifier"));
            }
            Request::BufferAdd(b)
        }
        ty::BUFFER_REMOVE => Request::BufferRemove { buffer: r.u32()? },
        ty::LAYERS => {
            let display = r.u32()?;
            let below = r.u32()?;
            let count = r.u32_in(0..=limits::MAX_LAYERS, "layer count")?;
            if below > count {
                return Err(DecodeError::Invalid("below exceeds count"));
            }
            let mut layers = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let layer = Layer {
                    buffer: r.u32()?,
                    rect: [
                        r.i32()?,
                        r.i32()?,
                        dim(&mut r, "layer width")? as i32,
                        dim(&mut r, "layer height")? as i32,
                    ],
                    crop: [
                        r.u32()?,
                        r.u32()?,
                        dim(&mut r, "crop width")?,
                        dim(&mut r, "crop height")?,
                    ],
                };
                if layers.iter().any(|l: &Layer| l.buffer == layer.buffer) {
                    return Err(DecodeError::Invalid("a buffer in two layers"));
                }
                layers.push(layer);
            }
            Request::Layers {
                display,
                below,
                layers,
            }
        }
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
            hotspot,
        } => {
            w = Writer::message(ty::CURSOR, tag);
            w.u32(*display).u32(*width).u32(*height).i32(*x).i32(*y);
            if let Some((hx, hy)) = hotspot {
                w.i32(*hx).i32(*hy);
            }
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
        Request::DisplayWindow {
            display,
            width,
            height,
            dpi,
            app_id,
            title,
        } => {
            w = Writer::message(ty::DISPLAY_WINDOW, tag);
            w.u32(*display)
                .u32(*width)
                .u32(*height)
                .u32(*dpi)
                .name(app_id)
                .name(title);
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
        Request::Layers {
            display,
            below,
            layers,
        } => {
            w = Writer::message(ty::LAYERS, tag);
            w.u32(*display).u32(*below).u32(layers.len() as u32);
            for l in layers {
                w.u32(l.buffer);
                for v in l.rect {
                    w.i32(v);
                }
                for v in l.crop {
                    w.u32(v);
                }
            }
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

/// Event bodies. The caller adds the header.
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
            Request::DisplayWindow {
                display: 0,
                width: 720,
                height: 1280,
                dpi: 320,
                app_id: "dev.sargunv.innkeeper.ohos".into(),
                title: "OpenHarmony".into(),
            },
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
            Request::Cursor {
                display: 3,
                width: 36,
                height: 55,
                x: -4,
                y: 10,
                hotspot: None,
            },
            Request::Cursor {
                display: 0,
                width: 36,
                height: 55,
                x: 100,
                y: 200,
                hotspot: Some((0, 0)),
            },
            Request::Layers {
                display: 0,
                below: 1,
                layers: vec![layer(8), layer(9)],
            },
            Request::Layers {
                display: 0,
                below: 0,
                layers: vec![],
            },
        ] {
            assert_eq!(roundtrip(&encode(1, &req)), Ok(req));
        }
    }

    fn layer(buffer: u32) -> Layer {
        Layer {
            buffer,
            rect: [36, 558, 648, 486],
            crop: [0, 0, 320, 240],
        }
    }

    #[test]
    fn nv12_buffers_cover_their_chroma_rows() {
        let b = BufferSpec {
            buffer: 1,
            display: 0,
            width: 320,
            height: 241,
            fourcc: fourcc::NV12,
            stride: 512,
            offset: 0,
            modifier: 0,
        };
        assert_eq!(b.bytes(), 512 * (241 + 121));
        let req = Request::BufferAdd(b.clone());
        assert_eq!(roundtrip(&encode(1, &req)), Ok(req));
        let narrow = Request::BufferAdd(BufferSpec { stride: 256, ..b });
        assert!(roundtrip(&encode(1, &narrow)).is_err()); // stride < width
    }

    #[test]
    fn rejects_bad_layers() {
        let layers = |below, layers: Vec<Layer>| {
            roundtrip(&encode(
                1,
                &Request::Layers {
                    display: 0,
                    below,
                    layers,
                },
            ))
        };
        assert!(layers(2, vec![layer(8)]).is_err()); // below > count
        assert!(layers(0, vec![layer(8), layer(8)]).is_err()); // one buffer twice
        assert!(layers(0, (0..9).map(layer).collect()).is_err()); // too many
        let mut empty = layer(8);
        empty.crop[2] = 0;
        assert!(layers(0, vec![empty]).is_err());
        let mut empty = layer(8);
        empty.rect[3] = 0;
        assert!(layers(0, vec![empty]).is_err());
    }

    #[test]
    fn wide_buffers_require_the_real_pixel_size() {
        let mut b = buffer(5120);
        if let Request::BufferAdd(s) = &mut b {
            s.fourcc = fourcc::ABGR2101010;
        }
        assert_eq!(roundtrip(&encode(1, &b)), Ok(b.clone()));
        if let Request::BufferAdd(s) = &mut b {
            s.fourcc = fourcc::ABGR16161616F;
        }
        assert!(roundtrip(&encode(1, &b)).is_err());
        if let Request::BufferAdd(s) = &mut b {
            s.stride = 10240;
        }
        assert_eq!(roundtrip(&encode(1, &b)), Ok(b));
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
            s.fourcc = 0x3132564e; // NV21
        }
        assert!(roundtrip(&encode(1, &b)).is_err());
        assert_eq!(decode(0x02ff, &[]), Err(DecodeError::UnknownType(0x02ff)));
    }
}
