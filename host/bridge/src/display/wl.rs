//! The display service's Wayland side. There is one host Wayland connection
//! per composer connection and one `xdg_toplevel` per window, and frames
//! arrive as dma-bufs. The connection's seat belongs to the input service
//! (`input::seat`).
//!
//! Windows get an Adwaita-style client-side frame on every desktop. Everything
//! runs on one thread with a calloop event loop, and the service sends it
//! [`Cmd`]s over a channel.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

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
use sctk::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use sctk::shell::xdg::{XdgShell, XdgSurface};
use sctk::shm::{Shm, ShmHandler};
use sctk::subcompositor::SubcompositorState;
use sctk::{delegate_dispatch2, delegate_registry, dispatch2::Dispatch2, registry_handlers};
use sctk_adwaita::{Action, AdwaitaFrame};
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
use crate::input::proto::WindowPolicy;
use crate::input::{Hub, ime, seat::Seat};
use crate::ipc::server::Events;
use wayland_protocols::wp::idle_inhibit::zv1::client::{
    zwp_idle_inhibit_manager_v1::ZwpIdleInhibitManagerV1, zwp_idle_inhibitor_v1::ZwpIdleInhibitorV1,
};
use wayland_protocols::xdg::toplevel_icon::v1::client::{
    xdg_toplevel_icon_manager_v1::XdgToplevelIconManagerV1, xdg_toplevel_icon_v1::XdgToplevelIconV1,
};

pub(crate) mod appearance;
pub(crate) mod cursor;
mod data_device;
mod foreign;
mod formats;
mod frames;
mod inhibit;
mod policy;
mod presentation;
mod scale;
mod visibility;

use policy::{AppRequest, Event as PolicyEvent, Policy, WindowState, desired_window_state};

/// Work for the Wayland thread.
pub enum Cmd {
    Create(crate::input::PendingWindow, std::sync::mpsc::Sender<i32>),
    Destroy(u32),
    WindowAppearance {
        window: u32,
        color: u32,
        icon: Option<crate::ipc::blob::Image>,
    },
    /// Activates a window, with the activation token if there is one.
    Activate(u32, Option<String>),
    Minimize(u32),
    /// Returns an activation token for a portal launch instead of activating
    /// our window.
    ExportToken(u32, std::sync::mpsc::Sender<String>),
    /// A buffer of the window's display.
    BufferAdd(BufferSpec, u32, OwnedFd),
    BufferRemove(u32),
    Frame {
        window: u32,
        buffer: u32,
        damage: [i32; 4],
    },
    /// A request from the Innkeeper IME.
    TextInput(ime::Req),
    PointerCapture(bool),
    WindowPolicy(WindowPolicy),
    /// WINDOW_ROTATION: window, quarter turns.
    Rotation(u32, u32),
    KeepAwake {
        window: u32,
        requested: bool,
    },
    Cursor {
        window: u32,
        image: cursor::Image,
    },
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
    fallback: bool,
    /// Attached and not yet released by the compositor.
    frames: frames::Frames,
    source: OwnedFd,
    spec: BufferSpec,
    bench_offset: u64,
}

struct Win {
    id: u32,
    window: Window,
    /// Android's rotation of the display's content in its buffer, in quarter
    /// turns (WINDOW_ROTATION). The buffer transform turns it back.
    rotation: u32,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    /// xdg-foreign export, for portal dialogs parented to the window.
    exported:
        Option<wayland_protocols::xdg::foreign::zv2::client::zxdg_exported_v2::ZxdgExportedV2>,
    frame: Option<AdwaitaFrame<State>>,
    color: u32,
    icon: Option<crate::ipc::blob::Image>,
    title: String,
    app_id: String,
    /// Logical content size and scale×120.
    width: u32,
    height: u32,
    scale: scale::Scale,
    reported_outputs: Option<Vec<(u32, u32)>>,
    inhibit: inhibit::Inhibit,
    inhibitor: Option<ZwpIdleInhibitorV1>,
    frozen: Option<sctk::shm::slot::Buffer>,
    visibility: visibility::Visibility,
    configured: bool,
    mapped: bool,
    fullscreen: bool,
    constrained: bool,
    policy: Policy,
    /// Last windowed content size, restored if the compositor leaves size unspecified.
    windowed_size: (u32, u32),
    /// The buffer shown now and its size, which maps input to display pixels.
    shown: Option<u32>,
    shown_size: (u32, u32),
    /// The launch's activation token, used with the first frame.
    token: Option<String>,
}

