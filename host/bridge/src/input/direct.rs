//! Input for the bridge's own windows (DISPLAY_WINDOW). Such a window shows a
//! guest that has no window manager on the input service, such as
//! OpenHarmony. Its seat lines ([`super::proto`]) become records for the
//! broker's mouse, touchscreen and keyboard, which the guest reads as kernel
//! devices.
//!
//! The pointer is the mouse. The guest moves its cursor by each report's
//! relative motion, one display pixel per unit. Each motion therefore sends
//! the difference between the pixel under the host pointer and the pixel
//! where the guest's cursor is. When the pointer enters a window, one large
//! negative motion first moves the guest's cursor into the top-left corner,
//! where the guest stops it, so that the two positions start out the same.
//! The guest drops motion that comes before it has opened the mouse and
//! learned its display, so the cursor goes through the corner again before
//! each press that starts a click.
//!
//! The wheel sends REL_WHEEL in degrees, one fifteenth of a detent, together
//! with REL_WHEEL_HI_RES. A touchpad's scroll sends degrees for its distance.
//!
//! The touchscreen's axes span [`touch::MAX`], and each window maps its
//! display's pixels onto that span. Each host touch is a contact of its own.
//! A contact stays on the window where it started, and it lifts when its
//! window goes away.
//!
//! Keys go to the keyboard while an own window has keyboard focus. They keep
//! their evdev codes, so the guest applies its own layout. The guest repeats
//! held keys itself, so the host's repeats are dropped. Held keys and
//! buttons are released when the window loses focus or goes away.

use std::collections::HashMap;

use innkeeper_evdev_proto::{KEYBOARD, MOUSE, TOUCHSCREEN, touch};

const EV_KEY: u16 = 1;
const EV_REL: u16 = 2;
const EV_ABS: u16 = 3;
const REL_X: u16 = 0;
const REL_Y: u16 = 1;
const REL_HWHEEL: u16 = 6;
const REL_WHEEL: u16 = 8;
const REL_WHEEL_HI_RES: u16 = 11;
const REL_HWHEEL_HI_RES: u16 = 12;
/// BTN_LEFT to BTN_TASK, the buttons the mouse reports.
const BUTTONS: std::ops::RangeInclusive<i64> = 0x110..=0x117;
const BTN_TOUCH: u16 = 0x14a;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
/// A motion that takes the guest's cursor past the top-left corner of any
/// display.
const PIN: i32 = -(1 << 15);
/// The degrees in one wheel detent, and the REL_WHEEL_HI_RES units in one
/// degree.
const DETENT: f64 = 15.0;
const HI_RES_PER_DEGREE: i32 = 8;
/// OpenHarmony scrolls a list by 63 vp for one detent. A touchpad's scroll
/// distance is in surface units, which are vp at the window's default size.
const VP_PER_DETENT: f64 = 63.0;

/// What [`Direct`] asks the broker for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// A device appears or goes away.
    Device(u32, bool),
    /// One report of `(type, code, value)` records. The writer adds the
    /// SYN_REPORT.
    Events(u32, Vec<(u16, u16, i32)>),
}

#[derive(Debug, Clone, Copy)]
struct Contact {
    id: i32,
    window: u32,
}

/// The scroll in one pointer frame.
#[derive(Default)]
struct Scroll {
    /// `pointer_axis_source`: 0 wheel, 1 finger, 2 continuous, 3 wheel tilt.
    source: Option<u32>,
    /// Vertical and horizontal, in surface units and in 120ths of a detent.
    value: [f64; 2],
    value120: [i32; 2],
}

#[derive(Default)]
pub struct Direct {
    /// Each own window's display size in pixels.
    windows: HashMap<u32, (u32, u32)>,
    /// The own window under the pointer.
    pointer: Option<u32>,
    /// The display pixel under the guest's cursor, or None until a motion
    /// moves the cursor into the corner first.
    cursor: Option<(i32, i32)>,
    buttons: Vec<u16>,
    scroll: Scroll,
    /// Each axis's scroll that is less than a degree and not yet sent.
    degrees: [f64; 2],
    slots: [Option<Contact>; touch::SLOTS],
    next_tracking_id: i32,
    /// The current touch frame's records.
    frame: Vec<(u16, u16, i32)>,
    /// The own window with keyboard focus, and the keys held there.
    keyboard: Option<u32>,
    keys: Vec<u16>,
}

