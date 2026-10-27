//! The display service's Wayland side: one host Wayland connection per
//! composer connection, one `xdg_toplevel` per window, dma-buf frames, and
//! the seat turned into input-protocol v2 lines (docs/hal-display.md).
//!
//! Decorations: server-side when the compositor offers xdg-decoration (KDE),
//! else an Adwaita-style client-side frame from `sctk-adwaita` (GNOME,
//! weston). Everything runs on one thread with a calloop event loop; the
//! service sends it [`Cmd`]s over a channel.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use calloop::EventLoop;
use calloop::channel::{Channel, Event as ChannelEvent};
use sctk::compositor::{CompositorHandler, CompositorState};
use sctk::dmabuf::{DmabufFeedback, DmabufHandler, DmabufState};
use sctk::output::{OutputHandler, OutputState};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::globals::{GlobalList, registry_queue_init};
use sctk::reexports::client::protocol::{
    wl_buffer, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface, wl_touch,
};
use sctk::reexports::client::{Connection, Proxy, QueueHandle, WEnum};
use sctk::reexports::csd_frame::{DecorationsFrame, FrameAction, FrameClick};
use sctk::reexports::protocols::wp::linux_dmabuf::zv1::client::{
    zwp_linux_buffer_params_v1, zwp_linux_dmabuf_feedback_v1,
};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::seat::{Capability, SeatHandler, SeatState};
use sctk::shell::WaylandSurface;
use sctk::shell::xdg::window::{
    DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use sctk::shell::xdg::{XdgShell, XdgSurface};
use sctk::shm::{Shm, ShmHandler};
use sctk::subcompositor::SubcompositorState;
use sctk::{delegate_dispatch2, delegate_registry, dispatch2::Dispatch2, registry_handlers};
use sctk_adwaita::{AdwaitaFrame, FrameConfig};
use wayland_protocols::wp::fractional_scale::v1::client::{
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
    wp_fractional_scale_v1::{self, WpFractionalScaleV1},
};
use wayland_protocols::wp::viewporter::client::{
    wp_viewport::WpViewport, wp_viewporter::WpViewporter,
};
use wayland_protocols::xdg::activation::v1::client::{
    xdg_activation_token_v1::{self, XdgActivationTokenV1},
    xdg_activation_v1::XdgActivationV1,
};

use super::ime;
use super::proto::{BufferSpec, WindowSpec, event, ty};
use crate::server::Events;

mod foreign;
mod text_input;

/// Work for the Wayland thread.
pub enum Cmd {
    Create(WindowSpec, std::sync::mpsc::Sender<i32>),
    Destroy(u32),
    Title(u32, String),
    MinSize(u32, u32, u32),
    Activate(u32),
    BufferAdd(BufferSpec, OwnedFd),
    BufferRemove(u32),
    Frame {
        window: u32,
        buffer: u32,
        damage: [i32; 4],
    },
    /// A request from the Linux.apk IME (through the composer).
    TextInput(ime::Req),
    Quit,
}

/// evdev KEY_F11.
const KEY_F11: u32 = 87;

/// Default minimum logical window size.
const MIN_W: u32 = 240;
const MIN_H: u32 = 160;

struct Buf {
    window: u32,
    width: u32,
    height: u32,
    wl: Option<wl_buffer::WlBuffer>,
    /// Attached and not yet released by the compositor.
    busy: bool,
}

struct Win {
    id: u32,
    window: Window,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    /// xdg-foreign export, for portal dialogs parented to the window.
    exported:
        Option<wayland_protocols::xdg::foreign::zv2::client::zxdg_exported_v2::ZxdgExportedV2>,
    frame: Option<AdwaitaFrame<State>>,
    title: String,
    app_id: String,
    /// Logical content size and scale×120.
    width: u32,
    height: u32,
    scale120: u32,
    configured: bool,
    fullscreen: bool,
    /// Buffer shown now (its size maps input), and the last one asked for.
    shown: Option<u32>,
    shown_size: (u32, u32),
}

struct State {
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor: Arc<CompositorState>,
    subcompositor: Arc<SubcompositorState>,
    shm: Shm,
    xdg: XdgShell,
    dmabuf: DmabufState,
    viewporter: Option<WpViewporter>,
    fractional: Option<WpFractionalScaleManagerV1>,
    activation: Option<XdgActivationV1>,
    exporter:
        Option<wayland_protocols::xdg::foreign::zv2::client::zxdg_exporter_v2::ZxdgExporterV2>,
    qh: QueueHandle<State>,
    events: Events,

    windows: HashMap<u32, Win>,
    buffers: HashMap<u32, Buf>,
    /// zwp_linux_buffer_params_v1 being created -> buffer id.
    params: HashMap<sctk::reexports::client::backend::ObjectId, u32>,

    seat: Option<wl_seat::WlSeat>,
    pointer: Option<wl_pointer::WlPointer>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    touch: Option<wl_touch::WlTouch>,
    /// Where the pointer is: a window's content, or a window's frame.
    pointer_focus: Option<(u32, bool)>,
    /// The surface the pointer entered (decoration parts are subsurfaces).
    pointer_surface: Option<sctk::reexports::client::backend::ObjectId>,
    keyboard_focus: Option<u32>,
    touch_focus: HashMap<i32, u32>,
    /// zwp_text_input_v3 for the IME (`text_input.rs`).
    text: text_input::TextInput,
    last_serial: u32,
    quit: bool,
}

// ---- globals we bind ourselves ------------------------------------------------

/// User data for objects whose events we ignore or handle inline.
struct Unit;

impl Dispatch2<WpViewporter, State> for Unit {
    fn event(
        &self,
        _: &mut State,
        _: &WpViewporter,
        _: <WpViewporter as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
impl Dispatch2<WpViewport, State> for Unit {
    fn event(
        &self,
        _: &mut State,
        _: &WpViewport,
        _: <WpViewport as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
impl Dispatch2<WpFractionalScaleManagerV1, State> for Unit {
    fn event(
        &self,
        _: &mut State,
        _: &WpFractionalScaleManagerV1,
        _: <WpFractionalScaleManagerV1 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
impl Dispatch2<XdgActivationV1, State> for Unit {
    fn event(
        &self,
        _: &mut State,
        _: &XdgActivationV1,
        _: <XdgActivationV1 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

/// A window's fractional-scale object.
struct ScaleOf(u32);
impl Dispatch2<WpFractionalScaleV1, State> for ScaleOf {
    fn event(
        &self,
        st: &mut State,
        _: &WpFractionalScaleV1,
        e: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = e
            && let Some(w) = st.windows.get_mut(&self.0)
        {
            w.scale120 = scale.clamp(60, 120 * 8);
            if let Some(f) = w.frame.as_mut() {
                f.set_scaling_factor(scale as f64 / 120.0);
            }
            st.send_configure(self.0);
        }
    }
}

/// An activation token request for a window.
struct TokenFor(u32);
impl Dispatch2<XdgActivationTokenV1, State> for TokenFor {
    fn event(
        &self,
        st: &mut State,
        t: &XdgActivationTokenV1,
        e: xdg_activation_token_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token } = e {
            if let (Some(a), Some(w)) = (&st.activation, st.windows.get(&self.0)) {
                a.activate(token, w.window.wl_surface());
            }
            t.destroy();
        }
    }
}

// ---- seat devices: raw events, turned into protocol v2 lines -------------------

struct Ptr;
struct Kbd;
struct Tch;

impl State {
    fn input(&self, line: String) {
        if let Err(e) = self.events.try_send(ty::INPUT, &event::input(&line), &[]) {
            crate::debug!("display: input event dropped: {e}");
        }
    }

    /// Which window `s` belongs to, and whether it is the window's frame.
    fn window_of(&self, s: &wl_surface::WlSurface) -> Option<(u32, bool)> {
        for w in self.windows.values() {
            if w.window.wl_surface() == s {
                return Some((w.id, false));
            }
        }
        // Decoration subsurfaces have the window surface as parent.
        let parent = s
            .data::<sctk::compositor::SurfaceData<()>>()
            .and_then(|d| d.parent_surface().cloned())?;
        self.windows
            .values()
            .find(|w| w.window.wl_surface() == &parent)
            .map(|w| (w.id, true))
    }

    /// Surface-local logical -> the shown frame's pixels.
    fn to_display(&self, id: u32, x: f64, y: f64) -> (f64, f64) {
        match self.windows.get(&id) {
            Some(w) if w.width > 0 && w.height > 0 && w.shown_size.0 > 0 => (
                x * w.shown_size.0 as f64 / w.width as f64,
                y * w.shown_size.1 as f64 / w.height as f64,
            ),
            _ => (x, y),
        }
    }

    fn frame_action(&mut self, id: u32, action: FrameAction, serial: u32) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let Some(seat) = self.seat.clone() else {
            return;
        };
        match action {
            FrameAction::Close => {
                let _ = self.events.try_send(ty::CLOSE, &event::close(id), &[]);
            }
            FrameAction::Minimize => w.window.set_minimized(),
            FrameAction::Maximize => w.window.set_maximized(),
            FrameAction::UnMaximize => w.window.unset_maximized(),
            FrameAction::Move => w.window.move_(&seat, serial),
            FrameAction::Resize(edge) => {
                use sctk::reexports::csd_frame::ResizeEdge as E;
                use sctk::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge as X;
                let e = match edge {
                    E::Top => X::Top,
                    E::Bottom => X::Bottom,
                    E::Left => X::Left,
                    E::TopLeft => X::TopLeft,
                    E::BottomLeft => X::BottomLeft,
                    E::Right => X::Right,
                    E::TopRight => X::TopRight,
                    E::BottomRight => X::BottomRight,
                    _ => X::None,
                };
                w.window.resize(&seat, serial, e)
            }
            FrameAction::ShowMenu(x, y) => w.window.show_window_menu(&seat, serial, (x, y)),
            _ => {}
        }
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
        match e {
            E::Enter {
                serial,
                surface,
                surface_x,
                surface_y,
            } => {
                st.last_serial = serial;
                let f = st.window_of(&surface);
                st.pointer_focus = f;
                st.pointer_surface = Some(surface.id());
                match f {
                    Some((id, false)) => {
                        let (x, y) = st.to_display(id, surface_x, surface_y);
                        st.input(format!("pointer_enter {x:.2} {y:.2} {id}"));
                    }
                    Some((id, true)) => {
                        if let Some(fr) = st.windows.get_mut(&id).and_then(|w| w.frame.as_mut()) {
                            fr.click_point_moved(
                                Duration::ZERO,
                                &surface.id(),
                                surface_x,
                                surface_y,
                            );
                        }
                    }
                    None => {}
                }
            }
            E::Leave { .. } => {
                st.pointer_surface = None;
                let focus = st.pointer_focus.take();
                match focus {
                    Some((id, false)) => st.input(format!("pointer_leave {id}")),
                    Some((id, true)) => {
                        if let Some(fr) = st.windows.get_mut(&id).and_then(|w| w.frame.as_mut()) {
                            fr.click_point_left();
                        }
                    }
                    None => {}
                }
            }
            E::Motion {
                time,
                surface_x,
                surface_y,
            } => match st.pointer_focus {
                Some((id, false)) => {
                    let (x, y) = st.to_display(id, surface_x, surface_y);
                    st.input(format!("pointer_motion {time} {x:.2} {y:.2}"));
                }
                Some((id, true)) => {
                    let sid = st.pointer_surface.clone();
                    if let (Some(sid), Some(fr)) =
                        (sid, st.windows.get_mut(&id).and_then(|w| w.frame.as_mut()))
                    {
                        fr.click_point_moved(
                            Duration::from_millis(time as u64),
                            &sid,
                            surface_x,
                            surface_y,
                        );
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
                st.last_serial = serial;
                let pressed = matches!(state, WEnum::Value(wl_pointer::ButtonState::Pressed));
                match st.pointer_focus {
                    Some((_, false)) => {
                        st.input(format!("pointer_button {time} {button} {}", pressed as u8))
                    }
                    Some((id, true)) => {
                        let click = match button {
                            0x110 => FrameClick::Normal,
                            0x111 => FrameClick::Alternate,
                            _ => return,
                        };
                        let action = st
                            .windows
                            .get_mut(&id)
                            .and_then(|w| w.frame.as_mut())
                            .and_then(|f| {
                                f.on_click(Duration::from_millis(time as u64), click, pressed)
                            });
                        if let Some(a) = action {
                            st.frame_action(id, a, serial);
                        }
                    }
                    None => {}
                }
            }
            E::Axis { time, axis, value } => {
                if matches!(st.pointer_focus, Some((_, false))) {
                    let a: u32 = axis.into();
                    st.input(format!("pointer_axis {time} {a} {value:.3}"));
                }
            }
            E::AxisStop { time, axis } => {
                if matches!(st.pointer_focus, Some((_, false))) {
                    let a: u32 = axis.into();
                    st.input(format!("pointer_axis_stop {time} {a}"));
                }
            }
            E::AxisValue120 { axis, value120 } => {
                if matches!(st.pointer_focus, Some((_, false))) {
                    let a: u32 = axis.into();
                    st.input(format!("pointer_axis_value120 {a} {value120}"));
                }
            }
            E::Frame if !matches!(st.pointer_focus, Some((_, true))) => {
                st.input("pointer_frame".into());
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
                st.last_serial = serial;
                let id = st.window_of(&surface).map(|(id, _)| id);
                st.keyboard_focus = id;
                if let Some(id) = id {
                    st.input(format!("keyboard_enter {id}"));
                }
            }
            E::Leave { .. } => {
                if let Some(id) = st.keyboard_focus.take() {
                    st.input(format!("keyboard_leave {id}"));
                }
            }
            E::Key {
                serial,
                time,
                key,
                state,
            } => {
                st.last_serial = serial;
                let Some(id) = st.keyboard_focus else { return };
                let s: u32 = state.into();
                // F11 toggles fullscreen, as in desktop apps; Android never
                // sees it.
                if key == KEY_F11 {
                    if s == 1
                        && let Some(w) = st.windows.get(&id)
                    {
                        if w.fullscreen {
                            w.window.unset_fullscreen();
                        } else {
                            w.window.set_fullscreen(None);
                        }
                    }
                    return;
                }
                st.input(format!("key {time} {key} {}", (s == 1) as u8));
            }
            E::Modifiers {
                mods_depressed,
                mods_latched,
                mods_locked,
                group,
                ..
            } => {
                st.input(format!(
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
                st.last_serial = serial;
                if let Some((w, false)) = st.window_of(&surface) {
                    st.touch_focus.insert(id, w);
                    let (x, y) = st.to_display(w, x, y);
                    st.input(format!("touch_down {time} {id} {x:.2} {y:.2} {w}"));
                }
            }
            E::Up { time, id, .. } => {
                if st.touch_focus.remove(&id).is_some() {
                    st.input(format!("touch_up {time} {id}"));
                }
            }
            E::Motion { time, id, x, y } => {
                if let Some(&w) = st.touch_focus.get(&id) {
                    let (x, y) = st.to_display(w, x, y);
                    st.input(format!("touch_motion {time} {id} {x:.2} {y:.2}"));
                }
            }
            E::Frame => st.input("touch_frame".into()),
            E::Cancel => {
                st.touch_focus.clear();
                st.input("touch_cancel".into());
            }
            _ => {}
        }
    }
}

// ---- window handling --------------------------------------------------------------

impl State {
    fn release(&self, buffer: u32) {
        if let Err(e) = self
            .events
            .try_send(ty::RELEASE, &event::release(buffer), &[])
        {
            crate::warn!("display: release of buffer {buffer} lost: {e}");
        }
    }

    fn send_configure(&self, id: u32) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if !w.configured {
            return;
        }
        let body = event::configure(id, w.width, w.height, w.scale120);
        if let Err(e) = self.events.try_send(ty::CONFIGURE, &body, &[]) {
            crate::warn!("display: configure of window {id} lost: {e}");
        }
    }

    fn create(&mut self, s: WindowSpec) -> i32 {
        use crate::wire::errno::*;
        if self.windows.contains_key(&s.window) {
            return -EBUSY;
        }
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let window = self
            .xdg
            .create_window(surface, WindowDecorations::RequestServer, &qh);
        window.set_title(s.title.clone());
        window.set_app_id(s.app_id.clone());
        window.set_min_size(Some((s.min_width.max(MIN_W), s.min_height.max(MIN_H))));
        let viewport = self
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(window.wl_surface(), &qh, Unit));
        let fractional = self
            .fractional
            .as_ref()
            .map(|f| f.get_fractional_scale(window.wl_surface(), &qh, ScaleOf(s.window)));
        let exported = foreign::export(self.exporter.as_ref(), window.wl_surface(), s.window, &qh);
        window.commit();
        crate::info!(
            "display: window {} \"{}\" ({}) {}x{}",
            s.window,
            s.title,
            s.app_id,
            s.width,
            s.height
        );
        self.windows.insert(
            s.window,
            Win {
                id: s.window,
                window,
                viewport,
                fractional,
                exported,
                frame: None,
                title: s.title,
                app_id: s.app_id,
                width: s.width,
                height: s.height,
                scale120: 120,
                configured: false,
                fullscreen: false,
                shown: None,
                shown_size: (0, 0),
            },
        );
        0
    }

    fn destroy(&mut self, id: u32) {
        let Some(w) = self.windows.remove(&id) else {
            return;
        };
        if let Some(v) = &w.viewport {
            v.destroy();
        }
        if let Some(f) = &w.fractional {
            f.destroy();
        }
        foreign::forget(id, w.exported.clone());
        // Buffers of this window go too; busy ones were in use by the
        // compositor, which keeps its own reference.
        let gone: Vec<u32> = self
            .buffers
            .iter()
            .filter(|(_, b)| b.window == id)
            .map(|(k, _)| *k)
            .collect();
        for b in gone {
            self.remove_buffer(b);
        }
        if self.pointer_focus.is_some_and(|(p, _)| p == id) {
            self.pointer_focus = None;
        }
        if self.keyboard_focus == Some(id) {
            self.keyboard_focus = None;
        }
        self.touch_focus.retain(|_, w| *w != id);
        self.text.forget(id);
        crate::info!("display: window {id} closed");
        drop(w);
    }

    fn add_buffer(&mut self, b: BufferSpec, fd: OwnedFd) {
        if self.buffers.contains_key(&b.buffer) || !self.windows.contains_key(&b.window) {
            return;
        }
        let params = match self.dmabuf.create_params(&self.qh) {
            Ok(p) => p,
            Err(e) => {
                crate::warn!("display: no linux-dmabuf: {e}");
                return;
            }
        };
        params.add(fd.as_fd(), 0, b.offset, b.stride, b.modifier);
        let p = params.create(
            b.width as i32,
            b.height as i32,
            b.fourcc,
            zwp_linux_buffer_params_v1::Flags::empty(),
        );
        self.params.insert(p.id(), b.buffer);
        self.buffers.insert(
            b.buffer,
            Buf {
                window: b.window,
                width: b.width,
                height: b.height,
                wl: None,
                busy: false,
            },
        );
        // `fd` closes here; the compositor has its own copy once the request
        // is flushed (libwayland dups fds it sends).
        drop(fd);
    }

    fn remove_buffer(&mut self, id: u32) {
        if let Some(b) = self.buffers.remove(&id)
            && let Some(wl) = b.wl
        {
            wl.destroy();
        }
    }

    fn frame(&mut self, id: u32, buffer: u32, damage: [i32; 4]) {
        let ready = self
            .buffers
            .get(&buffer)
            .is_some_and(|b| b.window == id && b.wl.is_some() && !b.busy);
        let configured = self.windows.get(&id).is_some_and(|w| w.configured);
        if !ready || !configured {
            // Not shown: hand the buffer straight back.
            self.release(buffer);
            return;
        }
        let b = self.buffers.get_mut(&buffer).unwrap();
        b.busy = true;
        let (bw, bh) = (b.width, b.height);
        let wl = b.wl.clone().unwrap();
        let w = self.windows.get_mut(&id).unwrap();
        let surface = w.window.wl_surface().clone();
        surface.attach(Some(&wl), 0, 0);
        let [x, y, dw, dh] = damage;
        if dw > 0 && dh > 0 {
            surface.damage_buffer(x, y, dw, dh);
        } else {
            surface.damage_buffer(0, 0, bw as i32, bh as i32);
        }
        if let Some(v) = &w.viewport {
            v.set_destination(w.width as i32, w.height as i32);
        }
        w.shown = Some(buffer);
        w.shown_size = (bw, bh);
        if let Some(f) = w.frame.as_mut()
            && f.is_dirty()
            && !f.is_hidden()
        {
            f.draw();
        }
        surface.commit();
    }

    fn activate(&self, id: u32) {
        let (Some(a), Some(w)) = (&self.activation, self.windows.get(&id)) else {
            return;
        };
        let t = a.get_activation_token(&self.qh, TokenFor(id));
        if let Some(seat) = &self.seat
            && self.last_serial != 0
        {
            t.set_serial(self.last_serial, seat);
        }
        if let Some(f) = self.keyboard_focus.and_then(|f| self.windows.get(&f)) {
            t.set_surface(f.window.wl_surface());
        }
        t.set_app_id(w.app_id.clone());
        t.commit();
    }

    fn command(&mut self, c: Cmd) {
        match c {
            Cmd::Create(s, reply) => {
                let r = self.create(s);
                let _ = reply.send(r);
            }
            Cmd::Destroy(id) => self.destroy(id),
            Cmd::Title(id, t) => {
                if let Some(w) = self.windows.get_mut(&id) {
                    w.window.set_title(t.clone());
                    if let Some(f) = w.frame.as_mut() {
                        f.set_title(t.clone());
                    }
                    w.title = t;
                }
            }
            Cmd::MinSize(id, mw, mh) => {
                if let Some(w) = self.windows.get(&id) {
                    w.window.set_min_size(Some((mw.max(MIN_W), mh.max(MIN_H))));
                    w.window.commit();
                }
            }
            Cmd::Activate(id) => self.activate(id),
            Cmd::BufferAdd(b, fd) => self.add_buffer(b, fd),
            Cmd::BufferRemove(id) => self.remove_buffer(id),
            Cmd::Frame {
                window,
                buffer,
                damage,
            } => self.frame(window, buffer, damage),
            Cmd::TextInput(r) => self.text_input(r),
            Cmd::Quit => self.quit = true,
        }
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window) {
        if let Some(w) = self.windows.values().find(|w| &w.window == window) {
            let _ = self.events.try_send(ty::CLOSE, &event::close(w.id), &[]);
        }
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        window: &Window,
        c: WindowConfigure,
        _: u32,
    ) {
        let Some(id) = self
            .windows
            .values()
            .find(|w| &w.window == window)
            .map(|w| w.id)
        else {
            return;
        };
        let (shm, compositor, subcompositor) = (
            &self.shm,
            self.compositor.clone(),
            self.subcompositor.clone(),
        );
        let w = self.windows.get_mut(&id).unwrap();
        let (width, height) = if c.decoration_mode == DecorationMode::Client {
            if w.frame.is_none() {
                match AdwaitaFrame::new(
                    &w.window,
                    shm,
                    compositor,
                    subcompositor,
                    qh.clone(),
                    FrameConfig::auto(),
                ) {
                    Ok(mut f) => {
                        f.set_title(w.title.clone());
                        f.set_scaling_factor(w.scale120 as f64 / 120.0);
                        w.frame = Some(f);
                    }
                    Err(e) => crate::warn!("display: no client-side frame: {e}"),
                }
            }
            let (cw, ch) = (
                NonZeroU32::new(w.width).unwrap_or(NonZeroU32::MIN),
                NonZeroU32::new(w.height).unwrap_or(NonZeroU32::MIN),
            );
            if let Some(f) = w.frame.as_mut() {
                f.set_hidden(false);
                f.update_state(c.state);
                f.update_wm_capabilities(c.capabilities);
                let (nw, nh) = match c.new_size {
                    (Some(nw), Some(nh)) => f.subtract_borders(nw, nh),
                    _ => (Some(cw), Some(ch)),
                };
                let (nw, nh) = (nw.unwrap_or(NonZeroU32::MIN), nh.unwrap_or(NonZeroU32::MIN));
                f.resize(nw, nh);
                let (x, y) = f.location();
                let (ow, oh) = f.add_borders(nw.get(), nh.get());
                w.window
                    .xdg_surface()
                    .set_window_geometry(x, y, ow as i32, oh as i32);
                (nw.get(), nh.get())
            } else {
                (
                    c.new_size.0.map_or(w.width, |v| v.get()),
                    c.new_size.1.map_or(w.height, |v| v.get()),
                )
            }
        } else {
            if let Some(f) = w.frame.as_mut() {
                f.set_hidden(true);
            }
            let nw = c.new_size.0.map_or(w.width, |v| v.get());
            let nh = c.new_size.1.map_or(w.height, |v| v.get());
            w.window
                .xdg_surface()
                .set_window_geometry(0, 0, nw as i32, nh as i32);
            (nw, nh)
        };
        w.width = width;
        w.height = height;
        w.configured = true;
        w.fullscreen = c.is_fullscreen();
        if let Some(v) = &w.viewport {
            v.set_destination(width as i32, height as i32);
        }
        if let Some(f) = w.frame.as_mut()
            && !f.is_hidden()
        {
            f.draw();
        }
        w.window.commit();
        self.send_configure(id);
    }
}

impl CompositorHandler for State {
    /// The integer scale (`wl_surface.preferred_buffer_scale`, or the
    /// outputs the surface is on). Only used where the compositor has no
    /// `wp_fractional_scale_v1`; the fractional scale wins otherwise.
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        let Some(w) = self
            .windows
            .values_mut()
            .find(|w| w.window.wl_surface() == surface && w.fractional.is_none())
        else {
            return;
        };
        let scale120 = factor.clamp(1, 8) as u32 * 120;
        if scale120 == w.scale120 {
            return;
        }
        w.scale120 = scale120;
        // Without a viewport, the buffer scale maps the frame's pixels to
        // the window's logical size.
        if w.viewport.is_none() {
            surface.set_buffer_scale(factor.clamp(1, 8));
        }
        if let Some(f) = w.frame.as_mut() {
            f.set_scaling_factor(factor as f64);
        }
        let id = w.id;
        self.send_configure(id);
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        cap: Capability,
    ) {
        if self.seat.is_none() {
            self.seat = Some(seat.clone());
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
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        cap: Capability,
    ) {
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
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl DmabufHandler for State {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf
    }
    fn dmabuf_feedback(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &zwp_linux_dmabuf_feedback_v1::ZwpLinuxDmabufFeedbackV1,
        _: DmabufFeedback,
    ) {
    }
    fn created(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        params: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
        buffer: wl_buffer::WlBuffer,
    ) {
        params.destroy();
        let Some(id) = self.params.remove(&params.id()) else {
            buffer.destroy();
            return;
        };
        // Re-home the buffer's events on our own user data (release).
        let _ = qh;
        match self.buffers.get_mut(&id) {
            Some(b) => b.wl = Some(buffer),
            None => buffer.destroy(),
        }
    }
    fn failed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        params: &zwp_linux_buffer_params_v1::ZwpLinuxBufferParamsV1,
    ) {
        params.destroy();
        if let Some(id) = self.params.remove(&params.id()) {
            crate::warn!("display: the compositor refused dma-buf {id}");
            self.buffers.remove(&id);
        }
    }
    fn released(&mut self, _: &Connection, _: &QueueHandle<Self>, buffer: &wl_buffer::WlBuffer) {
        let id = self
            .buffers
            .iter()
            .find(|(_, b)| b.wl.as_ref() == Some(buffer))
            .map(|(k, _)| *k);
        if let Some(id) = id
            && let Some(b) = self.buffers.get_mut(&id)
            && b.busy
        {
            b.busy = false;
            self.release(id);
        }
    }
}

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(State);
delegate_dispatch2!(State);

fn bind_opt<I: Proxy + 'static>(
    globals: &GlobalList,
    qh: &QueueHandle<State>,
    max: u32,
) -> Option<I>
where
    Unit: Dispatch2<I, State>,
{
    globals.bind(qh, 1..=max, Unit).ok()
}

pub(super) fn foreign_handles(window: u32) -> Option<String> {
    foreign::HANDLES
        .lock()
        .unwrap()
        .as_ref()?
        .get(&window)
        .cloned()
}

/// Runs the Wayland side until [`Cmd::Quit`] or the compositor goes away.
pub fn run(conn: Connection, commands: Channel<Cmd>, events: Events) -> Result<()> {
    let (globals, queue) = registry_queue_init::<State>(&conn).context("registry")?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor")?;
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
        .context("wl_subcompositor")?;
    let mut st = State {
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor: Arc::new(compositor),
        subcompositor: Arc::new(subcompositor),
        shm: Shm::bind(&globals, &qh).context("wl_shm")?,
        xdg: XdgShell::bind(&globals, &qh).context("xdg_wm_base")?,
        dmabuf: DmabufState::new(&globals, &qh),
        viewporter: bind_opt(&globals, &qh, 1),
        fractional: bind_opt(&globals, &qh, 1),
        activation: bind_opt(&globals, &qh, 1),
        exporter: bind_opt(&globals, &qh, 1),
        qh: qh.clone(),
        events,
        windows: HashMap::new(),
        buffers: HashMap::new(),
        params: HashMap::new(),
        seat: None,
        pointer: None,
        keyboard: None,
        touch: None,
        pointer_focus: None,
        pointer_surface: None,
        keyboard_focus: None,
        touch_focus: HashMap::new(),
        text: text_input::TextInput::new(bind_opt(&globals, &qh, 1)),
        last_serial: 0,
        quit: false,
    };
    crate::info!(
        "display: connected to the compositor (dmabuf v{:?}, viewporter {}, fractional-scale {}, activation {}, text-input {})",
        st.dmabuf.version(),
        st.viewporter.is_some(),
        st.fractional.is_some(),
        st.activation.is_some(),
        st.text.available()
    );
    let mut ev: EventLoop<State> = EventLoop::try_new().context("event loop")?;
    WaylandSource::new(conn, queue)
        .insert(ev.handle())
        .map_err(|e| anyhow::anyhow!("wayland source: {e}"))?;
    ev.handle()
        .insert_source(commands, |e, _, st: &mut State| match e {
            ChannelEvent::Msg(c) => st.command(c),
            ChannelEvent::Closed => st.quit = true,
        })
        .map_err(|e| anyhow::anyhow!("command source: {e}"))?;
    while !st.quit {
        ev.dispatch(None, &mut st).context("dispatch")?;
    }
    for id in st.windows.keys().copied().collect::<Vec<_>>() {
        st.destroy(id);
    }
    Ok(())
}