pub(crate) struct State {
    registry: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    outputs_changed: bool,
    compositor: Arc<CompositorState>,
    subcompositor: Arc<SubcompositorState>,
    shm: Shm,
    xdg: XdgShell,
    dmabuf: DmabufState,
    formats: Vec<(u32, u64)>,
    viewporter: Option<WpViewporter>,
    presentation: Option<presentation::Presentation>,
    presentation_clock: presentation::Clock,
    fractional: Option<WpFractionalScaleManagerV1>,
    activation: Option<XdgActivationV1>,
    idle_inhibit: Option<ZwpIdleInhibitManagerV1>,
    icon_manager: Option<XdgToplevelIconManagerV1>,
    appearance: appearance::Appearance,
    exporter:
        Option<wayland_protocols::xdg::foreign::zv2::client::zxdg_exporter_v2::ZxdgExporterV2>,
    qh: QueueHandle<State>,
    /// The composer's connection, for buffer releases.
    events: Events,
    /// Sends window events and input lines to the input service's clients.
    pub(crate) hub: Arc<Hub>,

    windows: HashMap<u32, Win>,
    buffers: HashMap<u32, Buf>,
    /// zwp_linux_buffer_params_v1 being created -> buffer id.
    params: HashMap<ObjectId, u32>,
    pub(crate) seat: Seat,
    pub(crate) clipboard: Option<data_device::Clipboard>,
    cursors: HashMap<u32, cursor::Cursor>,
    cursor_surface: Option<(wl_surface::WlSurface, Option<WpViewport>)>,
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

macro_rules! ignore_events {
    ($($proxy:ty),*) => { $(
        impl Dispatch2<$proxy, State> for Unit {
            fn event(&self, _: &mut State, _: &$proxy, _: <$proxy as Proxy>::Event, _: &Connection, _: &QueueHandle<State>) {}
        }
    )* };
}
ignore_events!(
    XdgToplevelIconManagerV1,
    XdgToplevelIconV1,
    ZwpIdleInhibitManagerV1,
    ZwpIdleInhibitorV1
);

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
            && w.scale.update(scale)
        {
            if let Some(f) = w.frame.as_mut() {
                f.set_scaling_factor(w.scale.get() as f64 / 120.0);
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

    /// Maps surface-local logical coordinates to the shown frame's pixels.
    pub(crate) fn to_display(&self, id: u32, x: f64, y: f64) -> (f64, f64) {
        match self.windows.get(&id).and_then(pixel_ratio) {
            Some((rx, ry)) => (x * rx, y * ry),
            None => (x, y),
        }
    }

    /// Maps the shown frame's pixels to surface-local logical coordinates,
    /// the inverse of [`State::to_display`]. Before the first frame it divides
    /// by the window's scale.
    pub(crate) fn display_to_surface(&self, id: u32, x: f64, y: f64) -> (f64, f64) {
        let (rx, ry) = self
            .windows
            .get(&id)
            .and_then(pixel_ratio)
            .unwrap_or_else(|| {
                let s = f64::from(self.scale120(id)) / 120.0;
                (s, s)
            });
        (x / rx, y / ry)
    }

    /// The window's scale × 120.
    pub(crate) fn scale120(&self, id: u32) -> u32 {
        self.windows.get(&id).map_or(120, |w| w.scale.get())
    }

    pub(crate) fn toggle_fullscreen(&mut self, id: u32) {
        self.apply_window_policy(id, PolicyEvent::ToggleFullscreen);
    }

    pub(crate) fn rotate(&mut self, id: u32) {
        self.apply_window_policy(id, PolicyEvent::Rotate);
    }

    /// Swaps the window's width and height. A client chooses its own size
    /// only while floating, so the policy swaps only then.
    fn swap_shape(&mut self, id: u32) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
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

    /// Shows the window's frames turned back by `rotation` quarter turns and
    /// gives the window the turned shape. `rotation` is Android's rotation of
    /// a display whose buffer keeps its natural orientation.
    fn rotation(&mut self, id: u32, rotation: u32) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if w.rotation == rotation {
            return;
        }
        let swap = w.rotation % 2 != rotation % 2;
        w.rotation = rotation;
        w.window
            .wl_surface()
            .set_buffer_transform(buffer_transform(rotation));
        if swap {
            self.swap_shape(id);
        } else {
            w.window.commit();
        }
    }

    fn window_policy(&mut self, p: WindowPolicy) {
        self.apply_window_policy(
            p.window,
            PolicyEvent::App(AppRequest {
                shape: p.orientation,
            }),
        );
    }

    fn apply_window_policy(&mut self, id: u32, event: PolicyEvent) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        let compositor = WindowState {
            fullscreen: w.fullscreen,
            shape: if w.width <= w.height { 1 } else { 2 },
            constrained: w.constrained,
        };
        let (policy, desired, fullscreen_request) =
            desired_window_state(w.policy, event, compositor);
        w.policy = policy;
        if let Some(fullscreen) = fullscreen_request {
            if fullscreen {
                w.window.set_fullscreen(None);
            } else {
                w.window.unset_fullscreen();
            }
        }
        if desired.shape != 0 && desired.shape != compositor.shape {
            self.swap_shape(id);
        }
    }

    // Pointer and touch contacts share the client-side frame interaction handlers.

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

