//! The display service's Wayland side: one host Wayland connection per
//! composer connection, one `xdg_toplevel` per window and dma-buf frames.
//! The connection's seat is the input service's (`input::seat`).
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
use sctk::reexports::client::backend::ObjectId;
use sctk::reexports::client::globals::{GlobalList, registry_queue_init};
use sctk::reexports::client::protocol::{wl_buffer, wl_output, wl_seat, wl_surface};
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
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

use super::proto::{BufferSpec, event, ty};
use crate::input::proto::{WindowPolicy, WindowRequest};
use crate::input::{Hub, ime, seat::Seat};
use crate::ipc::server::Events;

mod data_device;
mod foreign;

/// Work for the Wayland thread.
pub enum Cmd {
    Create(WindowRequest, std::sync::mpsc::Sender<i32>),
    Destroy(u32),
    /// Activate a window, with this activation token if there is one.
    Activate(u32, Option<String>),
    /// Return a token for a portal launch rather than activating our window.
    ExportToken(u32, std::sync::mpsc::Sender<String>),
    /// A buffer of the window's display.
    BufferAdd(BufferSpec, u32, OwnedFd),
    BufferRemove(u32),
    Frame {
        window: u32,
        buffer: u32,
        damage: [i32; 4],
    },
    /// A request from the Linux.apk IME.
    TextInput(ime::Req),
    WindowPolicy(WindowPolicy),
    Quit,
}

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
    constrained: bool,
    app_immersive: bool,
    manual_fullscreen_choice: bool,
    /// The last requested Android orientation; manual Rotate does not change it.
    orientation: u32,
    /// True only if Android, rather than the user, entered fullscreen.
    app_fullscreen: bool,
    app_requested_fullscreen: bool,
    orientation_pending: bool,
    /// Last windowed content size, restored if the compositor leaves size unspecified.
    windowed_size: (u32, u32),
    /// Buffer shown now (its size maps input), and the last one asked for.
    shown: Option<u32>,
    shown_size: (u32, u32),
    /// The launch's activation token, used with the first frame.
    token: Option<String>,
}

pub(crate) struct State {
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
    /// The composer's connection (buffer releases).
    events: Events,
    /// Window and input lines go to the input service's clients.
    pub(crate) hub: Arc<Hub>,

    windows: HashMap<u32, Win>,
    buffers: HashMap<u32, Buf>,
    /// zwp_linux_buffer_params_v1 being created -> buffer id.
    params: HashMap<ObjectId, u32>,
    pub(crate) seat: Seat,
    pub(crate) clipboard: Option<data_device::Clipboard>,
    quit: bool,
}

// ---- globals we bind ourselves ------------------------------------------------

/// User data for objects whose events we ignore or handle inline.
pub(crate) struct Unit;

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
struct TokenFor(u32, Option<std::sync::mpsc::Sender<String>>);
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
            if let Some(reply) = &self.1 {
                let _ = reply.send(token);
            } else if let (Some(a), Some(w)) = (&st.activation, st.windows.get(&self.0)) {
                a.activate(token, w.window.wl_surface());
            }
            t.destroy();
        }
    }
}

// ---- what the seat needs from the windows ------------------------------------

impl State {
    /// Which window `s` belongs to, and whether it is the window's frame.
    pub(crate) fn window_of(&self, s: &wl_surface::WlSurface) -> Option<(u32, bool)> {
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
    pub(crate) fn to_display(&self, id: u32, x: f64, y: f64) -> (f64, f64) {
        match self.windows.get(&id) {
            Some(w) if w.width > 0 && w.height > 0 && w.shown_size.0 > 0 => (
                x * w.shown_size.0 as f64 / w.width as f64,
                y * w.shown_size.1 as f64 / w.height as f64,
            ),
            _ => (x, y),
        }
    }

    /// The window's scale × 120.
    pub(crate) fn scale120(&self, id: u32) -> u32 {
        self.windows.get(&id).map_or(120, |w| w.scale120.max(1))
    }

    pub(crate) fn toggle_fullscreen(&mut self, id: u32) {
        if let Some(w) = self.windows.get_mut(&id) {
            w.app_fullscreen = false;
            w.manual_fullscreen_choice = true;
            if w.fullscreen {
                w.window.unset_fullscreen();
            } else {
                w.window.set_fullscreen(None);
            }
        }
    }

    /// A client chooses its floating size in xdg-shell; a later configure may override it.
    /// Android letterboxes fixed-orientation activities inside the resulting display buffer.
    pub(crate) fn rotate(&mut self, id: u32) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if w.fullscreen || w.constrained {
            return;
        }
        std::mem::swap(&mut w.width, &mut w.height);
        w.windowed_size = (w.width, w.height);
        if let Some(f) = w.frame.as_mut().filter(|f| !f.is_hidden()) {
            f.resize(
                NonZeroU32::new(w.width).unwrap(),
                NonZeroU32::new(w.height).unwrap(),
            );
            let (x, y) = f.location();
            let (width, height) = f.add_borders(w.width, w.height);
            w.window
                .xdg_surface()
                .set_window_geometry(x, y, width as i32, height as i32);
            f.draw();
        } else {
            w.window
                .xdg_surface()
                .set_window_geometry(0, 0, w.width as i32, w.height as i32);
        }
        if let Some(v) = &w.viewport {
            v.set_destination(w.width as i32, w.height as i32);
        }
        w.window.commit();
        self.send_configure(id);
    }

