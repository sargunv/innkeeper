//! Input service messages (id 6, types 0x0600..=0x06ff, events
//! 0x4600..=0x46ff). The client is the Linux.apk System app: its window
//! manager asks for windows, its injector gets the seat, and its IME drives
//! the host's input methods. Only the system app's uid may open the service.
//!
//! | Type     | Request        | Body                                                                                                                                         | Reply                                                     |
//! | -------- | -------------- | -------------------------------------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
//! | `0x0601` | WINDOW_REQUEST | `u32 window` (the Android display id), `u32 width`, `u32 height` (display pixels), `u32 dpi`, `u32 min_width`, `u32 min_height` (dp, 0: 240x160), name `app_id`, name `title` | 0: queued; `-EBUSY`: the window is shown already |
//! | `0x0602` | WINDOW_ACTIVATE | `u32 window`                                                                                                                                | `-ENOENT` if not shown; else `xdg_activation_v1` with the newest input serial |
//! | `0x0603` | WINDOW_DESTROY | `u32 window`                                                                                                                                 | Drops a queued request, or closes the window now (its display may stay, headless) |
//! | `0x0604` | TEXT_INPUT     | `u16 len` + one `ti_*` line from the IME (≤ 480 bytes, [`super::ime`])                                                                       | Applied to the window with text-input focus               |
//!
//! | Event    | Message         | Body                                                                 |
//! | -------- | --------------- | -------------------------------------------------------------------- |
//! | `0x4601` | INPUT           | `u16 len` + one input line (≤ 400 bytes, below)                      |
//! | `0x4602` | WINDOW_MAPPED   | `u32 window`: a display shows in it                                  |
//! | `0x4603` | WINDOW_UNMAPPED | `u32 window`: its display went away, or it was destroyed               |
//!
//! **Windows.** A queued request waits up to 10 s for the composer to add an
//! HWC virtual display of exactly `width`×`height` (display service
//! `DISPLAY_ADD`); that display then shows in a new toplevel, which starts at
//! the display's size in dp. The window manager queues the request before it
//! gives its VirtualDisplay a surface, which is what makes SurfaceFlinger
//! create the HWC display.
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
}

impl WindowRequest {
    /// The logical size the window starts at: the display's size in dp.
    pub fn logical_size(&self) -> (u32, u32) {
        let dp =
            |px: u32| ((px as u64 * 160 + self.dpi as u64 / 2) / self.dpi as u64).max(1) as u32;
        (dp(self.width), dp(self.height))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    WindowRequest(WindowRequest),
    WindowActivate { window: u32 },
    WindowDestroy { window: u32 },
    TextInput(ime::Req),
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
        }),
        ty::WINDOW_ACTIVATE => Request::WindowActivate { window: r.u32()? },
        ty::WINDOW_DESTROY => Request::WindowDestroy { window: r.u32()? },
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
                .name(&s.title);
        }
        Request::WindowActivate { window } => {
            w = Writer::message(ty::WINDOW_ACTIVATE, tag);
            w.u32(*window);
        }
        Request::WindowDestroy { window } => {
            w = Writer::message(ty::WINDOW_DESTROY, tag);
            w.u32(*window);
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
    use proptest::prelude::*;

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
        }
    }

    #[test]
    fn roundtrips() {
        for req in [
            Request::WindowRequest(request(272)),
            Request::WindowActivate { window: 3 },
            Request::WindowDestroy { window: 3 },
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
    fn rejects_bad_requests() {
        assert!(roundtrip(&encode(1, &Request::WindowRequest(request(0)))).is_err());
        let mut big = request(160);
        big.width = limits::MAX_DIM + 1;
        assert!(roundtrip(&encode(1, &Request::WindowRequest(big))).is_err());
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

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20000))]

        // The decoder never panics, and anything it accepts re-encodes to
        // the same bytes.
        #[test]
        fn decode_arbitrary_bodies(t in 0x0600u16..0x0606, body in proptest::collection::vec(any::<u8>(), 0..300)) {
            if let Ok(req) = decode(t, &body) {
                prop_assert_eq!(&encode(9, &req)[HDR..], &body[..]);
            }
        }
    }
}