    pub(crate) fn frame_touch_point(&mut self, id: u32) {
        if let Some(f) = self.windows.get_mut(&id).and_then(|w| w.frame.as_mut()) {
            f.reset_touch_point();
        }
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
            .and_then(|f| f.on_frame_click(time, click, pressed));
        match action {
            Some(Action::ToggleFullscreen) => self.toggle_fullscreen(id),
            Some(Action::Back) => self.hub.input(&format!("back {id}")),
            Some(Action::Window(a)) => self.frame_action(id, a, serial),
            None => {}
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

impl State {
    fn release(&self, buffer: u32, invalid: bool) {
        if let Err(e) = self
            .events
            .try_send(ty::RELEASE, &event::release(buffer, invalid), &[])
        {
            crate::warn!("display: release of buffer {buffer} lost: {e}");
            self.events.shutdown();
        }
    }

    fn sync_inhibit(&mut self, id: u32) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        match w
            .inhibit
            .update(w.visibility.visible, self.idle_inhibit.is_some())
        {
            Some(true) => {
                w.inhibitor = self
                    .idle_inhibit
                    .as_ref()
                    .map(|m| m.create_inhibitor(w.window.wl_surface(), &self.qh, Unit));
                crate::info!("display: window {id} idle-inhibit on");
            }
            Some(false) => {
                if let Some(inhibitor) = w.inhibitor.take() {
                    inhibitor.destroy();
                }
                crate::info!("display: window {id} idle-inhibit off");
            }
            None => {}
        }
    }

    // Every visibility transition arrives here, whether from a suspended
    // configure, a missing callback or a restore. Output reporting also
    // reconciles the primary scheduling clock from the same visible outputs.
    fn send_visibility(&mut self, id: u32) {
        self.sync_inhibit(id);
        if self.windows.get(&id).is_some_and(|w| !w.visibility.visible) {
            self.freeze(id);
        }
        if let Some(w) = self.windows.get(&id) {
            self.hub.input(&format!(
                "window_visible {} {id}",
                u8::from(w.visibility.visible)
            ));
        }
        self.send_output(id);
    }

    // An ImageReader lease must not go back while Wayland still reads the
    // buffer. Replacing the last dma-buf with a shm snapshot lets
    // wl_buffer.release retire every image even after Android stops producing
    // frames. The surface stays mapped, and its outstanding frame callback
    // stays live to observe a later restore.
    fn freeze(&mut self, id: u32) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        let Some(b) = w.shown.and_then(|id| self.buffers.get(&id)) else {
            return;
        };
        if b.fallback {
            return;
        } // Its host-owned snapshot is already attached.
        match visibility::snapshot(&b.source, &b.spec, &self.shm) {
            Ok(buffer) => {
                let surface = w.window.wl_surface();
                surface.attach(Some(buffer.wl_buffer()), 0, 0);
                surface.damage_buffer(0, 0, b.width as i32, b.height as i32);
                surface.commit();
                w.frozen = Some(buffer);
            }
            Err(e) => crate::warn!("display: snapshot of hidden window {id}: {e}"),
        }
    }

    fn probe_visibility(&mut self) {
        let now = Instant::now();
        let mut changed = Vec::new();
        for w in self.windows.values_mut().filter(|w| w.shown.is_some()) {
            let visible = w.visibility.visible;
            if w.visibility.tick(now) {
                w.visibility.requested(now);
                let surface = w.window.wl_surface();
                surface.frame(
                    &self.qh,
                    sctk::compositor::FrameCallbackData(surface.clone()),
                );
                if crate::log::debug()
                    && let Some(p) = &self.presentation
                {
                    let stamp = crate::log::now();
                    crate::log::trace(format_args!("probe ns={stamp} window={}", w.id));
                    p.feedback(
                        surface,
                        &self.qh,
                        presentation::Probe {
                            stamp,
                            window: w.id,
                        },
                    );
                }
                surface.commit();
            }
            if visible != w.visibility.visible {
                changed.push(w.id);
            }
        }
        for id in changed {
            self.send_visibility(id);
        }
    }

    /// The window's outputs, shared by output reporting and presentation
    /// pacing. It is SCTK's full output membership, or empty while the window
    /// is hidden or unmapped. Callers read it on lifecycle and output events,
    /// so it needs no polling.
    pub(crate) fn visible_outputs(&self, id: u32) -> Vec<wl_output::WlOutput> {
        self.windows
            .get(&id)
            .filter(|w| w.mapped && w.visibility.visible)
            .and_then(|w| {
                w.window
                    .wl_surface()
                    .data::<sctk::compositor::SurfaceData<()>>()
            })
            .into_iter()
            .flat_map(|surface| surface.outputs())
            .filter(|output| self.output_state.info(output).is_some())
            .collect()
    }

    fn send_output(&mut self, id: u32) {
        self.window_timing(id);
        let mut outputs: Vec<_> = self
            .visible_outputs(id)
            .iter()
            .map(|o| (o.id().protocol_id(), self.output_rate(o)))
            .collect();
        outputs.sort_unstable();
        outputs.dedup();
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if w.reported_outputs.as_ref() == Some(&outputs) {
            return;
        }
        let mut line = format!("window_outputs {id}");
        for (output, rate) in &outputs {
            use std::fmt::Write;
            let _ = write!(line, " {output} {rate}");
        }
        crate::info!("display: {line}");
        self.hub.input(&line);
        w.reported_outputs = Some(outputs);
    }

    fn output_rate(&self, output: &wl_output::WlOutput) -> u32 {
        self.output_state
            .info(output)
            .and_then(|info| {
                info.modes
                    .into_iter()
                    .find(|m| m.current)
                    .and_then(|m| u32::try_from(m.refresh_rate).ok())
            })
            .unwrap_or(0)
    }

    fn report_outputs(&mut self) {
        // SCTK tracks output membership. Only changes are sent to Android.
        let ids: Vec<_> = self.windows.keys().copied().collect();
        for id in ids {
            self.send_output(id);
        }
    }

    fn send_configure(&self, id: u32) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        if w.configured {
            let scale = w.scale.get() as f64 / 120.0;
            self.hub
                .input(&format!("window {} {} {scale:.3} {id}", w.width, w.height));
        }
    }

