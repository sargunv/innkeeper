//! The host seat, turned into input lines ([`super::proto`]). The display
//! service owns the Wayland connection; its seat's pointer, keyboard and
//! touch events land here. Pointer events over a window's client-side frame
//! go back to the display service, which drives the frame.

use std::collections::HashMap;
use std::time::Duration;

use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::backend::ObjectId;
use sctk::reexports::client::protocol::{wl_keyboard, wl_pointer, wl_seat, wl_touch};
use sctk::reexports::client::{Connection, Proxy, QueueHandle, WEnum};
use sctk::seat::Capability;
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;

use super::text_input::TextInput;
use crate::display::wl::State;

/// evdev KEY_F11.
const KEY_F11: u32 = 87;

pub(crate) struct Seat {
    pub(crate) wl_seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    touch: Option<wl_touch::WlTouch>,
    /// Where the pointer is: a window's content, or a window's frame.
    pointer_focus: Option<(u32, bool)>,
    /// The surface the pointer entered (decoration parts are subsurfaces).
    pointer_surface: Option<ObjectId>,
    pub(crate) keyboard_focus: Option<u32>,
    touch_focus: HashMap<i32, u32>,
    /// zwp_text_input_v3 for the IME.
    pub(crate) text: TextInput,
    /// The newest input serial (window moves, resizes and activation).
    pub(crate) last_serial: u32,
}

impl Seat {
    pub(crate) fn new(text_input: Option<ZwpTextInputManagerV3>) -> Seat {
        Seat {
            wl_seat: None,
            pointer: None,
            keyboard: None,
            touch: None,
            pointer_focus: None,
            pointer_surface: None,
            keyboard_focus: None,
            touch_focus: HashMap::new(),
            text: TextInput::new(text_input),
            last_serial: 0,
        }
    }

    pub(crate) fn add_capability(
        &mut self,
        seat: wl_seat::WlSeat,
        cap: Capability,
        qh: &QueueHandle<State>,
    ) {
        if self.wl_seat.is_none() {
            self.wl_seat = Some(seat.clone());
        }
        match cap {
            Capability::Pointer if self.pointer.is_none() => {
                self.pointer = Some(seat.get_pointer(qh, Ptr))
            }
            Capability::Keyboard if self.keyboard.is_none() => {
                self.keyboard = Some(seat.get_keyboard(qh, Kbd));
                self.text.attach(&seat, qh);
            }
            Capability::Touch if self.touch.is_none() => self.touch = Some(seat.get_touch(qh, Tch)),
            _ => {}
        }
    }

    pub(crate) fn remove_capability(&mut self, cap: Capability) {
        match cap {
            Capability::Pointer => {
                if let Some(p) = self.pointer.take() {
                    p.release();
                }
            }
            Capability::Keyboard => {
                if let Some(k) = self.keyboard.take() {
                    k.release();
                }
                self.text.detach();
            }
            Capability::Touch => {
                if let Some(t) = self.touch.take() {
                    t.release();
                }
            }
            _ => {}
        }
    }

    fn over_content(&self) -> bool {
        matches!(self.pointer_focus, Some((_, false)))
    }

    /// The window `id` is gone.
    pub(crate) fn forget(&mut self, id: u32) {
        if self.pointer_focus.is_some_and(|(p, _)| p == id) {
            self.pointer_focus = None;
        }
        if self.keyboard_focus == Some(id) {
            self.keyboard_focus = None;
        }
        self.touch_focus.retain(|_, w| *w != id);
        self.text.forget(id);
    }
}

struct Ptr;
struct Kbd;
struct Tch;

