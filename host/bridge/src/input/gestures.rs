//! Touchpad gestures (`zwp_pointer_gestures_v1`) as input lines: swipes of
//! three or more fingers, pinches and holds that begin over a window's
//! content ([`super::proto`]). Two-finger scrolling is `wl_pointer.axis`
//! with `axis_source` finger, not a gesture. The system app turns these into
//! Android's touchpad gestures (`docs/input.md`).

use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::{wl_pointer::WlPointer, wl_surface::WlSurface};
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::pointer_gestures::zv1::client::{
    zwp_pointer_gesture_hold_v1::{self, ZwpPointerGestureHoldV1},
    zwp_pointer_gesture_pinch_v1::{self, ZwpPointerGesturePinchV1},
    zwp_pointer_gesture_swipe_v1::{self, ZwpPointerGestureSwipeV1},
    zwp_pointer_gestures_v1::ZwpPointerGesturesV1,
};

use super::Hub;
use crate::display::wl::State;

#[derive(Default)]
pub(crate) struct Gestures {
    manager: Option<ZwpPointerGesturesV1>,
    swipe: Option<ZwpPointerGestureSwipeV1>,
    pinch: Option<ZwpPointerGesturePinchV1>,
    /// Version 3 and later.
    hold: Option<ZwpPointerGestureHoldV1>,
    /// The window each gesture in progress began on; only those are
    /// forwarded.
    on: [Option<u32>; 3],
}

const SWIPE: usize = 0;
const PINCH: usize = 1;
const HOLD: usize = 2;
const NAMES: [&str; 3] = ["swipe", "pinch", "hold"];

/// User data of the gesture objects.
pub(crate) struct G;

impl Gestures {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Gestures {
        Gestures {
            manager: globals.bind(qh, 1..=3, G).ok(),
            ..Default::default()
        }
    }

    pub(crate) fn version(&self) -> u32 {
        self.manager.as_ref().map_or(0, |m| m.version())
    }

    pub(crate) fn attach(&mut self, pointer: &WlPointer, qh: &QueueHandle<State>) {
        let Some(m) = &self.manager else { return };
        self.swipe = Some(m.get_swipe_gesture(pointer, qh, G));
        self.pinch = Some(m.get_pinch_gesture(pointer, qh, G));
        if m.version() >= 3 {
            self.hold = Some(m.get_hold_gesture(pointer, qh, G));
        }
    }

    pub(crate) fn detach(&mut self) {
        if let Some(s) = self.swipe.take() {
            s.destroy();
        }
        if let Some(p) = self.pinch.take() {
            p.destroy();
        }
        if let Some(h) = self.hold.take() {
            h.destroy();
        }
        self.on = [None; 3];
    }

    /// The window `id` is gone: its gestures end, cancelled.
    pub(crate) fn forget(&mut self, id: u32, hub: &Hub) {
        for (g, on) in self.on.iter_mut().enumerate() {
            if *on == Some(id) {
                *on = None;
                hub.input(&format!("gesture_{}_end 0 1", NAMES[g]));
            }
        }
    }
}

impl State {
    fn gesture_begin(&mut self, g: usize, time: u32, surface: &WlSurface, fingers: u32) {
        let Some((w, false)) = self.window_of(surface) else {
            return;
        };
        self.seat.gestures.on[g] = Some(w);
        self.hub
            .input(&format!("gesture_{}_begin {time} {fingers}", NAMES[g]));
    }

    fn gesture_end(&mut self, g: usize, time: u32, cancelled: i32) {
        if self.seat.gestures.on[g].take().is_some() {
            self.hub.input(&format!(
                "gesture_{}_end {time} {}",
                NAMES[g],
                (cancelled != 0) as u8
            ));
        }
    }
}

impl Dispatch2<ZwpPointerGesturesV1, State> for G {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpPointerGesturesV1,
        _: <ZwpPointerGesturesV1 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch2<ZwpPointerGestureSwipeV1, State> for G {
    fn event(
        &self,
        st: &mut State,
        _: &ZwpPointerGestureSwipeV1,
        e: zwp_pointer_gesture_swipe_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use zwp_pointer_gesture_swipe_v1::Event as E;
        match e {
            E::Begin {
                time,
                surface,
                fingers,
                ..
            } => st.gesture_begin(SWIPE, time, &surface, fingers),
            E::Update { time, dx, dy } => {
                if let Some(w) = st.seat.gestures.on[SWIPE] {
                    let (dx, dy) = st.to_display(w, dx, dy);
                    st.hub
                        .input(&format!("gesture_swipe_update {time} {dx:.2} {dy:.2}"));
                }
            }
            E::End {
                time, cancelled, ..
            } => st.gesture_end(SWIPE, time, cancelled),
            _ => {}
        }
    }
}

impl Dispatch2<ZwpPointerGesturePinchV1, State> for G {
    fn event(
        &self,
        st: &mut State,
        _: &ZwpPointerGesturePinchV1,
        e: zwp_pointer_gesture_pinch_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use zwp_pointer_gesture_pinch_v1::Event as E;
        match e {
            E::Begin {
                time,
                surface,
                fingers,
                ..
            } => st.gesture_begin(PINCH, time, &surface, fingers),
            E::Update {
                time,
                dx,
                dy,
                scale,
                rotation,
            } => {
                if let Some(w) = st.seat.gestures.on[PINCH] {
                    let (dx, dy) = st.to_display(w, dx, dy);
                    st.hub.input(&format!(
                        "gesture_pinch_update {time} {dx:.2} {dy:.2} {scale:.4} {rotation:.2}"
                    ));
                }
            }
            E::End {
                time, cancelled, ..
            } => st.gesture_end(PINCH, time, cancelled),
            _ => {}
        }
    }
}

impl Dispatch2<ZwpPointerGestureHoldV1, State> for G {
    fn event(
        &self,
        st: &mut State,
        _: &ZwpPointerGestureHoldV1,
        e: zwp_pointer_gesture_hold_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use zwp_pointer_gesture_hold_v1::Event as E;
        match e {
            E::Begin {
                time,
                surface,
                fingers,
                ..
            } => st.gesture_begin(HOLD, time, &surface, fingers),
            E::End {
                time, cancelled, ..
            } => st.gesture_end(HOLD, time, cancelled),
            _ => {}
        }
    }
}
