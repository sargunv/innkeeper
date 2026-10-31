//! The launch splash (docs/display.md, "Launch feedback"): while a cold
//! session boots, `innkeeper run` shows a toplevel of its own at once, in the
//! app's splash colour and window size with its icon, under the app's own
//! app id. It takes focus with the desktop launch's `XDG_ACTIVATION_TOKEN`
//! (which completes the startup notification), then asks for a fresh token
//! for the app's real window; [`Splash::close`] drops it once that window
//! shows its first frame.

use std::fs::File;
use std::path::Path;
use std::sync::mpsc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use anyhow::{Context, Result};
use sctk::activation::{ActivationHandler, ActivationState, RequestData};
use sctk::compositor::{CompositorHandler, CompositorState};
use sctk::output::{OutputHandler, OutputState};
use sctk::reexports::calloop::EventLoop;
use sctk::reexports::calloop::channel::{self, Event as ChannelEvent};
use sctk::reexports::calloop_wayland_source::WaylandSource;
use sctk::reexports::client::globals::registry_queue_init;
use sctk::reexports::client::protocol::{wl_output, wl_shm, wl_surface};
use sctk::reexports::client::{Connection, QueueHandle};
use sctk::registry::{ProvidesRegistryState, RegistryState};
use sctk::shell::WaylandSurface;
use sctk::shell::xdg::XdgShell;
use sctk::shell::xdg::window::{Window, WindowConfigure, WindowDecorations, WindowHandler};
use sctk::shm::slot::SlotPool;
use sctk::shm::{Shm, ShmHandler};
use sctk::{delegate_dispatch2, delegate_registry, registry_handlers};

/// What the splash shows.
pub struct Look {
    pub app_id: String,
    pub title: String,
    /// `0xRRGGBB`.
    pub color: u32,
    /// Logical pixels: the app window's size in dp.
    pub width: u32,
    pub height: u32,
    pub icon: Option<Rgba>,
}

/// A decoded icon, not premultiplied.
pub struct Rgba {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Rgba {
    pub fn load_png(path: &Path) -> Result<Rgba> {
        let mut d = png::Decoder::new(std::io::BufReader::new(File::open(path)?));
        d.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
        let mut r = d.read_info()?;
        let mut buf = vec![0; r.output_buffer_size().context("icon too large")?];
        let info = r.next_frame(&mut buf)?;
        let px = info.width as usize * info.height as usize;
        let pixels = match info.color_type {
            png::ColorType::Rgba => buf[..px * 4].to_vec(),
            png::ColorType::Rgb => buf[..px * 3]
                .as_chunks::<3>()
                .0
                .iter()
                .flat_map(|c| [c[0], c[1], c[2], 255])
                .collect(),
            png::ColorType::GrayscaleAlpha => buf[..px * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .flat_map(|c| [c[0], c[0], c[0], c[1]])
                .collect(),
            png::ColorType::Grayscale => buf[..px].iter().flat_map(|&g| [g, g, g, 255]).collect(),
            png::ColorType::Indexed => anyhow::bail!("indexed PNG after EXPAND"),
        };
        Ok(Rgba {
            width: info.width,
            height: info.height,
            pixels,
        })
    }
}

/// A shown splash.
pub struct Splash {
    close: channel::Sender<()>,
    token: mpsc::Receiver<String>,
    thread: JoinHandle<()>,
}

impl Splash {
    /// Shows `look` on the desktop's compositor and activates it with
    /// `token`. None if there is no compositor to show it on.
    pub fn show(look: Look, token: Option<String>) -> Option<Splash> {
        let conn = Connection::connect_to_env().ok()?;
        let (close, closed) = channel::channel();
        let (tx, rx) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("splash".into())
            .spawn(move || {
                if let Err(e) = run(conn, look, token, closed, tx) {
                    eprintln!("innkeeper: launch splash: {e:#}");
                }
            })
            .ok()?;
        Some(Splash {
            close,
            token: rx,
            thread,
        })
    }

    /// An activation token for the app's window, from the splash once it
    /// has focus: what lets the compositor move focus on to that window.
    pub fn token(&self, wait: Duration) -> Option<String> {
        self.token.recv_timeout(wait).ok()
    }

