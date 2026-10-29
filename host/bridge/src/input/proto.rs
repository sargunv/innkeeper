//! Input service messages (id 6, types 0x0600..=0x06ff, events
//! 0x4600..=0x46ff). The client is the Linux.apk System app: its window
//! manager asks for windows, its injector gets the seat, and its IME drives
//! the host's input methods. Only the system app's uid may open the service.
//!
//! | Type     | Request        | Body                                                                                                                                         | Reply                                                     |
//! | -------- | -------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
//! | `0x0601` | WINDOW_REQUEST | `u32 window` (the Android display id), `u32 width`, `u32 height` (display pixels), `u32 dpi`, `u32 min_width`, `u32 min_height` (dp, 0: 240x160), name `app_id`, name `title`, name `token` | 0: queued; `-EBUSY`: the window is shown already |
//! | `0x0602` | WINDOW_ACTIVATE | `u32 window`, name `token`                                                                                                                  | `-ENOENT` if not shown; else `xdg_activation_v1` with `token`, or one for the newest input serial |
//! | `0x0603` | WINDOW_DESTROY | `u32 window`                                                                                                                                 | Drops a queued request, or closes the window now (its display may stay, headless) |
//! | `0x0604` | TEXT_INPUT     | `u16 len` + one `ti_*` line from the IME (≤ 480 bytes, [`super::ime`])                                                                       | Applied to the window with text-input focus               |
//!
//! | Event    | Message         | Body                                                                 |
//! | -------- | --------------- | -------------------------------------------------------------------- |
//! | `0x4601` | INPUT           | `u16 len` + one input line (≤ 400 bytes, below)                      |
//! | `0x4602` | WINDOW_MAPPED   | `u32 window`: its first frame is on screen                           |
//! | `0x4603` | WINDOW_UNMAPPED | `u32 window`: its display went away, or it was destroyed               |
//!
//! **Windows.** A queued request waits up to 10 s for the composer to add an
//! HWC virtual display of exactly `width`×`height` (display service
//! `DISPLAY_ADD`); that display then shows in a new toplevel, which starts at
//! the display's size in dp. The window manager queues the request before it
//! gives its VirtualDisplay a surface, which is what makes SurfaceFlinger
//! create the HWC display.
//!
//! **Activation.** `token` is an `xdg_activation_v1` token from the desktop
//! launch (`XDG_ACTIVATION_TOKEN`, or one the launch's splash window asked
//! for); empty: none. A new window uses it once its first frame is on
//! screen, so the compositor focuses it without flagging it as demanding
//! attention.
//!
//! `WINDOW_POLICY` (0x0605): u32 display, u32 orientation (0 unspecified,
//! 1 portrait, 2 landscape), u32 automatic fullscreen (0 or 1), u32 immersive
//! request (both bars hidden, 0 or 1). The separate immersive cycle preserves a
//! manual fullscreen exit across orientation eligibility changes. The Android
//! window manager sends the current policy when mapped and when it changes.
//! The bridge reshapes floating toplevels and translates fullscreen to xdg-shell.
//!
//! `WINDOW_MINIMIZE` (0x0607): u32 display; minimize the toplevel when Android
//! moves its root task to the back. `-ENOENT` if not shown.
//!
//! `WINDOW_APPEARANCE` (0x0606): u32 display, u32 primary colour (Android ARGB,
//! 0: default caption), u32 blob length, one sealed memfd containing a blob
//! RGBA image (u16 width/height, packed pixels; 0x0 clears the icon). Each side
//! is at most 256 pixels; at most 262148 bytes. Sent on mapping and debounced
//! TaskDescription changes. Unsupported icon protocols receive no fallback.
//! `EVDEV` (0x0608): u32 device (0 keyboard, 1 mouse), u64 monotonic
//! microseconds, u32 count (1..32), then count records of u16 type, u16 code,
//! i32 value. The bridge adds SYN_REPORT and writes to the broker's evdev.sock.
//! `POINTER_POSITION` (0x0609): u32 display, i32 x, i32 y in display pixels,
//! sent before mouse records so the cursor layer origin can yield its hotspot.
//!
//! **Input lines** are protocol v2: ASCII, space-separated fields, in the
//! order the seat produced them.
//!
//! - Coordinates are in the pixels of the target display (surface-local
//!   position × the shown frame's size / the window's logical size), as
//!   floats with 2 decimals. `<display>` is the window's Android display id:
//!   the lines that start a pointer, keyboard or touch sequence carry it, and
//!   the events that follow belong to that window. `<t>` is the compositor's
//!   millisecond clock; buttons and keys are Linux evdev codes.
//! - `window <logical_w> <logical_h> <scale> <display>`: the window's size
//!   or scale changed.
//! - `close <display>`: the user asked to close the window.
//! - `pointer_enter <x> <y> <display>`, `pointer_leave <display>`,
//!   `pointer_motion <t> <x> <y>`, `pointer_button <t> <button> <0|1>`,
//!   `pointer_axis <t> <axis> <value>` (axis 0 vertical, 1 horizontal; value
//!   in surface units), `pointer_axis_value120 <axis> <v120>` (120 = one
//!   detent), `pointer_axis_source <source>` (0 wheel, 1 finger,
//!   2 continuous, 3 wheel tilt; at most one per frame; the values already
//!   include natural scrolling), `pointer_axis_stop <t> <axis>`,
//!   `pointer_frame`.
//! - Touchpad gestures over a window's content, which go where the pointer
//!   is: `gesture_swipe_begin <t> <fingers>`, `gesture_swipe_update <t> <dx>
//!   <dy>`, `gesture_pinch_begin <t> <fingers>`, `gesture_pinch_update <t>
//!   <dx> <dy> <scale> <rotation>` (the center's motion in display pixels,
//!   the scale since the begin, degrees clockwise since the last update),
//!   `gesture_hold_begin <t> <fingers>`, and `gesture_{swipe,pinch,hold}_end
//!   <t> <0|1>` (1: cancelled).
//! - A tablet tool: `tablet_proximity_in <tool> <display>` (the tool is its
//!   `BTN_TOOL_*` code: 0x140 pen, 0x141 eraser, …), `tablet_proximity_out`,
//!   `tablet_down`, `tablet_up`, `tablet_motion <x> <y>`,
//!   `tablet_pressure <0..65535>`, `tablet_distance <0..65535>`,
//!   `tablet_tilt <x> <y>` (degrees), `tablet_button <button> <0|1>`, each
//!   group ended by `tablet_frame <t>`.
//! - `back <display>`: a title-bar Back click, a BACK key tap on that display.
//! - `keyboard_enter <display>`, `keyboard_leave <display>`,
//!   `key <t> <keycode> <0|1|2> [<sym> [text <text> | compose]]` (2: a
//!   repeat, from a wl_keyboard v10 compositor that repeats keys itself;
//!   KWin also repeats a key whose press a host input method turned into a
//!   press and release),
//!   `modifiers <depressed> <latched> <locked> <group>` (xkb masks),
//!   `repeat_info <rate> <delay>` (`wl_keyboard.repeat_info`: repeats per
//!   second, 0 when the compositor repeats or there is no repeat, and ms;
//!   also sent to each new connection).
//!   Under the compositor's keymap ([`super::xkb`]) a press also carries
//!   `<sym>`, the key's level-1 keysym in the active layout (decimal), and
//!   what it types: `text` and the percent-encoded text (as `ti_commit`),
//!   `compose` for a step of a compose sequence (it types nothing), or
//!   neither (a repeat never feeds compose). Without a keymap, and on
//!   release, only the evdev code.
//! - `touch_down <t> <id> <x> <y> <display>`, `touch_motion <t> <id> <x> <y>`,
//!   `touch_up <t> <id>`, `touch_frame`, `touch_cancel`.
//! - `ti_delete`, `ti_commit`, `ti_preedit`, `ti_done <display>`: the host
//!   input method's results for the IME ([`super::ime`]).