impl Dispatch2<wl_pointer::WlPointer, State> for Ptr {
    fn event(
        &self,
        st: &mut State,
        _: &wl_pointer::WlPointer,
        e: wl_pointer::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use wl_pointer::Event as E;
        let ms = |t: u32| Duration::from_millis(t as u64);
        match e {
            E::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                st.seat.last_serial = serial;
                let f = st.window_of(&surface);
                st.seat.pointer_focus = f;
                st.seat.pointer_surface = Some(surface.id());
                match f {
                    Some((id, false)) => {
                        let (x, y) = st.to_display(id, surface_x, surface_y);
                        st.hub.input(&format!("pointer_enter {x:.2} {y:.2} {id}"));
                    }
                    Some((id, true)) => st.frame_pointer_moved(
                        id,
                        &surface.id(),
                        Duration::ZERO,
                        surface_x,
                        surface_y,
                    ),
                    None => {}
                }
            }
            E::Leave { .. } => {
                st.seat.pointer_surface = None;
                match st.seat.pointer_focus.take() {
                    Some((id, false)) => st.hub.input(&format!("pointer_leave {id}")),
                    Some((id, true)) => st.frame_pointer_left(id),
                    None => {}
                }
            }
            E::Motion {
                time,
                surface_x,
                surface_y,
            } => match st.seat.pointer_focus {
                Some((id, false)) => {
                    let (x, y) = st.to_display(id, surface_x, surface_y);
                    st.hub
                        .input(&format!("pointer_motion {time} {x:.2} {y:.2}"));
                }
                Some((id, true)) => {
                    if let Some(sid) = st.seat.pointer_surface.clone() {
                        st.frame_pointer_moved(id, &sid, ms(time), surface_x, surface_y);
                    }
                }
                None => {}
            },
            E::Button {
                serial,
                time,
                button,
                state,
            } => {
                st.seat.last_serial = serial;
                let pressed = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                match st.seat.pointer_focus {
                    Some((_, false)) => st
                        .hub
                        .input(&format!("pointer_button {time} {button} {}", pressed as u8)),
                    Some((id, true)) => st.frame_click(id, ms(time), button, pressed, serial),
                    None => {}
                }
            }
            E::Axis { time, axis, value } => {
                if st.seat.over_content() {
                    let a: u32 = axis.into();
                    st.hub.input(&format!("pointer_axis {time} {a} {value:.3}"));
                }
            }
            E::AxisStop { time, axis } => {
                if st.seat.over_content() {
                    let a: u32 = axis.into();
                    st.hub.input(&format!("pointer_axis_stop {time} {a}"));
                }
            }
            E::AxisValue120 { axis, value120 } => {
                if st.seat.over_content() {
                    let a: u32 = axis.into();
                    st.hub
                        .input(&format!("pointer_axis_value120 {a} {value120}"));
                }
            }
            // Also after a leave, which needs its frame too.
            E::Frame if !matches!(st.seat.pointer_focus, Some((_, true))) => {
                st.hub.input("pointer_frame")
            }
            _ => {}
        }
    }
}

impl Dispatch2<wl_keyboard::WlKeyboard, State> for Kbd {
    fn event(
        &self,
        st: &mut State,
        _: &wl_keyboard::WlKeyboard,
        e: wl_keyboard::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use wl_keyboard::Event as E;
        match e {
            E::Keymap { fd, .. } => drop(fd),
            E::Enter {
                serial, surface, ..
            } => {
                st.seat.last_serial = serial;
                let id = st.window_of(&surface).map(|(id, _)| id);
                st.seat.keyboard_focus = id;
                if let Some(id) = id {
                    st.hub.input(&format!("keyboard_enter {id}"));
                }
            }
            E::Leave { .. } => {
                if let Some(id) = st.seat.keyboard_focus.take() {
                    st.hub.input(&format!("keyboard_leave {id}"));
                }
            }
            E::Key {
                serial,
                time,
                key,
                state,
            } => {
                st.seat.last_serial = serial;
                let Some(id) = st.seat.keyboard_focus else {
                    return;
                };
                let s: u32 = state.into();
                // F11 toggles fullscreen, as in desktop apps; Android never
                // sees it.
                if key == KEY_F11 {
                    if s == 1 {
                        st.toggle_fullscreen(id);
                    }
                    return;
                }
                st.hub
                    .input(&format!("key {time} {key} {}", (s == 1) as u8));
            }
            E::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                st.hub.input(&format!(
                    "modifiers {mods_depressed} {mods_latched} {mods_locked} {group}"
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch2<wl_touch::WlTouch, State> for Tch {
    fn event(
        &self,
        st: &mut State,
        _: &wl_touch::WlTouch,
        e: wl_touch::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use wl_touch::Event as E;
        match e {
            E::Down {
                serial,
                time,
                surface,
                id,
                x,
                y,
            } => {
                st.seat.last_serial = serial;
                if let Some((w, false)) = st.window_of(&surface) {
                    st.seat.touch_focus.insert(id, w);
                    let (x, y) = st.to_display(w, x, y);
                    st.hub
                        .input(&format!("touch_down {time} {id} {x:.2} {y:.2} {w}"));
                }
            }
            E::Up { time, id, .. } => {
                if st.seat.touch_focus.remove(&id).is_some() {
                    st.hub.input(&format!("touch_up {time} {id}"));
                }
            }
            E::Motion { time, id, x, y } => {
                if let Some(&w) = st.seat.touch_focus.get(&id) {
                    let (x, y) = st.to_display(w, x, y);
                    st.hub
                        .input(&format!("touch_motion {time} {id} {x:.2} {y:.2}"));
                }
            }
            E::Frame => st.hub.input("touch_frame"),
            E::Cancel => {
                st.seat.touch_focus.clear();
                st.hub.input("touch_cancel");
            }
            _ => {}
        }
    }
}