    pub fn close(self) {
        let _ = self.close.send(());
        let _ = self.thread.join();
    }
}

struct State {
    registry: RegistryState,
    output: OutputState,
    shm: Shm,
    activation: Option<ActivationState>,
    window: Window,
    pool: Option<SlotPool>,
    look: Look,
    launch_token: Option<String>,
    token_out: mpsc::Sender<String>,
    asked_token: bool,
    scale: u32,
    size: (u32, u32),
    configured: bool,
    quit: bool,
}

fn run(
    conn: Connection,
    look: Look,
    token: Option<String>,
    closed: channel::Channel<()>,
    token_out: mpsc::Sender<String>,
) -> Result<()> {
    let (globals, queue) = registry_queue_init::<State>(&conn).context("registry")?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor")?;
    let xdg = XdgShell::bind(&globals, &qh).context("xdg_wm_base")?;
    let surface = compositor.create_surface(&qh);
    let window = xdg.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title(look.title.clone());
    window.set_app_id(look.app_id.clone());
    window.commit();
    let mut st = State {
        registry: RegistryState::new(&globals),
        output: OutputState::new(&globals, &qh),
        shm: Shm::bind(&globals, &qh).context("wl_shm")?,
        activation: ActivationState::bind(&globals, &qh).ok(),
        window,
        pool: None,
        size: (look.width, look.height),
        look,
        launch_token: token,
        token_out,
        asked_token: false,
        scale: 1,
        configured: false,
        quit: false,
    };
    let mut ev: EventLoop<State> = EventLoop::try_new().context("event loop")?;
    WaylandSource::new(conn, queue)
        .insert(ev.handle())
        .map_err(|e| anyhow::anyhow!("wayland source: {e}"))?;
    ev.handle()
        .insert_source(closed, |_: ChannelEvent<()>, _, st: &mut State| {
            st.quit = true
        })
        .map_err(|e| anyhow::anyhow!("close source: {e}"))?;
    while !st.quit {
        ev.dispatch(None, &mut st).context("dispatch")?;
    }
    Ok(())
}

impl State {
    fn draw(&mut self) -> Result<()> {
        let s = self.scale;
        let (w, h) = (self.size.0 * s, self.size.1 * s);
        let stride = w as i32 * 4;
        let pool = match &mut self.pool {
            Some(p) => p,
            None => self
                .pool
                .insert(SlotPool::new((stride * h as i32) as usize, &self.shm)?),
        };
        let (buffer, canvas) =
            pool.create_buffer(w as i32, h as i32, stride, wl_shm::Format::Xrgb8888)?;
        paint(canvas, w, h, &self.look, s);
        let surface = self.window.wl_surface();
        let _ = self.window.set_buffer_scale(s);
        buffer.attach_to(surface)?;
        surface.damage_buffer(0, 0, w as i32, h as i32);
        self.window.commit();
        Ok(())
    }
}

/// Fills `canvas` (XRGB8888, `w`x`h`) with the splash colour and the icon,
/// centred, a third of the shorter side but at most 192 logical pixels.
fn paint(canvas: &mut [u8], w: u32, h: u32, look: &Look, scale: u32) {
    let bg = look.color & 0xff_ffff;
    let px = bg.to_le_bytes();
    for c in canvas.as_chunks_mut::<4>().0 {
        c.copy_from_slice(&px);
    }
    let Some(icon) = &look.icon else { return };
    if icon.width == 0 || icon.height == 0 {
        return;
    }
    let side = (w.min(h) / 3).min(192 * scale).max(1);
    let (x0, y0) = ((w - side.min(w)) / 2, (h - side.min(h)) / 2);
    let [bb, bg_, br, _] = px;
    for y in 0..side.min(h) {
        // Nearest-neighbour sampling: a splash is up for a second or two.
        let sy = (y as u64 * icon.height as u64 / side as u64) as usize;
        for x in 0..side.min(w) {
            let sx = (x as u64 * icon.width as u64 / side as u64) as usize;
            let i = (sy * icon.width as usize + sx) * 4;
            let [r, g, b, a] = [
                icon.pixels[i],
                icon.pixels[i + 1],
                icon.pixels[i + 2],
                icon.pixels[i + 3],
            ];
            let mix =
                |f: u8, b: u8| ((f as u32 * a as u32 + b as u32 * (255 - a as u32)) / 255) as u8;
            let o = (((y0 + y) * w + x0 + x) * 4) as usize;
            canvas[o] = mix(b, bb);
            canvas[o + 1] = mix(g, bg_);
            canvas[o + 2] = mix(r, br);
        }
    }
}

impl WindowHandler for State {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        // Hide it; the app's window still comes.
        self.quit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &Window,
        c: WindowConfigure,
        _: u32,
    ) {
        if let (Some(w), Some(h)) = c.new_size {
            self.size = (w.get(), h.get());
        }
        if let Err(e) = self.draw() {
            eprintln!("innkeeper: launch splash: {e:#}");
            self.quit = true;
            return;
        }
        if !self.configured {
            self.configured = true;
            if let (Some(a), Some(t)) = (&self.activation, self.launch_token.take()) {
                a.activate::<State>(self.window.wl_surface(), t);
            }
        }
        // Focused now, so the compositor grants the app's window a token.
        if c.is_activated()
            && !self.asked_token
            && let Some(a) = &self.activation
        {
            self.asked_token = true;
            a.request_token(
                qh,
                RequestData {
                    app_id: Some(self.look.app_id.clone()),
                    seat_and_serial: None,
                    surface: Some(self.window.wl_surface().clone()),
                    udata: (),
                },
            );
        }
    }
}

impl ActivationHandler for State {
    type RequestUdata = ();

    fn new_token(&mut self, token: String, _: &RequestData<()>) {
        let _ = self.token_out.send(token);
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        let s = factor.clamp(1, 4) as u32;
        if s != self.scale {
            self.scale = s;
            if self.configured {
                let _ = self.draw();
            }
        }
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
        &mut self.output
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

impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState];
}

delegate_registry!(State);
delegate_dispatch2!(State);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paints_colour_and_icon() {
        let look = Look {
            app_id: "dev.sargunv.innkeeper.x".into(),
            title: "X".into(),
            color: 0x102030,
            width: 30,
            height: 30,
            // 1x1, opaque red.
            icon: Some(Rgba {
                width: 1,
                height: 1,
                pixels: vec![255, 0, 0, 255],
            }),
        };
        let mut c = vec![0; 30 * 30 * 4];
        paint(&mut c, 30, 30, &look, 1);
        // XRGB8888 little-endian: B, G, R, X.
        assert_eq!(&c[..3], &[0x30, 0x20, 0x10]);
        let mid = ((15 * 30 + 15) * 4) as usize;
        assert_eq!(&c[mid..mid + 3], &[0, 0, 255]);
    }
}