use super::ime;
use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const WINDOW_REQUEST: u16 = 0x0601;
    pub const WINDOW_ACTIVATE: u16 = 0x0602;
    pub const WINDOW_DESTROY: u16 = 0x0603;
    pub const TEXT_INPUT: u16 = 0x0604;
    pub const WINDOW_POLICY: u16 = 0x0605;
    pub const WINDOW_APPEARANCE: u16 = 0x0606;
    pub const WINDOW_MINIMIZE: u16 = 0x0607;
    pub const EVDEV: u16 = 0x0608;
    pub const POINTER_POSITION: u16 = 0x0609;

    /// Events (bridge to system app, tag 0).
    pub const INPUT: u16 = 0x4601;
    pub const WINDOW_MAPPED: u16 = 0x4602;
    pub const WINDOW_UNMAPPED: u16 = 0x4603;
}

/// Bounds on what the client may ask for.
pub mod limits {
    /// Display sizes in pixels, minimum sizes in dp.
    pub const MAX_DIM: u32 = 16384;
    /// Densities, in dpi (160 is scale 1).
    pub const MAX_DPI: u32 = 160 * 8;
    pub const MAX_ICON_SIDE: u16 = 256;
    pub const MAX_ICON_BLOB: usize = 4 + MAX_ICON_SIDE as usize * MAX_ICON_SIDE as usize * 4;
    /// Longest line in an INPUT event.
    pub const MAX_LINE: usize = 400;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowRequest {
    pub window: u32,
    /// The display's size, in pixels.
    pub width: u32,
    pub height: u32,
    pub dpi: u32,
    /// Minimum logical size (0: the bridge's default).
    pub min_width: u32,
    pub min_height: u32,
    pub app_id: String,
    pub title: String,
    /// An activation token for the new window (empty: none).
    pub token: String,
}

impl WindowRequest {
    /// The logical size the window starts at: the display's size in dp.
    pub fn logical_size(&self) -> (u32, u32) {
        let dp =
            |px: u32| ((px as u64 * 160 + self.dpi as u64 / 2) / self.dpi as u64).max(1) as u32;
        (dp(self.width), dp(self.height))
    }
}

/// Per-display Android requests. Orientation: 0 unspecified, 1 portrait, 2 landscape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowPolicy {
    pub window: u32,
    pub orientation: u32,
    pub fullscreen: bool,
    pub immersive: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    WindowRequest(WindowRequest),
    WindowActivate { window: u32, token: String },
    WindowDestroy { window: u32 },
    WindowMinimize { window: u32 },
    TextInput(ime::Req),
    WindowPolicy(WindowPolicy),
    WindowAppearance {
        window: u32,
        color: u32,
        len: u32,
    },
    PointerPosition {
        display: u32,
        x: i32,
        y: i32,
    },
    Evdev {
        device: u8,
        micros: i64,
        events: Vec<(u16, u16, i32)>,
    },
}

