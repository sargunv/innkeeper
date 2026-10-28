//! The host seat, turned into input lines ([`super::proto`]). The display
//! service owns the Wayland connection; its seat's pointer, keyboard and
//! touch events land here, and [`super::gestures`] and [`super::tablet`]
//! add touchpad gestures and tablets. Pointer events over a window's
//! client-side frame go back to the display service, which drives the frame.
//!
//! The cursor is always defined: the default arrow over a window's content
//! and the frame's own shapes over its edges (`wp_cursor_shape_v1`, where
//! the compositor has it). Android draws no pointer of its own.

use std::collections::HashMap;
use std::time::Duration;

use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::backend::ObjectId;
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::{wl_keyboard, wl_pointer, wl_seat, wl_touch};
use sctk::reexports::client::{Connection, Proxy, QueueHandle, WEnum};
use sctk::reexports::csd_frame::CursorIcon;
use sctk::seat::Capability;
use wayland_protocols::wp::cursor_shape::v1::client::{
    wp_cursor_shape_device_v1::{Shape, WpCursorShapeDeviceV1},
    wp_cursor_shape_manager_v1::WpCursorShapeManagerV1,
};
use wayland_protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;

use super::Hub;
use super::gestures::Gestures;
use super::tablet::Tablets;
use super::text_input::TextInput;
use crate::display::wl::State;

/// evdev KEY_F11.
const KEY_F11: u32 = 87;

pub(crate) struct Seat {
    pub(crate) wl_seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    pub(crate) cursor_shape: Option<WpCursorShapeManagerV1>,
    /// The pointer's cursor-shape device, the serial of its last enter and
    /// the shape set since.
    cursor: Option<WpCursorShapeDeviceV1>,
    enter_serial: u32,
    shape: Option<Shape>,
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
    pub(crate) gestures: Gestures,
    pub(crate) tablets: Tablets,
    /// The newest input serial (window moves, resizes and activation).
    pub(crate) last_serial: u32,
}

impl Seat {
    pub(crate) fn new(
        text_input: Option<ZwpTextInputManagerV3>,
        globals: &GlobalList,
        qh: &QueueHandle<State>,
    ) -> Seat {
        let seat = Seat {
            wl_seat: None,
            pointer: None,
            cursor_shape: globals.bind(qh, 1..=1, Cursor).ok(),
            cursor: None,
            enter_serial: 0,
            shape: None,
            keyboard: None,
            touch: None,
            pointer_focus: None,
            pointer_surface: None,
            keyboard_focus: None,
            touch_focus: HashMap::new(),
            text: TextInput::new(text_input),
            gestures: Gestures::bind(globals, qh),
            tablets: Tablets::bind(globals, qh),
            last_serial: 0,
        };
        crate::info!(
            "input: cursor-shape {}, pointer-gestures v{}, tablets {}",
            seat.cursor_shape.is_some(),
            seat.gestures.version(),
            seat.tablets.available()
        );
        seat
    }

    pub(crate) fn add_capability(
        &mut self,
        seat: wl_seat::WlSeat,
        cap: Capability,
        qh: &QueueHandle<State>,
    ) {
        if self.wl_seat.is_none() {
            self.wl_seat = Some(seat.clone());
            self.tablets.attach(&seat, qh);
        }
        match cap {
            Capability::Pointer if self.pointer.is_none() => {
                let p = seat.get_pointer(qh, Ptr);
                self.cursor = self
                    .cursor_shape
                    .as_ref()
                    .map(|m| m.get_pointer(&p, qh, Cursor));
                self.gestures.attach(&p, qh);
                self.pointer = Some(p);
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
                if let Some(c) = self.cursor.take() {
                    c.destroy();
                }
                self.gestures.detach();
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

    /// Sets the pointer's cursor (after an enter).
    fn set_cursor(&mut self, shape: Shape) {
        if let Some(c) = &self.cursor
            && self.shape != Some(shape)
        {
            c.set_shape(self.enter_serial, shape);
            self.shape = Some(shape);
        }
    }

    /// The window `id` is gone; gestures and a tablet tool over it end.
    pub(crate) fn forget(&mut self, id: u32, hub: &Hub) {
        self.gestures.forget(id, hub);
        self.tablets.forget(id, hub);
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

/// User data of the cursor-shape manager and devices (no events).
pub(crate) struct Cursor;

impl Dispatch2<WpCursorShapeManagerV1, State> for Cursor {
    fn event(
        &self,
        _: &mut State,
        _: &WpCursorShapeManagerV1,
        _: <WpCursorShapeManagerV1 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch2<WpCursorShapeDeviceV1, State> for Cursor {
    fn event(
        &self,
        _: &mut State,
        _: &WpCursorShapeDeviceV1,
        _: <WpCursorShapeDeviceV1 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

/// The shape for a client-side frame's cursor (edges resize, the rest is
/// the arrow).
fn frame_shape(icon: CursorIcon) -> Shape {
    use CursorIcon as C;
    match icon {
        C::NResize => Shape::NResize,
        C::SResize => Shape::SResize,
        C::EResize => Shape::EResize,
        C::WResize => Shape::WResize,
        C::NeResize => Shape::NeResize,
        C::NwResize => Shape::NwResize,
        C::SeResize => Shape::SeResize,
        C::SwResize => Shape::SwResize,
        C::EwResize => Shape::EwResize,
        C::NsResize => Shape::NsResize,
        C::NeswResize => Shape::NeswResize,
        C::NwseResize => Shape::NwseResize,
        C::Pointer => Shape::Pointer,
        C::Move => Shape::Move,
        _ => Shape::Default,
    }
}

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
                st.seat.enter_serial = serial;
                st.seat.shape = None;
                let f = st.window_of(&surface);
                st.seat.pointer_focus = f;
                st.seat.pointer_surface = Some(surface.id());
                match f {
                    Some((id, false)) => {
                        st.seat.set_cursor(Shape::Default);
                        let (x, y) = st.to_display(id, surface_x, surface_y);
                        st.hub.input(&format!("pointer_enter {x:.2} {y:.2} {id}"));
                    }
                    Some((id, true)) => {
                        let icon = st.frame_pointer_moved(
                            id,
                            &surface.id(),
                            Duration::ZERO,
                            surface_x,
                            surface_y,
                        );
                        st.seat.set_cursor(icon.map_or(Shape::Default, frame_shape));
                    }
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
                    if let Some(sid) = st.seat.pointer_surface.clone()
                        && let Some(icon) =
                            st.frame_pointer_moved(id, &sid, ms(time), surface_x, surface_y)
                    {
                        st.seat.set_cursor(frame_shape(icon));
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
            E::AxisSource { axis_source } => {
                if st.seat.over_content() {
                    let s: u32 = axis_source.into();
                    st.hub.input(&format!("pointer_axis_source {s}"));
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