    fn window_policy(&mut self, p: WindowPolicy) {
        let Some(w) = self.windows.get_mut(&p.window) else {
            return;
        };
        if p.immersive && !w.app_immersive {
            w.manual_fullscreen_choice = false;
        }
        w.app_immersive = p.immersive;
        if p.fullscreen != w.app_requested_fullscreen && !w.manual_fullscreen_choice {
            if p.fullscreen && !w.fullscreen {
                w.app_fullscreen = true;
                w.window.set_fullscreen(None);
            } else if !p.fullscreen && w.app_fullscreen {
                w.app_fullscreen = false;
                w.window.unset_fullscreen();
            }
        }
        w.app_requested_fullscreen = p.fullscreen;
        if w.orientation != p.orientation {
            w.orientation = p.orientation;
            w.orientation_pending = true;
        }
        self.apply_orientation(p.window);
    }

    fn apply_orientation(&mut self, id: u32) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if !w.orientation_pending || w.fullscreen || w.constrained || w.app_fullscreen {
            return;
        }
        w.orientation_pending = false;
        if matches!((w.orientation, w.width > w.height), (1, true) | (2, false)) {
            self.rotate(id);
        }
    }

    // The pointer over a window's client-side frame drives the frame.

    /// Returns the cursor the frame wants.
    pub(crate) fn frame_pointer_moved(
        &mut self,
        id: u32,
        surface: &ObjectId,
        time: Duration,
        x: f64,
        y: f64,
    ) -> Option<sctk::reexports::csd_frame::CursorIcon> {
        let f = self.windows.get_mut(&id)?.frame.as_mut()?;
        f.click_point_moved(time, surface, x, y)
    }

    pub(crate) fn frame_pointer_left(&mut self, id: u32) {
        if let Some(f) = self.windows.get_mut(&id).and_then(|w| w.frame.as_mut()) {
            f.click_point_left();
        }
    }

    pub(crate) fn frame_click(
        &mut self,
        id: u32,
        time: Duration,
        button: u32,
        pressed: bool,
        serial: u32,
    ) {
        let click = match button {
            0x110 => FrameClick::Normal,
            0x111 => FrameClick::Alternate,
            _ => return,
        };
        let action = self
            .windows
            .get_mut(&id)
            .and_then(|w| w.frame.as_mut())
            .and_then(|f| f.on_click(time, click, pressed));
        if let Some(a) = action {
            self.frame_action(id, a, serial);
        }
    }

    fn frame_action(&mut self, id: u32, action: FrameAction, serial: u32) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let Some(seat) = self.seat.wl_seat.clone() else {
            return;
        };
        match action {
            FrameAction::Close => self.hub.input(&format!("close {id}")),
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
        if w.configured {
            let scale = w.scale120 as f64 / 120.0;
            self.hub
                .input(&format!("window {} {} {scale:.3} {id}", w.width, w.height));
        }
    }

    fn create(&mut self, s: WindowRequest) -> i32 {
        use crate::ipc::wire::errno::*;
        if self.windows.contains_key(&s.window) {
            return -EBUSY;
        }
        let (width, height) = s.logical_size();
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
            "display: window {} \"{}\" ({}) {width}x{height}",
            s.window,
            s.title,
            s.app_id,
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
                width,
                height,
                scale120: 120,
                configured: false,
                fullscreen: false,
                constrained: false,
                app_immersive: false,
                manual_fullscreen_choice: false,
                orientation: 0,
                app_fullscreen: false,
                app_requested_fullscreen: false,
                orientation_pending: false,
                windowed_size: (width, height),
                shown: None,
                shown_size: (0, 0),
                token: (!s.token.is_empty()).then_some(s.token),
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
        self.seat.forget(id, &self.hub);
        crate::info!("display: window {id} closed");
        drop(w);
    }

    fn add_buffer(&mut self, b: BufferSpec, window: u32, fd: OwnedFd) {
        if self.buffers.contains_key(&b.buffer) || !self.windows.contains_key(&window) {
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
                window,
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
        let first = w.shown.is_none();
        w.shown = Some(buffer);
        w.shown_size = (bw, bh);
        if let Some(f) = w.frame.as_mut()
            && f.is_dirty()
            && !f.is_hidden()
        {
            f.draw();
        }
        surface.commit();
        if first {
            // Mapped now: the launch's token can focus it.
            if let (Some(t), Some(a)) = (w.token.take(), &self.activation) {
                a.activate(t, &surface);
            }
            crate::info!("display: window {id} shows its first frame");
            self.hub.first_frame(id);
        }
    }

    /// Activates a window with `token`, or else with a token for the
    /// newest input serial (which compositors may only grant as "demands
    /// attention").
    fn activate(&self, id: u32, token: Option<String>) {
        let (Some(a), Some(w)) = (&self.activation, self.windows.get(&id)) else {
            return;
        };
        if let Some(t) = token {
            a.activate(t, w.window.wl_surface());
            return;
        }
        self.token(id, None);
    }

    fn token(&self, id: u32, reply: Option<std::sync::mpsc::Sender<String>>) {
        let Some(a) = &self.activation else { return };
        let internal = reply.is_none();
        let t = a.get_activation_token(&self.qh, TokenFor(id, reply));
        if let Some(seat) = &self.seat.wl_seat
            && self.seat.last_serial != 0
        {
            t.set_serial(self.seat.last_serial, seat);
        }
        if let Some(f) = self.seat.keyboard_focus.and_then(|f| self.windows.get(&f)) {
            t.set_surface(f.window.wl_surface());
        }
        if internal && let Some(w) = self.windows.get(&id) {
            t.set_app_id(w.app_id.clone());
        }
        t.commit();
    }

    fn command(&mut self, c: Cmd) {
        match c {
            Cmd::Create(s, reply) => {
                let r = self.create(s);
                let _ = reply.send(r);
            }
            Cmd::Destroy(id) => self.destroy(id),
            Cmd::Activate(id, token) => self.activate(id, token),
            Cmd::ExportToken(id, reply) => self.token(id, Some(reply)),
            Cmd::BufferAdd(b, window, fd) => self.add_buffer(b, window, fd),
            Cmd::BufferRemove(id) => self.remove_buffer(id),
            Cmd::Frame {
                window,
                buffer,
                damage,
            } => self.frame(window, buffer, damage),
            Cmd::TextInput(r) => self.text_input(r),
            Cmd::WindowPolicy(p) => self.window_policy(p),
            Cmd::Quit => self.quit = true,
        }
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, window: &Window) {
        if let Some(w) = self.windows.values().find(|w| &w.window == window) {
            self.hub.input(&format!("close {}", w.id));
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
        if w.fullscreen && !c.is_fullscreen() {
            if w.app_fullscreen {
                w.manual_fullscreen_choice = true;
            }
            w.app_fullscreen = false;
            (w.width, w.height) = w.windowed_size;
        } else if !w.fullscreen && c.is_fullscreen() {
            w.windowed_size = (w.width, w.height);
        }
        let (width, height) = if c.decoration_mode == DecorationMode::Client && !c.is_fullscreen() {
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
        w.constrained = c.is_maximized() || c.is_tiled();
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
        self.apply_orientation(id);
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
        if let Some(c) = &mut self.clipboard {
            c.seat(&seat, qh);
        }
        self.seat.add_capability(seat, cap, qh);
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        cap: Capability,
    ) {
        self.seat.remove_capability(cap);
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
pub fn run(
    conn: Connection,
    commands: Channel<Cmd>,
    events: Events,
    hub: Arc<Hub>,
    shared: Arc<crate::system::Shared>,
) -> Result<()> {
    let (globals, queue) = registry_queue_init::<State>(&conn).context("registry")?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor")?;
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
        .context("wl_subcompositor")?;
    let mut ev: EventLoop<State> = EventLoop::try_new().context("event loop")?;
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
        hub,
        windows: HashMap::new(),
        buffers: HashMap::new(),
        params: HashMap::new(),
        seat: Seat::new(bind_opt(&globals, &qh, 1), &globals, &qh),
        clipboard: data_device::Clipboard::new(&globals, &qh, &shared, &ev)?,
        quit: false,
    };
    crate::info!(
        "display: connected to the compositor (dmabuf v{:?}, viewporter {}, fractional-scale {}, activation {}, text-input {})",
        st.dmabuf.version(),
        st.viewporter.is_some(),
        st.fractional.is_some(),
        st.activation.is_some(),
        st.seat.text.available()
    );
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