pub fn decode(ty: u16, body: &[u8]) -> Result<Request, DecodeError> {
    let mut r = Reader::new(body);
    let req = match ty {
        ty::WINDOW_REQUEST => Request::WindowRequest(WindowRequest {
            window: r.u32()?,
            width: r.u32_in(1..=limits::MAX_DIM, "width")?,
            height: r.u32_in(1..=limits::MAX_DIM, "height")?,
            dpi: r.u32_in(1..=limits::MAX_DPI, "dpi")?,
            min_width: r.u32_in(0..=limits::MAX_DIM, "minimum width")?,
            min_height: r.u32_in(0..=limits::MAX_DIM, "minimum height")?,
            app_id: r.name()?,
            title: r.name()?,
            token: r.name()?,
        }),
        ty::WINDOW_ACTIVATE => Request::WindowActivate {
            window: r.u32()?,
            token: r.name()?,
        },
        ty::WINDOW_MINIMIZE => Request::WindowMinimize { window: r.u32()? },
        ty::WINDOW_DESTROY => Request::WindowDestroy { window: r.u32()? },
        ty::WINDOW_APPEARANCE => Request::WindowAppearance {
            window: r.u32()?,
            color: r.u32()?,
            len: r.u32_in(4..=limits::MAX_ICON_BLOB as u32, "icon length")?,
        },
        ty::WINDOW_POLICY => Request::WindowPolicy(WindowPolicy {
            window: r.u32()?,
            orientation: r.u32_in(0..=2, "orientation")?,
            fullscreen: r.u32_in(0..=1, "fullscreen")? != 0,
            immersive: r.u32_in(0..=1, "immersive")? != 0,
        }),
        ty::POINTER_POSITION => Request::PointerPosition {
            display: r.u32()?,
            x: r.i32()?,
            y: r.i32()?,
        },
        ty::EVDEV => {
            let device = r.u32_in(0..=1, "evdev device")? as u8;
            let micros = r.u64()? as i64;
            let count = r.u32_in(1..=32, "evdev count")?;
            let mut events = Vec::new();
            for _ in 0..count {
                events.push((r.u16()?, r.u16()?, r.i32()?));
            }
            Request::Evdev {
                device,
                micros,
                events,
            }
        }
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
        Request::WindowRequest(s) => {
            w = Writer::message(ty::WINDOW_REQUEST, tag);
            w.u32(s.window)
                .u32(s.width)
                .u32(s.height)
                .u32(s.dpi)
                .u32(s.min_width)
                .u32(s.min_height)
                .name(&s.app_id)
                .name(&s.title)
                .name(&s.token);
        }
        Request::WindowActivate { window, token } => {
            w = Writer::message(ty::WINDOW_ACTIVATE, tag);
            w.u32(*window).name(token);
        }
        Request::WindowMinimize { window } => {
            w = Writer::message(ty::WINDOW_MINIMIZE, tag);
            w.u32(*window);
        }
        Request::WindowDestroy { window } => {
            w = Writer::message(ty::WINDOW_DESTROY, tag);
            w.u32(*window);
        }
        Request::WindowAppearance { window, color, len } => {
            w = Writer::message(ty::WINDOW_APPEARANCE, tag);
            w.u32(*window).u32(*color).u32(*len);
        }
        Request::WindowPolicy(p) => {
            w = Writer::message(ty::WINDOW_POLICY, tag);
            w.u32(p.window)
                .u32(p.orientation)
                .u32(u32::from(p.fullscreen))
                .u32(u32::from(p.immersive));
        }
        Request::PointerPosition { display, x, y } => {
            w = Writer::message(ty::POINTER_POSITION, tag);
            w.u32(*display).i32(*x).i32(*y);
        }
        Request::Evdev {
            device,
            micros,
            events,
        } => {
            w = Writer::message(ty::EVDEV, tag);
            w.u32(*device as u32)
                .u64(*micros as u64)
                .u32(events.len() as u32);
            for &(ty, code, value) in events {
                w.u16(ty).u16(code).i32(value);
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

    /// One input line, without '\n'.
    pub fn input(line: &str) -> Vec<u8> {
        let b = &line.as_bytes()[..line.len().min(limits::MAX_LINE)];
        let mut w = Writer::default();
        w.u16(b.len() as u16);
        w.0.extend_from_slice(b);
        w.into_vec()
    }
    pub fn window_mapped(window: u32) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(window);
        w.into_vec()
    }
    pub fn window_unmapped(window: u32) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(window);
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

    fn request(dpi: u32) -> WindowRequest {
        WindowRequest {
            window: 3,
            width: 2176,
            height: 1360,
            dpi,
            min_width: 240,
            min_height: 0,
            app_id: "dev.linuxapk.org.wikipedia".into(),
            title: "Wikipedia".into(),
            token: "kwin-12".into(),
        }
    }

    #[test]
    fn roundtrips() {
        for req in [
            Request::WindowRequest(request(272)),
            Request::WindowActivate {
                window: 3,
                token: String::new(),
            },
            Request::WindowDestroy { window: 3 },
            Request::WindowMinimize { window: 3 },
            Request::WindowAppearance {
                window: 3,
                color: 0xff1976d2,
                len: 8,
            },
            Request::TextInput(ime::Req::Surrounding {
                text: "日本語 text".into(),
                cursor: 3,
                anchor: 3,
                other: true,
            }),
            Request::TextInput(ime::Req::Apply),
            Request::WindowPolicy(WindowPolicy {
                window: 3,
                orientation: 1,
                fullscreen: true,
                immersive: true,
            }),
        ] {
            assert_eq!(roundtrip(&encode(1, &req)), Ok(req));
        }
    }

    #[test]
    fn rejects_bad_requests() {
        assert!(roundtrip(&encode(1, &Request::WindowRequest(request(0)))).is_err());
        let mut big = request(160);
        big.width = limits::MAX_DIM + 1;
        assert!(roundtrip(&encode(1, &Request::WindowRequest(big))).is_err());
        assert!(
            roundtrip(&encode(
                1,
                &Request::WindowPolicy(WindowPolicy {
                    window: 3,
                    orientation: 3,
                    fullscreen: false,
                    immersive: false,
                })
            ))
            .is_err()
        );
        assert_eq!(decode(0x06ff, &[]), Err(DecodeError::UnknownType(0x06ff)));
    }

    #[test]
    fn windows_start_at_the_display_size_in_dp() {
        assert_eq!(request(272).logical_size(), (1280, 800));
        assert_eq!(request(160).logical_size(), (2176, 1360));
        let tiny = WindowRequest {
            width: 1,
            height: 1,
            ..request(1280)
        };
        assert_eq!(tiny.logical_size(), (1, 1));
    }

    #[test]
    fn input_lines_are_bounded() {
        let long = "x".repeat(1000);
        assert_eq!(event::input(&long).len(), 2 + limits::MAX_LINE);
    }
}
