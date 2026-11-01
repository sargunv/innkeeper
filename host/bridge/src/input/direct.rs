//! Input for the bridge's own windows (DISPLAY_WINDOW). Such a window shows a
//! guest that has no window manager on the input service, such as
//! OpenHarmony. Its seat lines ([`super::proto`]) become records for the
//! broker's touchscreen, which the guest reads as a kernel device.
//!
//! The touchscreen's axes span [`touch::MAX`], and each window maps its
//! display's pixels onto that span. The pointer's left button touches the
//! screen, so a click is a tap and a drag is a swipe. Each host touch is a
//! contact of its own. A contact stays on the window where it started, and
//! it lifts when its window goes away.

use std::collections::HashMap;

use innkeeper_evdev_proto::{TOUCHSCREEN, touch};

const EV_KEY: u16 = 1;
const EV_ABS: u16 = 3;
const BTN_LEFT: u32 = 0x110;
const BTN_TOUCH: u16 = 0x14a;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;

/// What [`Direct`] asks the broker for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Out {
    /// A device appears or goes away.
    Device(u32, bool),
    /// One report of `(type, code, value)` records. The writer adds the
    /// SYN_REPORT.
    Events(u32, Vec<(u16, u16, i32)>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Pointer,
    Touch(i32),
}

#[derive(Debug, Clone, Copy)]
struct Contact {
    source: Source,
    window: u32,
}

#[derive(Default)]
pub struct Direct {
    /// Each own window's display size in pixels.
    windows: HashMap<u32, (u32, u32)>,
    /// The own window under the pointer, and the pointer's position there.
    pointer: Option<(u32, f64, f64)>,
    slots: [Option<Contact>; touch::SLOTS],
    next_tracking_id: i32,
    /// The current touch frame's records.
    frame: Vec<(u16, u16, i32)>,
}

impl Direct {
    /// Window `window` shows a display of `width`×`height` pixels. The first
    /// window brings the touchscreen.
    pub fn map(&mut self, window: u32, width: u32, height: u32) -> Vec<Out> {
        let first = self.windows.is_empty();
        self.windows.insert(window, (width.max(1), height.max(1)));
        if first {
            vec![Out::Device(TOUCHSCREEN, true)]
        } else {
            vec![]
        }
    }

    /// Window `window` went away. Its contacts lift, and the touchscreen
    /// goes with the last window.
    pub fn unmap(&mut self, window: u32) -> Vec<Out> {
        if self.windows.remove(&window).is_none() {
            return vec![];
        }
        if self.pointer.is_some_and(|(w, ..)| w == window) {
            self.pointer = None;
        }
        let mut records = std::mem::take(&mut self.frame);
        for slot in 0..touch::SLOTS {
            if self.slots[slot].is_some_and(|c| c.window == window) {
                records.extend(self.lift(slot));
            }
        }
        let mut out = vec![];
        if !records.is_empty() {
            out.push(Out::Events(TOUCHSCREEN, records));
        }
        if self.windows.is_empty() {
            out.push(Out::Device(TOUCHSCREEN, false));
        }
        out
    }