impl Direct {
    /// Window `window` shows a display of `width`×`height` pixels. The first
    /// window brings the mouse and the touchscreen. A new size moves the
    /// guest's cursor into the corner again on the next motion.
    pub fn map(&mut self, window: u32, width: u32, height: u32) -> Vec<Out> {
        let first = self.windows.is_empty();
        let size = (width.max(1), height.max(1));
        if self.windows.insert(window, size).is_some_and(|s| s != size) {
            self.cursor = None;
        }
        if first {
            vec![Out::Device(MOUSE, true), Out::Device(TOUCHSCREEN, true)]
        } else {
            vec![]
        }
    }

    /// Window `window` went away. Its contacts lift, the buttons and keys
    /// held there are released, and the devices go with the last window.
    pub fn unmap(&mut self, window: u32) -> Vec<Out> {
        if self.windows.remove(&window).is_none() {
            return vec![];
        }
        let mut out = vec![];
        if self.pointer == Some(window) {
            out.extend(self.leave());
        }
        let mut records = std::mem::take(&mut self.frame);
        for slot in 0..touch::SLOTS {
            if self.slots[slot].is_some_and(|c| c.window == window) {
                records.extend(self.lift(slot));
            }
        }
        if !records.is_empty() {
            out.push(Out::Events(TOUCHSCREEN, records));
        }
        if self.keyboard == Some(window) {
            self.keyboard = None;
            out.extend(self.release_keys());
        }
        if self.windows.is_empty() {
            self.cursor = None;
            out.push(Out::Device(MOUSE, false));
            out.push(Out::Device(TOUCHSCREEN, false));
        }
        out
    }

    fn release_keys(&mut self) -> Vec<Out> {
        let records: Vec<_> = self.keys.drain(..).map(|k| (EV_KEY, k, 0)).collect();
        if records.is_empty() {
            vec![]
        } else {
            vec![Out::Events(KEYBOARD, records)]
        }
    }

    /// A `key` line's evdev code and state: 0 release, 1 press, 2 repeat.
    fn key(&mut self, code: Option<i64>, state: Option<i64>) -> Vec<Out> {
        let (Some(_), Some(code), Some(state)) = (self.keyboard, code, state) else {
            return vec![];
        };
        let Ok(code) = u16::try_from(code) else {
            return vec![];
        };
        let held = self.keys.contains(&code);
        let value = match state {
            1 if !held => {
                self.keys.push(code);
                1
            }
            0 if held => {
                self.keys.retain(|&k| k != code);
                0
            }
            _ => return vec![],
        };
        vec![Out::Events(KEYBOARD, vec![(EV_KEY, code, value)])]
    }

    /// Moves the guest's cursor to the pixel at display position `(x, y)`.
    fn motion(&mut self, x: f64, y: f64) -> Vec<Out> {
        let Some((w, h)) = self.pointer.and_then(|p| self.windows.get(&p).copied()) else {
            return vec![];
        };
        let pixel = |v: f64, size: u32| (v.floor() as i32).clamp(0, size as i32 - 1);
        let to = (pixel(x, w), pixel(y, h));
        let mut out = vec![];
        let from = self.cursor.unwrap_or_else(|| {
            out.push(Out::Events(
                MOUSE,
                vec![(EV_REL, REL_X, PIN), (EV_REL, REL_Y, PIN)],
            ));
            (0, 0)
        });
        out.extend(self.relative(from, to));
        self.cursor = Some(to);
        out
    }

    /// The motion from pixel `from` to pixel `to`, if they differ.
    fn relative(&self, from: (i32, i32), to: (i32, i32)) -> Option<Out> {
        let records: Vec<_> = [(REL_X, to.0 - from.0), (REL_Y, to.1 - from.1)]
            .into_iter()
            .filter(|&(_, d)| d != 0)
            .map(|(code, d)| (EV_REL, code, d))
            .collect();
        (!records.is_empty()).then_some(Out::Events(MOUSE, records))
    }

    /// The pointer leaves its window, and the buttons held there are
    /// released. The guest's cursor stays where it is.
    fn leave(&mut self) -> Vec<Out> {
        self.pointer = None;
        self.scroll = Scroll::default();
        self.degrees = [0.0; 2];
        let records: Vec<_> = self.buttons.drain(..).map(|b| (EV_KEY, b, 0)).collect();
        if records.is_empty() {
            vec![]
        } else {
            vec![Out::Events(MOUSE, records)]
        }
    }

