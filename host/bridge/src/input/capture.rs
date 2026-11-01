//! Host pointer lock for Android pointer capture. A change of Android's device
//! source requests a one-shot lock. Escape, a compositor unlock, focus loss or
//! removal of the window blocks relocking until Android releases capture.
//! Relative motion goes through the ordered evdev path.
use crate::display::wl::State;
use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_pointer::WlPointer;
use sctk::reexports::client::{Connection, QueueHandle};
use wayland_protocols::wp::pointer_constraints::zv1::client::{
    zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
    zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
};
use wayland_protocols::wp::relative_pointer::zv1::client::{
    zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
    zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
};

#[derive(Default)]
pub(crate) struct CaptureState {
    requested: bool,
    blocked: bool,
    window: Option<u32>,
    pub(crate) active: bool,
}
impl CaptureState {
    pub(crate) fn request(&mut self, enabled: bool) {
        self.requested = enabled;
        crate::info!("input: pointer capture requested={enabled}");
        if !enabled {
            self.blocked = false;
            self.window = None;
            self.active = false;
        }
    }
    fn reconcile(&mut self, keyboard: Option<u32>, pointer: Option<u32>) -> Option<u32> {
        if self.window.is_some() && (self.window != keyboard || self.window != pointer) {
            self.cancel();
        }
        if self.requested && !self.blocked && keyboard.is_some() && keyboard == pointer {
            self.window = keyboard;
        }
        self.window
    }
    fn cancel(&mut self) {
        self.blocked |= self.requested;
        self.window = None;
        self.active = false;
    }
}

pub(crate) struct Capture {
    constraints: Option<ZwpPointerConstraintsV1>,
    manager: Option<ZwpRelativePointerManagerV1>,
    relative: Option<ZwpRelativePointerV1>,
    lock: Option<(u32, ZwpLockedPointerV1)>,
    pub(crate) state: CaptureState,
}
pub(crate) struct C;
impl Capture {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Self {
        Self {
            constraints: globals.bind(qh, 1..=1, C).ok(),
            manager: globals.bind(qh, 1..=1, C).ok(),
            relative: None,
            lock: None,
            state: CaptureState::default(),
        }
    }
    pub(crate) fn attach(&mut self, pointer: &WlPointer, qh: &QueueHandle<State>) {
        self.relative = self
            .manager
            .as_ref()
            .map(|m| m.get_relative_pointer(pointer, qh, C));
    }
    pub(crate) fn detach(&mut self) {
        self.cancel();
        if let Some(r) = self.relative.take() {
            r.destroy();
        }
    }
    pub(crate) fn cancel(&mut self) {
        self.state.cancel();
        self.destroy_lock();
    }
    pub(crate) fn forget(&mut self, window: u32) {
        if self.state.window == Some(window) {
            self.cancel();
        }
    }
    fn destroy_lock(&mut self) {
        if let Some((window, lock)) = self.lock.take() {
            lock.destroy();
            crate::info!("input: pointer capture unlocked window {window}");
        }
    }
}
impl Capture {
    pub(crate) fn sync(
        &mut self,
        keyboard: Option<u32>,
        pointer_window: Option<u32>,
        surface: Option<&sctk::reexports::client::protocol::wl_surface::WlSurface>,
        pointer: Option<&WlPointer>,
        qh: &QueueHandle<State>,
    ) {
        let target = self.state.reconcile(keyboard, pointer_window);
        if self.lock.as_ref().map(|(w, _)| *w) == target {
            return;
        }
        self.destroy_lock();
        if let Some(window) = target
            && let (Some(m), Some(p), Some(surface), Some(_)) =
                (&self.constraints, pointer, surface, &self.relative)
        {
            let lock = m.lock_pointer(surface, p, None, Lifetime::Oneshot, qh, C);
            self.lock = Some((window, lock));
        }
    }
}
impl Dispatch2<ZwpLockedPointerV1, State> for C {
    fn event(
        &self,
        st: &mut State,
        lock: &ZwpLockedPointerV1,
        e: zwp_locked_pointer_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let c = &mut st.seat.capture;
        if !c.lock.as_ref().is_some_and(|(_, current)| current == lock) {
            return;
        }
        match e {
            zwp_locked_pointer_v1::Event::Locked => {
                c.state.active = true;
                crate::info!(
                    "input: pointer capture locked window {}",
                    c.state.window.unwrap()
                );
            }
            zwp_locked_pointer_v1::Event::Unlocked => c.cancel(),
            _ => {}
        }
    }
}
impl Dispatch2<ZwpRelativePointerV1, State> for C {
    fn event(
        &self,
        st: &mut State,
        _: &ZwpRelativePointerV1,
        e: zwp_relative_pointer_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if !st.seat.capture.state.active {
            return;
        }
        if let zwp_relative_pointer_v1::Event::RelativeMotion {
            dx_unaccel,
            dy_unaccel,
            ..
        } = e
            && let Some(window) = st.seat.capture.state.window
        {
            st.hub.input(&format!(
                "pointer_relative {window} {dx_unaccel} {dy_unaccel}"
            ));
        }
    }
}
impl Dispatch2<ZwpPointerConstraintsV1, State> for C {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpPointerConstraintsV1,
        _: wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
impl Dispatch2<ZwpRelativePointerManagerV1, State> for C {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpRelativePointerManagerV1,
        _: wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn capture_waits_for_its_focused_content_and_releases() {
        let mut s = CaptureState::default();
        s.request(true);
        assert_eq!(s.reconcile(Some(7), None), None);
        assert_eq!(s.reconcile(Some(7), Some(8)), None);
        assert_eq!(s.reconcile(Some(7), Some(7)), Some(7));
        s.active = true;
        s.request(false);
        assert!(!s.active);
        assert_eq!(s.reconcile(Some(7), Some(7)), None);
    }
    #[test]
    fn escape_or_compositor_unlock_cannot_relock_until_android_releases() {
        let mut s = CaptureState::default();
        s.request(true);
        assert_eq!(s.reconcile(Some(7), Some(7)), Some(7));
        s.cancel();
        s.request(true); // duplicate notification does not rearm
        assert_eq!(s.reconcile(Some(7), Some(7)), None);
        s.request(false);
        s.request(true);
        assert_eq!(s.reconcile(Some(7), Some(7)), Some(7));
    }
    #[test]
    fn focus_loss_does_not_transfer_capture_to_another_window() {
        for focus in [None, Some(8)] {
            let mut s = CaptureState::default();
            s.request(true);
            s.reconcile(Some(7), Some(7));
            assert_eq!(s.reconcile(focus, Some(7)), None);
            assert_eq!(s.reconcile(Some(8), Some(8)), None);
        }
    }
}
