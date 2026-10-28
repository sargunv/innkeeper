//! `zwp_text_input_v3` for the Linux.apk IME (`docs/input.md`).
//!
//! One text input per seat. It follows the keyboard focus: the compositor
//! sends `enter`/`leave` for our windows' surfaces. The IME's state arrives
//! as [`Req`]s on the input service and is applied to the window that has
//! text-input focus; the compositor's input-method results (preedit, commit,
//! delete) go back to the IME as input lines at each `done`.
//!
//! Without a host input method (IBus, Fcitx), enabling changes nothing and
//! keys keep arriving as `wl_keyboard` keys.

use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::protocol::{wl_seat, wl_surface};
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::text_input::zv3::client::{
    zwp_text_input_manager_v3::ZwpTextInputManagerV3,
    zwp_text_input_v3::{self, ChangeCause, ContentHint, ContentPurpose, ZwpTextInputV3},
};

use super::ime::{Done, Req};
use crate::display::wl::State;

#[derive(Default)]
pub(crate) struct TextInput {
    manager: Option<ZwpTextInputManagerV3>,
    ti: Option<ZwpTextInputV3>,
    /// The window whose surface has text-input focus.
    entered: Option<u32>,
    /// Enabled since the last enter (the IME asked for it).
    enabled: bool,
    /// Gathered until the next `done`.
    pending: Done,
}

impl TextInput {
    pub(crate) fn new(manager: Option<ZwpTextInputManagerV3>) -> TextInput {
        TextInput {
            manager,
            ..Default::default()
        }
    }

    pub(crate) fn available(&self) -> bool {
        self.manager.is_some()
    }

    /// Creates the seat's text input once there is a seat.
    pub(crate) fn attach(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<State>) {
        if self.ti.is_none()
            && let Some(m) = &self.manager
        {
            self.ti = Some(m.get_text_input(seat, qh, Ti));
        }
    }

    pub(crate) fn detach(&mut self) {
        if let Some(t) = self.ti.take() {
            t.destroy();
        }
        self.entered = None;
        self.enabled = false;
    }

    /// The window `id` is gone.
    pub(crate) fn forget(&mut self, id: u32) {
        if self.entered == Some(id) {
            self.entered = None;
            self.enabled = false;
        }
    }
}

impl State {
    /// A request from the IME, applied to the focused window's text input.
    pub(crate) fn text_input(&mut self, req: Req) {
        let text = &mut self.seat.text;
        let (Some(id), Some(ti)) = (text.entered, text.ti.clone()) else {
            return;
        };
        match req {
            Req::Enable => {
                ti.enable();
                text.enabled = true;
            }
            Req::Disable => {
                ti.disable();
                text.enabled = false;
            }
            _ if !text.enabled => {}
            Req::Content { hint, purpose } => ti.set_content_type(
                ContentHint::from_bits_truncate(hint),
                ContentPurpose::try_from(purpose).unwrap_or(ContentPurpose::Normal),
            ),
            Req::Surrounding {
                text,
                cursor,
                anchor,
                other,
            } => {
                ti.set_surrounding_text(text, cursor as i32, anchor as i32);
                // Double-buffered; the next commit resets it to input_method.
                if other {
                    ti.set_text_change_cause(ChangeCause::Other);
                }
            }
            Req::Cursor { x, y, w, h } => {
                // Display pixels to surface-local (logical) coordinates.
                let s = self.scale120(id) as i64;
                let f = |v: i32| (v as i64 * 120 / s) as i32;
                ti.set_cursor_rectangle(f(x), f(y), f(w).max(1), f(h).max(1));
            }
            Req::Apply => ti.commit(),
        }
    }

    fn text_lines(&self, lines: Vec<String>) {
        for l in lines {
            self.hub.input(&l);
        }
    }
}

/// User data of the seat's text input.
pub(crate) struct Ti;

impl Dispatch2<ZwpTextInputManagerV3, State> for crate::display::wl::Unit {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpTextInputManagerV3,
        _: <ZwpTextInputManagerV3 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch2<ZwpTextInputV3, State> for Ti {
    fn event(
        &self,
        st: &mut State,
        _: &ZwpTextInputV3,
        e: zwp_text_input_v3::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        use zwp_text_input_v3::Event as E;
        match e {
            E::Enter { surface } => {
                // A frame's decoration surface is not a text field.
                let id = window_content(st, &surface);
                st.seat.text.entered = id;
                st.seat.text.enabled = false;
                st.seat.text.pending = Done::default();
            }
            E::Leave { .. } => {
                // The compositor dropped our state; the IME clears any
                // preedit it shows (the protocol asks clients to reset it).
                if let Some(id) = st.seat.text.entered.take()
                    && st.seat.text.enabled
                {
                    st.text_lines(Done::default().to_lines(id));
                }
                st.seat.text.enabled = false;
                st.seat.text.pending = Done::default();
            }
            E::PreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => {
                st.seat.text.pending.preedit =
                    Some((text.unwrap_or_default(), cursor_begin, cursor_end));
            }
            E::CommitString { text } => {
                st.seat.text.pending.commit = text;
            }
            E::DeleteSurroundingText {
                before_length,
                after_length,
            } => {
                st.seat.text.pending.delete = Some((before_length, after_length));
            }
            E::Done { .. } => {
                let done = std::mem::take(&mut st.seat.text.pending);
                // Results for a window that lost focus meanwhile are dropped.
                if let Some(id) = st.seat.text.entered
                    && st.seat.text.enabled
                {
                    st.text_lines(done.to_lines(id));
                }
            }
            _ => {}
        }
    }
}

fn window_content(st: &State, s: &wl_surface::WlSurface) -> Option<u32> {
    st.window_of(s)
        .and_then(|(id, frame)| (!frame).then_some(id))
}