    fn create(&mut self, pending: crate::input::PendingWindow) -> i32 {
        let crate::input::PendingWindow {
            request: s,
            color,
            icon,
        } = pending;
        use crate::ipc::wire::errno::*;
        if self.windows.contains_key(&s.window) {
            return -EBUSY;
        }
        let (width, height) = s.logical_size();
        let qh = self.qh.clone();
        let surface = self.compositor.create_surface(&qh);
        let window = self
            .xdg
            .create_window(surface, WindowDecorations::ClientOnly, &qh);
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
                rotation: 0,
                viewport,
                fractional,
                exported,
                frame: None,
                color: 0,
                icon: None,
                title: s.title,
                app_id: s.app_id,
                width,
                height,
                scale: scale::Scale::default(),
                reported_outputs: None,
                inhibit: inhibit::Inhibit::default(),
                inhibitor: None,
                frozen: None,
                visibility: visibility::Visibility::new(Instant::now()),
                configured: false,
                mapped: false,
                fullscreen: false,
                constrained: false,
                policy: Policy::default(),
                windowed_size: (width, height),
                shown: None,
                shown_size: (0, 0),
                token: (!s.token.is_empty()).then_some(s.token),
            },
        );
        self.window_appearance(s.window, color, icon);
        self.windows[&s.window].window.commit();
        0
    }

    pub(crate) fn update_capture(&mut self) {
        let pointer = self.seat.pointer_window();
        let surface = pointer
            .and_then(|id| self.windows.get(&id))
            .map(|w| w.window.wl_surface());
        self.seat.capture.sync(
            self.seat.keyboard_focus,
            pointer,
            surface,
            self.seat.pointer.as_ref(),
            &self.qh,
        );
    }

    fn destroy(&mut self, id: u32) {
        self.seat.capture.forget(id);
        self.cursors.remove(&id);
        self.hub.pointer_positions.lock().unwrap().remove(&id);
        let Some(w) = self.windows.remove(&id) else {
            return;
        };
        self.window_timing(id);
        if let Some(v) = &w.viewport {
            v.destroy();
        }
        if let Some(f) = &w.fractional {
            f.destroy();
        }
        if let Some(inhibitor) = &w.inhibitor {
            inhibitor.destroy();
        }
        foreign::forget(id, w.exported.clone());
        // The window's buffers go too. The compositor keeps its own reference
        // to any buffer it still uses.
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
        let advertised: Vec<_> = self
            .dmabuf
            .modifiers()
            .iter()
            .map(|f| (f.format, f.modifier))
            .chain(self.formats.iter().copied())
            .collect();
        let params = formats::direct(b.fourcc, b.modifier, &advertised)
            .then(|| self.dmabuf.create_params(&self.qh).ok())
            .flatten();
        let direct = params.is_some();
        if let Some(params) = params {
            params.add(fd.as_fd(), 0, b.offset, b.stride, b.modifier);
            let p = params.create(
                b.width as i32,
                b.height as i32,
                b.fourcc,
                zwp_linux_buffer_params_v1::Flags::empty(),
            );
            self.params.insert(p.id(), b.buffer);
        }
        crate::info!(
            "display: buffer {} format={:#x} modifier={:#x} {}",
            b.buffer,
            b.fourcc,
            b.modifier,
            if direct {
                "dma-buf"
            } else {
                "SDR shm fallback"
            }
        );
        self.buffers.insert(
            b.buffer,
            Buf {
                window,
                width: b.width,
                height: b.height,
                wl: None,
                fallback: !direct,
                frames: frames::Frames::default(),
                source: fd,
                spec: b.clone(),
                bench_offset: b.offset as u64
                    + (b.height / 2) as u64 * b.stride as u64
                    + (b.width / 2) as u64 * 4,
            },
        );
        // Keep the fd for a host-owned snapshot when the window goes hidden.
    }

    fn remove_buffer(&mut self, id: u32) {
        if let Some(mut b) = self.buffers.remove(&id) {
            for _ in 0..b.frames.release() {
                self.release(id, true);
            }
            if let Some(wl) = b.wl {
                wl.destroy();
            }
        }
    }

    fn frame(&mut self, id: u32, buffer: u32, damage: [i32; 4]) {
        self.cursor_frame(id);
        let configured = self
            .windows
            .get(&id)
            .is_some_and(|w| w.configured && w.visibility.visible);
        let b = self.buffers.get_mut(&buffer);
        let imported = b.as_ref().is_some_and(|b| b.wl.is_some() || b.fallback);
        let configured = configured && b.as_ref().is_none_or(|b| b.window == id);
        match frames::Frames::submit(b.map(|b| &mut b.frames), imported, configured, damage) {
            frames::Action::Release { invalid } => self.release(buffer, invalid),
            frames::Action::Wait => {}
            frames::Action::Present(damage) => self.present_frame(id, buffer, damage),
        }
    }

    fn present_frame(&mut self, id: u32, buffer: u32, damage: [i32; 4]) {
        if !self
            .windows
            .get(&id)
            .is_some_and(|w| w.configured && w.visibility.visible)
        {
            let count = self.buffers.get_mut(&buffer).unwrap().frames.release();
            for _ in 0..count {
                self.release(buffer, false);
            }
            return;
        }
        let b = self.buffers.get_mut(&buffer).unwrap();
        let (bw, bh) = (b.width, b.height);
        let snapshot = if b.fallback {
            match visibility::snapshot(&b.source, &b.spec, &self.shm) {
                Ok(buffer) => Some(buffer),
                Err(e) => {
                    crate::warn!("display: SDR fallback: {e}");
                    let count = b.frames.release();
                    for _ in 0..count {
                        self.release(buffer, true);
                    }
                    return;
                }
            }
        } else {
            None
        };
        let w = self.windows.get_mut(&id).unwrap();
        let surface = w.window.wl_surface().clone();
        if let Some(snapshot) = &snapshot {
            let _ = snapshot.attach_to(&surface);
        } else {
            surface.attach(b.wl.as_ref(), 0, 0);
        }
        let [x, y, dw, dh] = damage;
        if dw > 0 && dh > 0 {
            surface.damage_buffer(x, y, dw, dh);
        } else {
            surface.damage_buffer(0, 0, bw as i32, bh as i32);
        }
        if let Some(v) = &w.viewport {
            v.set_destination(w.width as i32, w.height as i32);
        }
        w.frozen = snapshot;
        let first = w.shown.is_none();
        w.shown = Some(buffer);
        w.shown_size = (bw, bh);
        if let Some(f) = w.frame.as_mut()
            && f.is_dirty()
            && !f.is_hidden()
        {
            f.draw();
        }
        let stamp = crate::log::now();
        if let Some(p) = &self.presentation {
            p.feedback(
                &surface,
                &self.qh,
                presentation::Frame {
                    stamp,
                    window: id,
                    generation: self.presentation_clock.generation(id),
                    output: Default::default(),
                },
            );
        }
        if crate::log::debug() {
            let mut pixel = [0u8; 4];
            let color = Some(&b.source).and_then(|fd| {
                use std::os::fd::AsRawFd;
                // dma-buf fds support mmap but not read or pread.
                let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
                let base = b.bench_offset / page * page;
                let within = (b.bench_offset - base) as usize;
                let len = within + 4;
                let ptr = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        len,
                        libc::PROT_READ,
                        libc::MAP_SHARED,
                        fd.as_raw_fd(),
                        base as libc::off_t,
                    )
                };
                if ptr == libc::MAP_FAILED {
                    return None;
                }
                unsafe {
                    std::ptr::copy_nonoverlapping(
                        ptr.cast::<u8>().add(within),
                        pixel.as_mut_ptr(),
                        4,
                    );
                    libc::munmap(ptr, len);
                }
                Some(u32::from_le_bytes(pixel) & 0xffffff)
            });
            crate::log::trace(format_args!(
                "commit ns={stamp} window={id} color={}",
                color.map_or(-1, i64::from)
            ));
        }
        if first {
            w.visibility.requested(Instant::now());
            surface.frame(
                &self.qh,
                sctk::compositor::FrameCallbackData(surface.clone()),
            );
        }
        surface.commit();
        let releases = if b.fallback { b.frames.release() } else { 0 };
        if first {
            // The window is mapped now, so the launch's token can focus it.
            if let (Some(t), Some(a)) = (w.token.take(), &self.activation) {
                a.activate(t, &surface);
            }
            crate::info!("display: window {id} shows its first frame");
        }
        for _ in 0..releases {
            self.release(buffer, false);
        }
        self.cursor_density(id);
    }

    /// Activates a window with `token`, or else with a new token for the
    /// newest input serial. Compositors may grant the latter only as "demands
    /// attention".
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
        let source = if internal {
            None
        } else {
            self.windows.get(&id)
        }
        .or_else(|| self.seat.keyboard_focus.and_then(|f| self.windows.get(&f)));
        if let Some(source) = source {
            t.set_surface(source.window.wl_surface());
        }
        if internal && let Some(w) = self.windows.get(&id) {
            t.set_app_id(w.app_id.clone());
        }
        t.commit();
    }

    fn window_appearance(&mut self, id: u32, color: u32, icon: Option<crate::ipc::blob::Image>) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        if w.color != color {
            w.color = color;
            if let Some(frame) = w.frame.as_mut() {
                frame.set_config(self.appearance.config(color));
                if !frame.is_hidden() {
                    frame.draw();
                }
            }
        }
        if w.icon != icon {
            if let Some(manager) = &self.icon_manager {
                if let Some(image) = &icon {
                    let (side, pixels) = appearance::icon_pixels(image);
                    // Compositors may map shm pools read/write, so IPC's
                    // sealed memfds can't back Wayland buffers. This pool is
                    // never reused.
                    let mut pool = match sctk::shm::slot::SlotPool::new(pixels.len(), &self.shm) {
                        Ok(pool) => pool,
                        Err(e) => {
                            crate::warn!("display: task icon pool: {e}");
                            return;
                        }
                    };
                    let (buffer, canvas) = match pool.create_buffer(
                        side,
                        side,
                        side * 4,
                        wayland_client::protocol::wl_shm::Format::Argb8888,
                    ) {
                        Ok(buffer) => buffer,
                        Err(e) => {
                            crate::warn!("display: task icon buffer: {e}");
                            return;
                        }
                    };
                    canvas.copy_from_slice(&pixels);
                    let object = manager.create_icon(&self.qh, Unit);
                    object.add_buffer(buffer.wl_buffer(), 1);
                    manager.set_icon(w.window.xdg_toplevel(), Some(&object));
                    object.destroy();
                } else {
                    manager.set_icon(w.window.xdg_toplevel(), None);
                }
            }
            w.icon = icon;
        }
    }

    fn command(&mut self, c: Cmd) {
        match c {
            Cmd::Create(s, reply) => {
                let r = self.create(s);
                let _ = reply.send(r);
            }
            Cmd::WindowAppearance {
                window,
                color,
                icon,
            } => {
                self.window_appearance(window, color, icon);
                if let Some(w) = self.windows.get(&window) {
                    w.window.commit();
                }
            }
            Cmd::Destroy(id) => self.destroy(id),
            Cmd::Minimize(id) => {
                if let Some(w) = self.windows.get(&id) {
                    w.window.set_minimized();
                }
            }
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
            Cmd::PointerCapture(enabled) => {
                self.seat.capture.state.request(enabled);
                self.update_capture();
            }
            Cmd::WindowPolicy(p) => self.window_policy(p),
            Cmd::Rotation(id, rotation) => self.rotation(id, rotation),
            Cmd::KeepAwake { window, requested } => {
                if let Some(w) = self.windows.get_mut(&window) {
                    w.inhibit.requested = requested;
                }
                self.sync_inhibit(window);
            }
            Cmd::Cursor { window, image } => self.cursor(window, image),
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
            (w.width, w.height) = w.windowed_size;
        } else if !w.fullscreen && c.is_fullscreen() {
            w.windowed_size = (w.width, w.height);
        }
        if w.frame.is_none() {
            match AdwaitaFrame::new(
                &w.window,
                shm,
                compositor,
                subcompositor,
                qh.clone(),
                self.appearance.config(w.color),
            ) {
                Ok(mut f) => {
                    f.set_title(w.title.clone());
                    f.set_scaling_factor(w.scale.get() as f64 / 120.0);
                    w.frame = Some(f);
                }
                Err(e) => crate::warn!("display: no client-side frame: {e}"),
            }
        }
        let (width, height) = if !c.is_fullscreen() {
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
            let nw = c.new_size.0.map_or(w.width, |v| v.get());
            let nh = c.new_size.1.map_or(w.height, |v| v.get());
            if let Some(f) = w.frame.as_mut() {
                f.set_hidden(false);
                f.update_state(c.state);
                f.update_wm_capabilities(c.capabilities);
                f.resize(
                    NonZeroU32::new(nw).unwrap_or(NonZeroU32::MIN),
                    NonZeroU32::new(nh).unwrap_or(NonZeroU32::MIN),
                );
            }
            w.window
                .xdg_surface()
                .set_window_geometry(0, 0, nw as i32, nh as i32);
            (nw, nh)
        };
        w.width = width;
        w.height = height;
        let was_visible = w.visibility.visible;
        w.visibility.configure(
            c.state
                .contains(sctk::reexports::csd_frame::WindowState::SUSPENDED),
            Instant::now(),
        );
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
        if was_visible != self.windows[&id].visibility.visible {
            self.send_visibility(id);
        }
        self.send_configure(id);
        self.apply_window_policy(id, PolicyEvent::Configure);
    }
}