    fn button(&mut self, code: Option<i64>, state: Option<i64>) -> Vec<Out> {
        let (Some(_), Some(code), Some(state)) = (self.pointer, code, state) else {
            return vec![];
        };
        if !BUTTONS.contains(&code) {
            return vec![];
        }
        let code = code as u16;
        let held = self.buttons.contains(&code);
        let mut out = vec![];
        match state {
            1 if !held => {
                if self.buttons.is_empty()
                    && let Some(at) = self.cursor
                {
                    out.push(Out::Events(
                        MOUSE,
                        vec![(EV_REL, REL_X, PIN), (EV_REL, REL_Y, PIN)],
                    ));
                    out.extend(self.relative((0, 0), at));
                }
                self.buttons.push(code)
            }
            0 if held => self.buttons.retain(|&b| b != code),
            _ => return vec![],
        }
        out.push(Out::Events(MOUSE, vec![(EV_KEY, code, state as i32)]));
        out
    }

    /// Sends the frame's scroll. A wheel's detents count in 120ths, and other
    /// sources count in surface units.
    fn scroll(&mut self) -> Vec<Out> {
        let scroll = std::mem::take(&mut self.scroll);
        if self.pointer.is_none() {
            return vec![];
        }
        let wheel = matches!(scroll.source, None | Some(0 | 3));
        let mut records = vec![];
        // Wayland's axes point down and right. REL_WHEEL points up.
        for (axis, (lo, hi, sign)) in [
            (REL_WHEEL, REL_WHEEL_HI_RES, -1),
            (REL_HWHEEL, REL_HWHEEL_HI_RES, 1),
        ]
        .into_iter()
        .enumerate()
        {
            let degrees = if wheel && scroll.value120[axis] != 0 {
                f64::from(scroll.value120[axis]) * DETENT / 120.0
            } else if wheel {
                scroll.value[axis]
            } else {
                scroll.value[axis] * DETENT / VP_PER_DETENT
            };
            self.degrees[axis] += degrees;
            let whole = self.degrees[axis].trunc();
            self.degrees[axis] -= whole;
            let n = sign * whole as i32;
            if n != 0 {
                records.push((EV_REL, lo, n));
                records.push((EV_REL, hi, n * HI_RES_PER_DEGREE));
            }
        }
        if records.is_empty() {
            vec![]
        } else {
            vec![Out::Events(MOUSE, records)]
        }
    }

    /// Translates one seat line. Lines for other windows give nothing.
    pub fn line(&mut self, line: &str) -> Vec<Out> {
        let mut words = line.split(' ');
        let kind = words.next().unwrap_or("");
        let args: Vec<&str> = words.collect();
        let num = |i: usize| args.get(i).and_then(|a| a.parse::<f64>().ok());
        let int = |i: usize| args.get(i).and_then(|a| a.parse::<i64>().ok());
        let records = match kind {
            "keyboard_enter" => {
                self.keyboard = int(0)
                    .map(|w| w as u32)
                    .filter(|w| self.windows.contains_key(w));
                vec![]
            }
            "keyboard_leave" => {
                self.keyboard = None;
                return self.release_keys();
            }
            "key" => return self.key(int(1), int(2)),
            "pointer_enter" => {
                let mut out = self.leave();
                if let (Some(x), Some(y), Some(w)) = (num(0), num(1), int(2))
                    && self.windows.contains_key(&(w as u32))
                {
                    self.pointer = Some(w as u32);
                    self.cursor = None;
                    out.extend(self.motion(x, y));
                }
                return out;
            }
            "pointer_leave" => return self.leave(),
            "pointer_motion" => {
                return match (num(1), num(2)) {
                    (Some(x), Some(y)) => self.motion(x, y),
                    _ => vec![],
                };
            }
            "pointer_button" => return self.button(int(1), int(2)),
            "pointer_axis_source" => {
                self.scroll.source = int(0).map(|s| s as u32);
                vec![]
            }
            "pointer_axis" => {
                if let (Some(a @ 0..=1), Some(v)) = (int(1), num(2)) {
                    self.scroll.value[a as usize] += v;
                }
                vec![]
            }
            "pointer_axis_value120" => {
                if let (Some(a @ 0..=1), Some(v)) = (int(0), int(1)) {
                    self.scroll.value120[a as usize] += v as i32;
                }
                vec![]
            }
            "pointer_frame" => return self.scroll(),
            "touch_down" => match (int(1), num(2), num(3), int(4)) {
                (Some(id), Some(x), Some(y), Some(w))
                    if self.windows.contains_key(&(w as u32)) && self.find(id as i32).is_none() =>
                {
                    let records = self.down(id as i32, w as u32, x, y);
                    self.frame.extend(records);
                    vec![]
                }
                _ => vec![],
            },
            "touch_motion" => match (int(1), num(2), num(3)) {
                (Some(id), Some(x), Some(y)) => {
                    if let Some(slot) = self.find(id as i32) {
                        let records = self.moved(slot, x, y);
                        self.frame.extend(records);
                    }
                    vec![]
                }
                _ => vec![],
            },
            "touch_up" => {
                if let Some(slot) = int(1).and_then(|id| self.find(id as i32)) {
                    let records = self.lift(slot);
                    self.frame.extend(records);
                }
                vec![]
            }
            "touch_cancel" => {
                for slot in 0..touch::SLOTS {
                    let records = self.lift(slot);
                    self.frame.extend(records);
                }
                std::mem::take(&mut self.frame)
            }
            "touch_frame" => std::mem::take(&mut self.frame),
            _ => vec![],
        };
        if records.is_empty() {
            vec![]
        } else {
            vec![Out::Events(TOUCHSCREEN, records)]
        }
    }