    /// Translates one seat line. Lines for other windows give nothing.
    pub fn line(&mut self, line: &str) -> Vec<Out> {
        let mut words = line.split(' ');
        let kind = words.next().unwrap_or("");
        let args: Vec<&str> = words.collect();
        let num = |i: usize| args.get(i).and_then(|a| a.parse::<f64>().ok());
        let int = |i: usize| args.get(i).and_then(|a| a.parse::<i64>().ok());
        let records = match kind {
            "pointer_enter" => {
                self.pointer = match (num(0), num(1), int(2)) {
                    (Some(x), Some(y), Some(w)) if self.windows.contains_key(&(w as u32)) => {
                        Some((w as u32, x, y))
                    }
                    _ => None,
                };
                vec![]
            }
            "pointer_leave" => {
                self.pointer = None;
                self.find(Source::Pointer)
                    .map(|slot| self.lift(slot))
                    .unwrap_or_default()
            }
            "pointer_motion" => match (self.pointer.as_mut(), num(1), num(2)) {
                (Some(p), Some(x), Some(y)) => {
                    (p.1, p.2) = (x, y);
                    match self.find(Source::Pointer) {
                        Some(slot) => self.moved(slot, x, y),
                        None => vec![],
                    }
                }
                _ => vec![],
            },
            "pointer_button" => match (self.pointer, int(1), int(2)) {
                (Some((window, x, y)), Some(b), Some(1)) if b == i64::from(BTN_LEFT) => {
                    match self.find(Source::Pointer) {
                        Some(_) => vec![],
                        None => self.down(Source::Pointer, window, x, y),
                    }
                }
                (_, Some(b), Some(0)) if b == i64::from(BTN_LEFT) => self
                    .find(Source::Pointer)
                    .map(|slot| self.lift(slot))
                    .unwrap_or_default(),
                _ => vec![],
            },
            "touch_down" => match (int(1), num(2), num(3), int(4)) {
                (Some(id), Some(x), Some(y), Some(w))
                    if self.windows.contains_key(&(w as u32))
                        && self.find(Source::Touch(id as i32)).is_none() =>
                {
                    let records = self.down(Source::Touch(id as i32), w as u32, x, y);
                    self.frame.extend(records);
                    vec![]
                }
                _ => vec![],
            },
            "touch_motion" => match (int(1), num(2), num(3)) {
                (Some(id), Some(x), Some(y)) => {
                    if let Some(slot) = self.find(Source::Touch(id as i32)) {
                        let records = self.moved(slot, x, y);
                        self.frame.extend(records);
                    }
                    vec![]
                }
                _ => vec![],
            },
            "touch_up" => {
                if let Some(slot) = int(1).and_then(|id| self.find(Source::Touch(id as i32))) {
                    let records = self.lift(slot);
                    self.frame.extend(records);
                }
                vec![]
            }
            "touch_cancel" => {
                for slot in 0..touch::SLOTS {
                    if matches!(self.slots[slot], Some(c) if matches!(c.source, Source::Touch(_))) {
                        let records = self.lift(slot);
                        self.frame.extend(records);
                    }
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

    fn find(&self, source: Source) -> Option<usize> {
        self.slots
            .iter()
            .position(|c| c.is_some_and(|c| c.source == source))
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
    fn down(&mut self, source: Source, window: u32, x: f64, y: f64) -> Vec<(u16, u16, i32)> {
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return vec![];
        };
        let first = self.slots.iter().all(Option::is_none);
        self.slots[slot] = Some(Contact { source, window });
        let id = self.next_tracking_id;
        self.next_tracking_id = (id + 1) & 0xffff;
        let mut r = vec![
            (EV_ABS, ABS_MT_SLOT, slot as i32),
            (EV_ABS, ABS_MT_TRACKING_ID, id),
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
        assert_eq!(d.map(W, 720, 1280), [Out::Device(TOUCHSCREEN, true)]);
        d
    }

    fn records(out: Vec<Out>) -> Vec<(u16, u16, i32)> {
        match out.as_slice() {
            [Out::Events(TOUCHSCREEN, r)] => r.clone(),
            [] => vec![],
            o => panic!("{o:?}"),
        }
    }

    #[test]
    fn a_click_taps_at_the_pointer() {
        let mut d = screen();
        assert!(
            d.line(&format!("pointer_enter 360.00 640.00 {W}"))
                .is_empty()
        );
        assert_eq!(
            records(d.line("pointer_button 5 272 1")),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, 0),
                (EV_ABS, ABS_MT_POSITION_X, 16384),
                (EV_ABS, ABS_MT_POSITION_Y, 16384),
                (EV_KEY, BTN_TOUCH, 1),
            ]
        );
        assert_eq!(
            records(d.line("pointer_motion 6 0.00 1279.99")),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_POSITION_X, 0),
                (EV_ABS, ABS_MT_POSITION_Y, touch::MAX),
            ]
        );
        assert_eq!(
            records(d.line("pointer_button 7 272 0")),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_KEY, BTN_TOUCH, 0),
            ]
        );
    }

    #[test]
    fn hover_and_other_buttons_touch_nothing() {
        let mut d = screen();
        d.line(&format!("pointer_enter 10.00 10.00 {W}"));
        assert!(d.line("pointer_motion 1 20.00 20.00").is_empty());
        assert!(d.line("pointer_button 2 273 1").is_empty());
        assert!(d.line("pointer_button 3 273 0").is_empty());
        // A release without a press, such as one that started on the frame.
        assert!(d.line("pointer_button 4 272 0").is_empty());
        assert!(d.line("pointer_frame").is_empty());
    }

    #[test]
    fn other_windows_get_nothing() {
        let mut d = screen();
        d.line("pointer_enter 10.00 10.00 3");
        assert!(d.line("pointer_button 1 272 1").is_empty());
        assert!(d.line("touch_down 1 0 10.00 10.00 3").is_empty());
        assert!(d.line("touch_frame").is_empty());
        assert!(d.line("pointer_enter 10.00").is_empty());
        assert!(d.line("touch_down 1 0 x 10.00 3").is_empty());
    }

    #[test]
    fn leaving_the_window_ends_a_drag() {
        let mut d = screen();
        d.line(&format!("pointer_enter 10.00 10.00 {W}"));
        d.line("pointer_button 1 272 1");
        assert_eq!(
            records(d.line(&format!("pointer_leave {W}"))),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_KEY, BTN_TOUCH, 0),
            ]
        );
        assert!(d.line("pointer_motion 2 20.00 20.00").is_empty());
        assert!(d.line("pointer_button 3 272 0").is_empty());
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
            records(d.line("touch_frame")),
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
            records(d.line("touch_frame")),
            [
                (EV_ABS, ABS_MT_SLOT, 0),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_POSITION_X, 16384),
                (EV_ABS, ABS_MT_POSITION_Y, 16384),
            ]
        );
        assert_eq!(
            records(d.line("touch_cancel")),
            [
                (EV_ABS, ABS_MT_SLOT, 1),
                (EV_ABS, ABS_MT_TRACKING_ID, -1),
                (EV_KEY, BTN_TOUCH, 0),
            ]
        );
        assert!(d.line("touch_frame").is_empty());
    }

    #[test]
    fn the_pointer_and_touches_share_the_slots() {
        let mut d = screen();
        for id in 0..touch::SLOTS {
            d.line(&format!("touch_down 1 {id} 1.00 1.00 {W}"));
        }
        d.line("touch_frame");
        d.line(&format!("pointer_enter 10.00 10.00 {W}"));
        assert!(d.line("pointer_button 1 272 1").is_empty());
        assert!(d.line("pointer_button 2 272 0").is_empty());
        d.line("touch_up 3 4");
        d.line("touch_frame");
        let down = records(d.line("pointer_button 4 272 1"));
        assert_eq!(down[0], (EV_ABS, ABS_MT_SLOT, 4));
        assert!(!down.contains(&(EV_KEY, BTN_TOUCH, 1)));
    }

    #[test]
    fn a_closed_window_lifts_its_contacts_and_the_last_takes_the_device() {
        let mut d = screen();
        let other = W + 1;
        assert!(d.map(other, 100, 100).is_empty());
        d.line(&format!("touch_down 1 0 1.00 1.00 {W}"));
        d.line(&format!("touch_down 1 1 1.00 1.00 {other}"));
        d.line("touch_frame");
        assert_eq!(
            d.unmap(W),
            [Out::Events(
                TOUCHSCREEN,
                vec![(EV_ABS, ABS_MT_SLOT, 0), (EV_ABS, ABS_MT_TRACKING_ID, -1)]
            )]
        );
        assert!(d.line("touch_motion 2 0 5.00 5.00").is_empty());
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