impl CompositorHandler for State {
    /// Handles the integer scale from `wl_surface.preferred_buffer_scale` or
    /// from the outputs the surface is on. It applies only when the compositor
    /// lacks `wp_fractional_scale_v1`. Otherwise the fractional scale wins.
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
        if !w.scale.update(factor.clamp(1, 8) as u32 * 120) {
            return;
        }
        // Without a viewport, the buffer scale maps the frame's pixels to
        // the window's logical size.
        if w.viewport.is_none() {
            surface.set_buffer_scale(factor.clamp(1, 8));
        }
        if let Some(f) = w.frame.as_mut() {
            f.set_scaling_factor(w.scale.get() as f64 / 120.0);
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
    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(w) = self
            .windows
            .values_mut()
            .find(|w| w.window.wl_surface() == surface)
        {
            let was_visible = w.visibility.visible;
            w.visibility.callback();
            if !w.mapped {
                w.mapped = true;
                self.hub.first_frame(w.id);
            }
            let id = w.id;
            if was_visible != w.visibility.visible {
                self.send_visibility(id);
            }
            self.send_output(id);
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
        if let Some((id, false)) = self.window_of(surface) {
            self.send_output(id);
        }
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
        if let Some((id, false)) = self.window_of(surface) {
            self.send_output(id);
        }
    }
}

impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outputs_changed = true;
    }
    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outputs_changed = true;
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        self.outputs_changed = true;
    }
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
        feedback: DmabufFeedback,
    ) {
        self.formats = feedback
            .tranches()
            .iter()
            .flat_map(|t| &t.formats)
            .filter_map(|&index| feedback.format_table().get(index as usize))
            .map(|f| (f.format, f.modifier))
            .collect();
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
        let _ = qh;
        match self.buffers.get_mut(&id) {
            Some(b) => {
                b.wl = Some(buffer);
                if let Some(damage) = b.frames.imported() {
                    let window = b.window;
                    self.present_frame(window, id, damage);
                }
            }
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
            if let Some(b) = self.buffers.get_mut(&id) {
                b.fallback = true;
                if let Some(damage) = b.frames.imported() {
                    let window = b.window;
                    self.present_frame(window, id, damage);
                }
            }
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
        {
            let count = b.frames.release();
            for _ in 0..count {
                self.release(id, false);
            }
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

/// A frame's sides as shown, turned by `rotation` quarter turns.
/// The shown frame's pixels per logical unit of the window, across and down.
fn pixel_ratio(w: &Win) -> Option<(f64, f64)> {
    pixels_per_unit((w.width, w.height), w.shown_size, w.rotation)
}

fn pixels_per_unit(logical: (u32, u32), shown: (u32, u32), rotation: u32) -> Option<(f64, f64)> {
    if logical.0 == 0 || logical.1 == 0 || shown.0 == 0 {
        return None;
    }
    let (sw, sh) = turned(shown, rotation);
    Some((
        f64::from(sw) / f64::from(logical.0),
        f64::from(sh) / f64::from(logical.1),
    ))
}

fn turned((w, h): (u32, u32), rotation: u32) -> (u32, u32) {
    if rotation % 2 == 1 { (h, w) } else { (w, h) }
}

/// The buffer transform that shows a frame Android rotated by `rotation`
/// quarter turns (Surface.ROTATION_*) upright.
fn buffer_transform(rotation: u32) -> wl_output::Transform {
    // Android turns content counterclockwise. The Wayland transform names the
    // turn that undoes it.
    match rotation % 4 {
        1 => wl_output::Transform::_270,
        2 => wl_output::Transform::_180,
        3 => wl_output::Transform::_90,
        _ => wl_output::Transform::Normal,
    }
}

/// Runs the Wayland side until [`Cmd::Quit`] or the compositor goes away.
pub fn run(
    conn: Connection,
    commands: Channel<Cmd>,
    events: Events,
    hub: Arc<Hub>,
    shared: Arc<crate::system::Shared>,
) -> Result<()> {
    let (globals, mut queue) = registry_queue_init::<State>(&conn).context("registry")?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor")?;
    let subcompositor = SubcompositorState::bind(compositor.wl_compositor().clone(), &globals, &qh)
        .context("wl_subcompositor")?;
    let mut ev: EventLoop<State> = EventLoop::try_new().context("event loop")?;
    let mut st = State {
        registry: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        outputs_changed: false,
        compositor: Arc::new(compositor),
        subcompositor: Arc::new(subcompositor),
        shm: Shm::bind(&globals, &qh).context("wl_shm")?,
        xdg: XdgShell::bind(&globals, &qh).context("xdg_wm_base")?,
        dmabuf: DmabufState::new(&globals, &qh),
        formats: Vec::new(),
        viewporter: bind_opt(&globals, &qh, 1),
        presentation: bind_opt(&globals, &qh, 1),
        presentation_clock: presentation::Clock::default(),
        fractional: bind_opt(&globals, &qh, 1),
        activation: bind_opt(&globals, &qh, 1),
        idle_inhibit: bind_opt(&globals, &qh, 1),
        icon_manager: bind_opt(&globals, &qh, 1),
        appearance: appearance::Appearance::default(),
        exporter: bind_opt(&globals, &qh, 1),
        qh: qh.clone(),
        events,
        hub,
        windows: HashMap::new(),
        buffers: HashMap::new(),
        params: HashMap::new(),
        seat: Seat::new(bind_opt(&globals, &qh, 1), &globals, &qh),
        clipboard: data_device::Clipboard::new(&globals, &qh, &shared, &ev)?,
        cursors: HashMap::new(),
        cursor_surface: None,
        quit: false,
    };
    crate::log::trace(format_args!(
        "capabilities presentation={}",
        u8::from(st.presentation.is_some())
    ));
    crate::info!(
        "display: connected to the compositor (dmabuf v{:?}, viewporter {}, fractional-scale {}, activation {}, text-input {})",
        st.dmabuf.version(),
        st.viewporter.is_some(),
        st.fractional.is_some(),
        st.activation.is_some(),
        st.seat.text.available()
    );
    // Complete default feedback before accepting any BUFFER_ADD commands.
    let _feedback = st.dmabuf.get_default_feedback(&qh).ok();
    queue.roundtrip(&mut st).context("dma-buf formats")?;
    WaylandSource::new(conn, queue)
        .insert(ev.handle())
        .map_err(|e| anyhow::anyhow!("wayland source: {e}"))?;
    ev.handle()
        .insert_source(commands, |e, _, st: &mut State| match e {
            ChannelEvent::Msg(c) => st.command(c),
            ChannelEvent::Closed => st.quit = true,
        })
        .map_err(|e| anyhow::anyhow!("command source: {e}"))?;
    ev.handle()
        .insert_source(appearance::subscribe(), |event, _, st: &mut State| {
            if let ChannelEvent::Msg(appearance) = event {
                st.appearance = appearance;
                for w in st.windows.values_mut() {
                    if let Some(frame) = w.frame.as_mut() {
                        frame.set_config(st.appearance.config(w.color));
                        if !frame.is_hidden() {
                            frame.draw();
                            w.window.commit();
                        }
                    }
                }
            }
        })
        .map_err(|e| anyhow::anyhow!("appearance source: {e}"))?;
    while !st.quit {
        let animating = st
            .windows
            .values()
            .any(|w| w.frame.as_ref().is_some_and(|f| f.is_animating()));
        let interval = if animating {
            Duration::from_millis(16)
        } else {
            visibility::PROBE_INTERVAL
        };
        ev.dispatch((!st.windows.is_empty()).then_some(interval), &mut st)
            .context("dispatch")?;
        // SCTK calls output_destroyed before removing OutputState's entry.
        // Reconcile after dispatch, when all output mutations are complete.
        if std::mem::take(&mut st.outputs_changed) {
            st.report_outputs();
        }
        st.probe_visibility();
        for w in st.windows.values_mut() {
            if let Some(frame) = w.frame.as_mut().filter(|f| f.is_animating()) {
                frame.animate();
                w.window.commit();
            }
        }
    }
    for id in st.windows.keys().copied().collect::<Vec<_>>() {
        st.destroy(id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quarter_turns_swap_the_shown_sides() {
        assert_eq!(turned((1080, 2400), 0), (1080, 2400));
        assert_eq!(turned((1080, 2400), 1), (2400, 1080));
        assert_eq!(turned((1080, 2400), 2), (1080, 2400));
        assert_eq!(turned((1080, 2400), 3), (2400, 1080));
        assert_eq!(buffer_transform(1), wl_output::Transform::_270);
        assert_eq!(buffer_transform(4), wl_output::Transform::Normal);
    }

    #[test]
    fn display_pixels_per_logical_unit() {
        // OpenHarmony's 720x1280 panel in a window of 360x640 logical units.
        assert_eq!(
            pixels_per_unit((360, 640), (720, 1280), 0),
            Some((2.0, 2.0))
        );
        // A rotated frame keeps its buffer's orientation.
        assert_eq!(
            pixels_per_unit((640, 360), (720, 1280), 1),
            Some((2.0, 2.0))
        );
        assert_eq!(
            pixels_per_unit((800, 400), (1200, 600), 0),
            Some((1.5, 1.5))
        );
        // Before the first frame, or for an empty window, there is none.
        assert_eq!(pixels_per_unit((360, 640), (0, 0), 0), None);
        assert_eq!(pixels_per_unit((0, 640), (720, 1280), 0), None);
    }
}