    fn find(&self, id: i32) -> Option<usize> {
        self.slots
            .iter()
            .position(|c| c.is_some_and(|c| c.id == id))
    }

    /// The axis value of display pixel `x` on a display `size` pixels long.
    fn axis(x: f64, size: u32) -> i32 {
        let span = f64::from(touch::MAX) + 1.0;
        ((x / f64::from(size) * span).round() as i32).clamp(0, touch::MAX)
    }

    fn position(&self, window: u32, x: f64, y: f64) -> [(u16, u16, i32); 2] {
        let (w, h) = self.windows.get(&window).copied().unwrap_or((1, 1));
        [
            (EV_ABS, ABS_MT_POSITION_X, Self::axis(x, w)),
            (EV_ABS, ABS_MT_POSITION_Y, Self::axis(y, h)),
        ]
    }

    /// A new contact in the first free slot. A contact beyond the last slot
    /// is dropped.
    fn down(&mut self, id: i32, window: u32, x: f64, y: f64) -> Vec<(u16, u16, i32)> {
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return vec![];
        };
        let first = self.slots.iter().all(Option::is_none);
        self.slots[slot] = Some(Contact { id, window });
        let tracking = self.next_tracking_id;
        self.next_tracking_id = (tracking + 1) & 0xffff;
        let mut r = vec![
            (EV_ABS, ABS_MT_SLOT, slot as i32),
            (EV_ABS, ABS_MT_TRACKING_ID, tracking),
        ];
        r.extend(self.position(window, x, y));
        if first {
            r.push((EV_KEY, BTN_TOUCH, 1));
        }
        r
    }

    fn moved(&self, slot: usize, x: f64, y: f64) -> Vec<(u16, u16, i32)> {
        let Some(c) = self.slots[slot] else {
            return vec![];
        };
        let mut r = vec![(EV_ABS, ABS_MT_SLOT, slot as i32)];
        r.extend(self.position(c.window, x, y));
        r
    }

    fn lift(&mut self, slot: usize) -> Vec<(u16, u16, i32)> {
        if self.slots[slot].take().is_none() {
            return vec![];
        }
        let mut r = vec![
            (EV_ABS, ABS_MT_SLOT, slot as i32),
            (EV_ABS, ABS_MT_TRACKING_ID, -1),
        ];
        if self.slots.iter().all(Option::is_none) {
            r.push((EV_KEY, BTN_TOUCH, 0));
        }
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: u32 = 1 << 31 | 1;

    fn screen() -> Direct {
        let mut d = Direct::default();
        assert_eq!(
            d.map(W, 720, 1280),
            [Out::Device(MOUSE, true), Out::Device(TOUCHSCREEN, true)]
        );
        d
    }

    fn mouse(out: Vec<Out>) -> Vec<Vec<(u16, u16, i32)>> {
        out.into_iter()
            .map(|o| match o {
                Out::Events(MOUSE, r) => r,
                o => panic!("{o:?}"),
            })
            .collect()
    }

    const CORNER: [(u16, u16, i32); 2] = [(EV_REL, REL_X, PIN), (EV_REL, REL_Y, PIN)];

    #[test]
    fn entering_moves_the_cursor_from_the_corner_to_the_pointer() {
        let mut d = screen();
        assert_eq!(
            mouse(d.line(&format!("pointer_enter 360.00 640.75 {W}"))),
            [
                CORNER.to_vec(),
                vec![(EV_REL, REL_X, 360), (EV_REL, REL_Y, 640)]
            ]
        );
        assert_eq!(
            mouse(d.line("pointer_motion 1 100.99 640.20")),
            [vec![(EV_REL, REL_X, -260)]]
        );
        // The same pixel needs no motion.
        assert!(d.line("pointer_motion 2 100.10 640.90").is_empty());
        assert_eq!(
            mouse(d.line("pointer_motion 3 101.00 639.99")),
            [vec![(EV_REL, REL_X, 1), (EV_REL, REL_Y, -1)]]
        );
        assert!(d.line(&format!("pointer_leave {W}")).is_empty());
        assert!(d.line("pointer_motion 4 5.00 5.00").is_empty());
        // Each enter starts from the corner again.
        assert_eq!(
            mouse(d.line(&format!("pointer_enter 0.00 0.00 {W}"))),
            [CORNER.to_vec()]
        );
    }

    #[test]
    fn motion_outside_the_display_stops_at_its_edge() {
        let mut d = screen();
        d.line(&format!("pointer_enter 700.00 10.00 {W}"));
        assert_eq!(
            mouse(d.line("pointer_motion 1 900.00 -30.00")),
            [vec![(EV_REL, REL_X, 19), (EV_REL, REL_Y, -10)]]
        );
        assert!(d.line("pointer_motion 2 2000.00 -1.00").is_empty());
    }

    /// The guest keeps its cursor as a fraction. It stops the whole pixels at
    /// the display's edges, keeps the fraction, and shows the cursor at the
    /// pixel the position truncates to.
    struct Guest(f64, f64);
    impl Guest {
        fn apply(&mut self, out: Vec<Out>) {
            for records in mouse(out) {
                for (ty, code, v) in records {
                    if ty == EV_KEY {
                        continue;
                    }
                    assert_eq!(ty, EV_REL);
                    let (p, size) = match code {
                        REL_X => (&mut self.0, 720),
                        REL_Y => (&mut self.1, 1280),
                        _ => panic!("{code}"),
                    };
                    let moved = *p + f64::from(v);
                    let whole = (moved as i32).clamp(0, size - 1);
                    *p = f64::from(whole) + (moved - moved.floor());
                }
            }
        }
        fn pixel(&self) -> (i32, i32) {
            (self.0 as i32, self.1 as i32)
        }
    }

    #[test]
    fn the_cursor_stays_on_the_pointers_pixel_from_any_start() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut next = |n: f64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % 10_000) as f64 / 10_000.0 * n
        };
        for start in [(0.0, 0.0), (359.5, 640.25), (719.9, 1279.0)] {
            let mut d = screen();
            let mut guest = Guest(start.0, start.1);
            let (mut x, mut y) = (next(720.0), next(1280.0));
            guest.apply(d.line(&format!("pointer_enter {x:.2} {y:.2} {W}")));
            for i in 0..2000 {
                x = (x + next(400.0) - 200.0).clamp(-50.0, 770.0);
                y = (y + next(400.0) - 200.0).clamp(-50.0, 1330.0);
                guest.apply(d.line(&format!("pointer_motion {i} {x:.2} {y:.2}")));
                let host = (
                    (format!("{x:.2}").parse::<f64>().unwrap().floor() as i32).clamp(0, 719),
                    (format!("{y:.2}").parse::<f64>().unwrap().floor() as i32).clamp(0, 1279),
                );
                assert_eq!(guest.pixel(), host, "move {i}");
            }
        }
    }

    #[test]
    fn a_click_places_a_cursor_that_missed_earlier_motion() {
        let mut d = screen();
        // The guest isn't ready yet and drops the enter's motion.
        d.line(&format!("pointer_enter 200.00 14.00 {W}"));
        let mut guest = Guest(360.0, 640.0);
        guest.apply(d.line("pointer_motion 1 360.50 600.50"));
        assert_eq!(guest.pixel(), (520, 1226));
        guest.apply(d.line("pointer_button 2 272 1"));
        assert_eq!(guest.pixel(), (360, 600));
        // A press while another button is held doesn't move the cursor.
        guest.0 = 0.0;
        guest.apply(d.line("pointer_button 3 273 1"));
        assert_eq!(guest.pixel(), (0, 600));
    }

    #[test]
    fn a_new_display_size_moves_from_the_corner_on_the_next_motion() {
        let mut d = screen();
        d.line(&format!("pointer_enter 10.00 10.00 {W}"));
        assert!(d.map(W, 720, 1280).is_empty());
        assert_eq!(
            mouse(d.line("pointer_motion 1 11.00 10.00")),
            [vec![(EV_REL, REL_X, 1)]]
        );
        assert!(d.map(W, 1080, 1920).is_empty());
        assert_eq!(
            mouse(d.line("pointer_motion 2 11.00 10.00")),
            [
                CORNER.to_vec(),
                vec![(EV_REL, REL_X, 11), (EV_REL, REL_Y, 10)]
            ]
        );
    }

    #[test]
    fn buttons_press_and_release_on_the_mouse() {
        let mut d = screen();
        assert!(d.line("pointer_button 1 272 1").is_empty());
        d.line(&format!("pointer_enter 10.00 0.00 {W}"));
        // A press that starts a click moves through the corner first.
        assert_eq!(
            mouse(d.line("pointer_button 2 272 1")),
            [
                CORNER.to_vec(),
                vec![(EV_REL, REL_X, 10)],
                vec![(EV_KEY, 0x110, 1)]
            ]
        );
        for (line, records) in [
            ("pointer_button 3 273 1", vec![(EV_KEY, 0x111, 1)]),
            ("pointer_button 4 274 1", vec![(EV_KEY, 0x112, 1)]),
            ("pointer_button 5 273 0", vec![(EV_KEY, 0x111, 0)]),
        ] {
            assert_eq!(mouse(d.line(line)), [records], "{line}");
        }
        // Repeats, releases of unheld buttons and other codes send nothing.
        for line in [
            "pointer_button 6 272 1",
            "pointer_button 7 273 0",
            "pointer_button 8 330 1",
            "pointer_button 9 272",
        ] {
            assert!(d.line(line).is_empty(), "{line}");
        }
        assert_eq!(
            mouse(d.line(&format!("pointer_leave {W}"))),
            [vec![(EV_KEY, 0x110, 0), (EV_KEY, 0x112, 0)]]
        );
        assert!(d.line("pointer_button 10 272 0").is_empty());
        d.line(&format!("pointer_enter 0.00 0.00 {W}"));
        assert_eq!(
            mouse(d.line("pointer_button 11 273 1")),
            [CORNER.to_vec(), vec![(EV_KEY, 0x111, 1)]]
        );
    }

    #[test]
    fn other_windows_get_nothing() {
        let mut d = screen();
        for line in [
            "pointer_enter 10.00 10.00 3",
            "pointer_motion 1 20.00 20.00",
            "pointer_button 1 272 1",
            "pointer_axis_source 0",
            "pointer_axis 1 0 15.000",
            "pointer_axis_value120 0 120",
            "pointer_frame",
            "touch_down 1 0 10.00 10.00 3",
            "touch_frame",
            "pointer_enter 10.00",
            "touch_down 1 0 x 10.00 3",
        ] {
            assert!(d.line(line).is_empty(), "{line}");
        }
    }

    fn scroll(d: &mut Direct, lines: &[&str]) -> Vec<Vec<(u16, u16, i32)>> {
        for l in lines {
            assert!(d.line(l).is_empty(), "{l}");
        }
        mouse(d.line("pointer_frame"))
    }

    #[test]
    fn a_wheel_detent_is_fifteen_degrees_and_fractions_add_up() {
        let mut d = screen();
        d.line(&format!("pointer_enter 10.00 10.00 {W}"));
        assert_eq!(
            scroll(
                &mut d,
                &[
                    "pointer_axis_source 0",
                    "pointer_axis 1 0 15.000",
                    "pointer_axis_value120 0 120"
                ]
            ),
            [vec![
                (EV_REL, REL_WHEEL, -15),
                (EV_REL, REL_WHEEL_HI_RES, -120)
            ]]
        );
        let half = [
            "pointer_axis_source 0",
            "pointer_axis 2 1 -7.500",
            "pointer_axis_value120 1 -60",
        ];
        assert_eq!(
            scroll(&mut d, &half),
            [vec![
                (EV_REL, REL_HWHEEL, -7),
                (EV_REL, REL_HWHEEL_HI_RES, -56)
            ]]
        );
        assert_eq!(
            scroll(&mut d, &half),
            [vec![
                (EV_REL, REL_HWHEEL, -8),
                (EV_REL, REL_HWHEEL_HI_RES, -64)
            ]]
        );
        // A wheel without detent counts reports degrees.
        assert_eq!(
            scroll(&mut d, &["pointer_axis 3 0 -2.000"]),
            [vec![(EV_REL, REL_WHEEL, 2), (EV_REL, REL_WHEEL_HI_RES, 16)]]
        );
    }

    #[test]
    fn touchpad_scrolling_sends_degrees_for_its_distance() {
        let mut d = screen();
        d.line(&format!("pointer_enter 10.00 10.00 {W}"));
        assert_eq!(
            scroll(
                &mut d,
                &["pointer_axis_source 1", "pointer_axis 1 0 42.000"]
            ),
            [vec![
                (EV_REL, REL_WHEEL, -10),
                (EV_REL, REL_WHEEL_HI_RES, -80)
            ]]
        );
        let small = ["pointer_axis_source 1", "pointer_axis 2 0 -2.100"];
        assert!(scroll(&mut d, &small).is_empty());
        assert_eq!(
            scroll(&mut d, &small),
            [vec![(EV_REL, REL_WHEEL, 1), (EV_REL, REL_WHEEL_HI_RES, 8)]]
        );
        assert!(
            scroll(
                &mut d,
                &[
                    "pointer_axis_source 1",
                    "pointer_axis_stop 3 0",
                    "pointer_axis 4 0 2.000"
                ]
            )
            .is_empty()
        );
        // A leave drops the part of a degree that is left.
        d.line(&format!("pointer_leave {W}"));
        d.line(&format!("pointer_enter 10.00 10.00 {W}"));
        assert!(scroll(&mut d, &["pointer_axis_source 1", "pointer_axis 5 0 3.000"]).is_empty());
    }

    fn touches(out: Vec<Out>) -> Vec<(u16, u16, i32)> {
        match out.as_slice() {
            [Out::Events(TOUCHSCREEN, r)] => r.clone(),
            [] => vec![],
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn touches_are_contacts_reported_per_frame() {
        let mut d = screen();
        assert!(d.line(&format!("touch_down 1 7 0.00 0.00 {W}")).is_empty());
        assert!(
            d.line(&format!("touch_down 1 9 720.00 640.00 {W}"))
                .is_empty()
        );
        assert_eq!(
            touches(d.line("touch_frame")),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 0),
                (EV_ABS, ABS_MT_POSITION_X, 0),
                (EV_ABS, ABS_MT_POSITION_Y, 0),
                (EV_KEY, BTN_TOUCH, 1),
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, 1),
                (EV_ABS, ABS_MT_POSITION_X, touch::MAX),
                (EV_ABS, ABS_MT_POSITION_Y, 16384),
            ]
        );
        d.line("touch_up 2 7");
        d.line("touch_motion 2 9 360.00 640.00");
        d.line("touch_motion 2 8 0.00 0.00");
        assert_eq!(
            touches(d.line("touch_frame")),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_POSITION_X, 16384),
                (EV_ABS, ABS_MT_POSITION_Y, 16384),
            ]
        );
        assert_eq!(
            touches(d.line("touch_cancel")),
            [
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_KEY, BTN_TOUCH, 0),
            ]
        );
        assert!(d.line("touch_frame").is_empty());
    }

    #[test]
    fn contacts_beyond_the_last_slot_are_dropped() {
        let mut d = screen();
        for id in 0..=touch::SLOTS {
            d.line(&format!("touch_down 1 {id} 1.00 1.00 {W}"));
        }
        let down = touches(d.line("touch_frame"));
        assert_eq!(
            down.iter().filter(|r| r.1 == ABS_MT_TRACKING_ID).count(),
            touch::SLOTS
        );
        d.line("touch_up 2 4");
        d.line(&format!("touch_down 3 20 1.00 1.00 {W}"));
        let down = touches(d.line("touch_frame"));
        assert_eq!(down[2], (EV_ABS, ABS_MT_SLOT, 4));
        assert!(!down.contains(&(EV_KEY, BTN_TOUCH, 1)));
    }

    #[test]
    fn a_closed_window_lifts_its_contacts_and_the_last_takes_the_devices() {
        let mut d = screen();
        let other = W + 1;
        assert!(d.map(other, 100, 100).is_empty());
        d.line(&format!("touch_down 1 0 1.00 1.00 {W}"));
        d.line(&format!("touch_down 1 1 1.00 1.00 {other}"));
        d.line("touch_frame");
        d.line(&format!("pointer_enter 1.00 1.00 {W}"));
        d.line("pointer_button 2 272 1");
        assert_eq!(
            d.unmap(W),
            [
                Out::Events(MOUSE, vec![(EV_KEY, 0x110, 0)]),
                Out::Events(
                    TOUCHSCREEN,
                    vec![(EV_ABS, ABS_MT_SLOT, 0), (EV_ABS, ABS_MT_TRACKING_ID, -1)]
                ),
            ]
        );
        assert!(d.line("touch_motion 2 0 5.00 5.00").is_empty());
        assert!(d.line("pointer_motion 3 5.00 5.00").is_empty());
        assert!(d.unmap(W).is_empty());
        assert_eq!(
            d.unmap(other),
            [
                Out::Events(
                    TOUCHSCREEN,
                    vec![
                        (EV_ABS, ABS_MT_SLOT, 1),
                        (EV_ABS, ABS_MT_TRACKING_ID, -1),
                        (EV_KEY, BTN_TOUCH, 0),
                    ]
                ),
                Out::Device(MOUSE, false),
                Out::Device(TOUCHSCREEN, false),
            ]
        );
    }

    fn keys(out: Vec<Out>) -> Vec<(u16, u16, i32)> {
        match out.as_slice() {
            [Out::Events(KEYBOARD, r)] => r.clone(),
            [] => vec![],
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn keys_reach_the_keyboard_while_an_own_window_has_focus() {
        let mut d = screen();
        assert!(d.line("key 1 30 1 97 text a").is_empty());
        assert!(d.line(&format!("keyboard_enter {W}")).is_empty());
        assert_eq!(keys(d.line("key 2 42 1")), [(EV_KEY, 42, 1)]);
        assert_eq!(keys(d.line("key 3 30 1 65 text A")), [(EV_KEY, 30, 1)]);
        // The guest repeats keys itself.
        assert!(d.line("key 4 30 2 65 text A").is_empty());
        assert!(d.line("key 5 30 1 65 text A").is_empty());
        assert_eq!(keys(d.line("key 6 30 0")), [(EV_KEY, 30, 0)]);
        assert!(d.line("key 7 30 0").is_empty());
        assert!(d.line("modifiers 1 0 0 0").is_empty());
        assert_eq!(
            keys(d.line(&format!("keyboard_leave {W}"))),
            [(EV_KEY, 42, 0)]
        );
        assert!(d.line("key 8 30 1").is_empty());
        assert!(d.line(&format!("keyboard_leave {W}")).is_empty());
    }

    #[test]
    fn other_windows_and_bad_keys_type_nothing() {
        let mut d = screen();
        d.line("keyboard_enter 3");
        assert!(d.line("key 1 30 1").is_empty());
        d.line(&format!("keyboard_enter {W}"));
        assert!(d.line("key 1 -1 1").is_empty());
        assert!(d.line("key 1 70000 1").is_empty());
        assert!(d.line("key 1 30").is_empty());
    }

    #[test]
    fn a_closed_window_releases_its_keys() {
        let mut d = screen();
        d.line(&format!("keyboard_enter {W}"));
        d.line("key 1 29 1");
        assert_eq!(
            d.unmap(W),
            [
                Out::Events(KEYBOARD, vec![(EV_KEY, 29, 0)]),
                Out::Device(MOUSE, false),
                Out::Device(TOUCHSCREEN, false),
            ]
        );
    }

    #[test]
    fn axes_cover_the_display() {
        assert_eq!(Direct::axis(-5.0, 720), 0);
        assert_eq!(Direct::axis(719.99, 720), touch::MAX);
        assert_eq!(Direct::axis(900.0, 720), touch::MAX);
        // libinput maps the axis back as value × width / (MAX + 1).
        for px in [0, 1, 123, 359, 718] {
            let back = f64::from(Direct::axis(f64::from(px), 720)) * 720.0 / 32768.0;
            assert!((back - f64::from(px)).abs() < 0.1, "{px} -> {back}");
            assert_eq!(back.round() as i32, px);
        }
    }
}
