//! Opt-in GTK experiment, deliberately limited to one window per composer.
//! All GTK objects live on one persistent GLib thread, including reconnects.
mod interop;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::os::fd::{AsRawFd, OwnedFd};
use std::rc::Rc;
use std::sync::{Arc, OnceLock, mpsc};
use std::thread::JoinHandle;
use std::time::Duration;

use adw::prelude::*;
use anyhow::{Context, Result};
use calloop::channel::{Channel, Event};
use gtk::{gdk, glib};

use super::proto::{BufferSpec, event, ty};
use super::wl::Cmd;
use crate::input::Hub;
use crate::input::proto::WindowRequest;
use crate::ipc::server::Events;

pub(super) fn start(rx: Channel<Cmd>, events: Events, hub: Arc<Hub>) -> Result<JoinHandle<()>> {
    static UI: OnceLock<Result<glib::MainContext, String>> = OnceLock::new();
    let context = UI
        .get_or_init(|| {
            let (tx, rx) = mpsc::sync_channel(1);
            std::thread::Builder::new()
                .name("gtk-spike".into())
                .spawn(move || {
                    let context = glib::MainContext::default();
                    let _owner = context.acquire().unwrap();
                    if let Err(e) = adw::init() {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                    // Keep one application registered for the UI thread's lifetime.
                    // Sequential Android windows must not re-export its D-Bus path.
                    let application = adw::Application::builder()
                        .application_id("dev.linuxapk.GtkSpike")
                        .flags(gtk::gio::ApplicationFlags::NON_UNIQUE)
                        .build();
                    if let Err(e) = application.register(None::<&gtk::gio::Cancellable>) {
                        let _ = tx.send(Err(e.to_string()));
                        return;
                    }
                    crate::info!(
                        "GTK spike: GTK {}.{}.{}; libadwaita {}.{}.{}",
                        gtk::major_version(),
                        gtk::minor_version(),
                        gtk::micro_version(),
                        adw::major_version(),
                        adw::minor_version(),
                        adw::micro_version()
                    );
                    let _ = tx.send(Ok(context.clone()));
                    glib::MainLoop::new(Some(&context), false).run();
                })
                .map_err(|e| e.to_string())?;
            rx.recv().map_err(|e| e.to_string())?
        })
        .as_ref()
        .map_err(|e| anyhow::anyhow!("GTK spike init: {e}"))?;
    let (done, wait) = mpsc::channel();
    context.invoke(move || {
        if let Err(e) = install(rx, events.clone(), hub, done.clone()) {
            crate::warn!("GTK spike: {e:#}");
            events.shutdown();
            let _ = done.send(());
        }
    });
    Ok(std::thread::Builder::new()
        .name("gtk-lifetime".into())
        .spawn(move || {
            let _ = wait.recv();
        })?)
}

struct State {
    win: Option<Win>,
    buffers: HashMap<u32, (BufferSpec, OwnedFd)>,
    hub: Arc<Hub>,
    events: Events,
    quit: bool,
}

fn install(rx: Channel<Cmd>, events: Events, hub: Arc<Hub>, done: mpsc::Sender<()>) -> Result<()> {
    let mut loop_ = calloop::EventLoop::<State>::try_new()?;
    loop_
        .handle()
        .insert_source(rx, |cmd, _, s| match cmd {
            Event::Msg(cmd) => {
                if let Err(e) = s.command(cmd) {
                    crate::warn!("GTK spike: {e:#}");
                    s.quit = true;
                }
            }
            Event::Closed => s.quit = true,
        })
        .map_err(|e| anyhow::anyhow!("GTK command source: {}", e.error))?;
    let fd = loop_.as_raw_fd();
    let mut state = State {
        win: None,
        buffers: HashMap::new(),
        hub,
        events,
        quit: false,
    };
    watch_fd(fd, move || {
        if let Err(e) = loop_.dispatch(Duration::ZERO, &mut state) {
            crate::warn!("GTK dispatch: {e}");
            state.quit = true;
        }
        if state.quit {
            state.win.take();
            state.buffers.clear();
            state.events.shutdown();
            let _ = done.send(());
            glib::ControlFlow::Break
        } else {
            glib::ControlFlow::Continue
        }
    });
    Ok(())
}

struct Win {
    id: u32,
    window: adw::ApplicationWindow,
    picture: gtk::Picture,
    size: Rc<Cell<(u32, u32)>>,
    pending: Rc<Cell<Option<(u64, i64)>>>,
    interop: Rc<RefCell<Option<interop::Interop>>>,
    first: bool,
    tick: Option<gtk::TickCallbackId>,
    paint_signal: Option<(gdk::FrameClock, glib::SignalHandlerId)>,
    // ApplicationWindow holds the application; this also prevents its early release.
    _application: adw::Application,
}

impl State {
    fn command(&mut self, cmd: Cmd) -> Result<()> {
        match cmd {
            Cmd::Create(req, reply) => {
                if self.win.is_some() {
                    let _ = reply.send(-libc::EBUSY);
                } else {
                    let win = Win::new(req, self.hub.clone())?;
                    self.win = Some(win);
                    let _ = reply.send(0);
                }
            }
            Cmd::Destroy(id) => {
                if self.win.as_ref().is_some_and(|w| w.id == id) {
                    self.win.take();
                    self.buffers.clear();
                }
            }
            Cmd::BufferAdd(spec, _, fd) => {
                self.buffers.insert(spec.buffer, (spec, fd));
            }
            Cmd::BufferRemove(id) => {
                self.buffers.remove(&id);
            }
            Cmd::Frame { window, buffer, .. } => {
                let Some(win) = self.win.as_mut().filter(|w| w.id == window) else {
                    release(&self.events, buffer);
                    return Ok(());
                };
                let (spec, fd) = self.buffers.get(&buffer).context("missing GTK buffer")?;
                // Each texture owns a duplicated fd until GDK/GSK/compositor release it.
                // Keeping a texture per buffer would prevent RELEASE and stall HWC's pool.
                let frame_fd = fd.try_clone()?;
                let builder = gdk::DmabufTextureBuilder::new()
                    .set_display(&win.picture.display())
                    .set_width(spec.width)
                    .set_height(spec.height)
                    .set_fourcc(spec.fourcc)
                    .set_modifier(spec.modifier)
                    .set_n_planes(1)
                    .set_stride(0, spec.stride)
                    .set_offset(0, spec.offset)
                    .set_premultiplied(true);
                let builder = unsafe { builder.set_fd(0, frame_fd.as_raw_fd()) };
                let texture = build_texture(builder, frame_fd, self.events.clone(), buffer)?;
                win.size.set((spec.width, spec.height));
                if crate::bench::enabled() {
                    let color = center_pixel(fd, spec).map_or(-1, i64::from);
                    win.pending.set(Some((crate::bench::now(), color)));
                }
                win.picture.set_paintable(Some(&texture));
                if !win.first {
                    win.first = true;
                    self.hub.first_frame(window);
                }
            }
            Cmd::PointerCapture(enabled) => {
                if let Some(w) = &self.win
                    && let Some(p) = w.interop.borrow_mut().as_mut()
                {
                    p.request(enabled);
                }
            }
            Cmd::Activate(_, _) => {
                if let Some(w) = &self.win {
                    w.window.present();
                }
            }
            Cmd::Minimize(_) => {
                if let Some(w) = &self.win {
                    w.window.minimize();
                }
            }
            Cmd::ExportToken(_, reply) => {
                let _ = reply.send(String::new());
            }
            Cmd::Cursor { image, .. } => {
                if let Some(w) = &self.win {
                    // GDK accepts Android's premultiplied BGRA bitmap directly. Hotspot
                    // inference remains a migration task; this spike uses top-left.
                    let cursor = if image.pixels.is_empty() {
                        gdk::Cursor::from_name("none", None)
                    } else {
                        let bytes = glib::Bytes::from_owned(image.pixels);
                        let texture = gdk::MemoryTexture::new(
                            image.width as i32,
                            image.height as i32,
                            gdk::MemoryFormat::B8g8r8a8Premultiplied,
                            &bytes,
                            image.width as usize * 4,
                        );
                        Some(gdk::Cursor::from_texture(&texture, 0, 0, None))
                    };
                    w.picture.set_cursor(cursor.as_ref());
                }
            }
            Cmd::Quit => self.quit = true,
            Cmd::TextInput(_) | Cmd::WindowPolicy(_) | Cmd::WindowAppearance { .. } => {}
        }
        Ok(())
    }
}

fn release(events: &Events, buffer: u32) {
    let _ = events.try_send(ty::RELEASE, &event::release(buffer), &[]);
}

fn center_pixel(fd: &OwnedFd, spec: &BufferSpec) -> Option<u32> {
    let offset = u64::from(spec.offset)
        + u64::from(spec.height / 2) * u64::from(spec.stride)
        + u64::from(spec.width / 2) * 4;
    let mut bytes = [0u8; 4];
    // dma-buf exporters need not implement pread (udmabuf does not).
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    let base = offset / page * page;
    let within = (offset - base) as usize;
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            within + 4,
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
        std::ptr::copy_nonoverlapping(ptr.cast::<u8>().add(within), bytes.as_mut_ptr(), 4);
        libc::munmap(ptr, within + 4);
    }
    Some(u32::from_le_bytes(bytes) & 0xffffff)
}

impl Win {
    fn new(req: WindowRequest, hub: Arc<Hub>) -> Result<Self> {
        let id = req.window;
        let application = gtk::gio::Application::default()
            .context("missing GTK application")?
            .downcast::<adw::Application>()
            .map_err(|_| anyhow::anyhow!("unexpected GTK application"))?;
        let picture = gtk::Picture::builder()
            .can_shrink(true)
            .hexpand(true)
            .vexpand(true)
            .focusable(true)
            .build();
        picture.set_content_fit(gtk::ContentFit::Fill);
        let offload = gtk::GraphicsOffload::new(Some(&picture));
        offload.set_enabled(gtk::GraphicsOffloadEnabled::Enabled);
        let toolbar = adw::ToolbarView::new();
        toolbar.set_content(Some(&offload));
        let header = adw::HeaderBar::new();
        header.set_title_widget(Some(&adw::WindowTitle::new(&req.title, "GTK spike")));
        let back = gtk::Button::from_icon_name("go-previous-symbolic");
        back.set_tooltip_text(Some("Back"));
        header.pack_start(&back);
        let fullscreen = gtk::Button::from_icon_name("view-fullscreen-symbolic");
        fullscreen.set_tooltip_text(Some("Fullscreen (F11)"));
        header.pack_end(&fullscreen);
        toolbar.add_top_bar(&header);
        let header_height = header
            .measure(gtk::Orientation::Vertical, req.width as i32)
            .1;
        let window = adw::ApplicationWindow::builder()
            .application(&application)
            .title(&req.title)
            .default_width(req.width as i32)
            .default_height(req.height as i32 + header_height)
            .content(&toolbar)
            .build();
        window.set_size_request(
            req.min_width.max(240) as i32,
            req.min_height.max(160) as i32,
        );
        let input = hub.clone();
        back.connect_clicked(move |_| input.input(&format!("back {id}")));
        let weak = window.downgrade();
        fullscreen.connect_clicked(move |_| {
            if let Some(w) = weak.upgrade() {
                toggle(&w);
            }
        });
        let bars = toolbar.clone();
        let button = fullscreen.clone();
        window.connect_fullscreened_notify(move |w| {
            bars.set_extend_content_to_top_edge(w.is_fullscreen());
            bars.set_reveal_top_bars(!w.is_fullscreen());
            button.set_icon_name(if w.is_fullscreen() {
                "view-restore-symbolic"
            } else {
                "view-fullscreen-symbolic"
            });
        });
        let input = hub.clone();
        window.connect_close_request(move |_| {
            input.input(&format!("close {id}"));
            glib::Propagation::Stop
        });
        let input = hub.clone();
        window.connect_is_active_notify(move |w| {
            input.input(&format!(
                "keyboard_{} {id}",
                if w.is_active() { "enter" } else { "leave" }
            ))
        });
        window.present();
        picture.grab_focus();
        let interop: Rc<RefCell<Option<interop::Interop>>> = Rc::new(RefCell::new(None));
        let size = Rc::new(Cell::new((req.width, req.height)));
        let pending = Rc::new(Cell::new(None));
        input_controllers(
            &window,
            &picture,
            &toolbar,
            &header,
            size.clone(),
            hub.clone(),
            interop.clone(),
            id,
        );
        let old_size = Rc::new(Cell::new((0, 0, 0)));
        let input = hub.clone();
        let app_id = req.app_id.clone();
        let weak = window.downgrade();
        let protocol = interop.clone();
        let paint = pending.clone();
        let tick = picture.add_tick_callback(move |p, _| {
            let Some(w) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let scale = w.surface().map_or(1.0, |s| s.scale());
            let dims = (p.width(), p.height(), (scale * 120.0).round() as i32);
            if dims.0 > 0 && dims.1 > 0 && old_size.replace(dims) != dims {
                input.input(&format!("window {} {} {scale:.3} {id}", dims.0, dims.1));
            }
            let mut protocol = protocol.borrow_mut();
            if protocol.is_none()
                && let Ok(p) = interop::Interop::new(&w, input.clone(), id, &app_id)
            {
                *protocol = Some(p)
            }
            if let Some(p) = protocol.as_mut() {
                p.keyboard_focus(w.is_active());
                p.dispatch();
            }
            glib::ControlFlow::Continue
        });
        let protocol = interop.clone();
        let paint_signal = window.frame_clock().map(|clock| {
            let signal = clock.connect_before_paint(move |_| {
                if let Some((stamp, color)) = paint.take() {
                    // This is a GTK before-paint timestamp, not an owned wl_surface.commit.
                    let now = crate::bench::now();
                    crate::bench::trace(format_args!("commit ns={now} window={id} color={color} queued_ns={stamp} backend=gtk-before-paint"));
                    if let Some(p) = protocol.borrow().as_ref() { p.feedback(now); }
                }
            });
            (clock, signal)
        });
        Ok(Self {
            id,
            window,
            picture,
            size,
            pending,
            interop,
            first: false,
            tick: Some(tick),
            paint_signal,
            _application: application,
        })
    }
}

impl Drop for Win {
    fn drop(&mut self) {
        if let Some(tick) = self.tick.take() {
            tick.remove();
        }
        if let Some((clock, signal)) = self.paint_signal.take() {
            clock.disconnect(signal);
        }
        if let Some(p) = self.interop.borrow_mut().as_mut() {
            p.cancel();
        }
        self.window.destroy();
    }
}
fn toggle(w: &adw::ApplicationWindow) {
    if w.is_fullscreen() {
        w.unfullscreen();
    } else {
        w.fullscreen();
    }
}

#[allow(clippy::too_many_arguments)]
fn input_controllers(
    window: &adw::ApplicationWindow,
    picture: &gtk::Picture,
    toolbar: &adw::ToolbarView,
    header: &adw::HeaderBar,
    size: Rc<Cell<(u32, u32)>>,
    hub: Arc<Hub>,
    interop: Rc<RefCell<Option<interop::Interop>>>,
    id: u32,
) {
    // GTK coordinates are logical and include the header at the toplevel. The
    // controller on the picture gives us a content-relative transform instead.
    let controller = gtk::EventControllerLegacy::new();
    let weak = picture.downgrade();
    let touches = RefCell::new(HashMap::new());
    let next_touch = Cell::new(0i32);
    let protocol = interop.clone();
    let input = hub.clone();
    let frame_size = size.clone();
    controller.connect_event(move |_, event| {
        let Some(p) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let Some((x, y)) = event.position() else {
            return glib::Propagation::Proceed;
        };
        let (x, y) = event
            .surface()
            .and_then(|_| p.root())
            .and_then(|r| r.dynamic_cast::<gtk::Widget>().ok())
            .and_then(|r| r.compute_point(&p, &gtk::graphene::Point::new(x as f32, y as f32)))
            .map_or((x, y), |point| (point.x() as f64, point.y() as f64));
        let (bw, bh) = frame_size.get();
        let x = x * bw as f64 / p.width().max(1) as f64;
        let y = y * bh as f64 / p.height().max(1) as f64;
        let time = event.time();
        match event.event_type() {
            gdk::EventType::MotionNotify
                if !protocol.borrow().as_ref().is_some_and(|p| p.active()) =>
            {
                input.input(&format!("pointer_motion {time} {x:.2} {y:.2}"));
                input.input("pointer_frame");
            }
            gdk::EventType::ButtonPress | gdk::EventType::ButtonRelease => {
                p.grab_focus();
                if let Some(e) = event.downcast_ref::<gdk::ButtonEvent>() {
                    let button = match e.button() {
                        1 => 272,
                        2 => 274,
                        3 => 273,
                        8 => 275,
                        9 => 276,
                        _ => return glib::Propagation::Proceed,
                    };
                    input.input(&format!(
                        "pointer_button {time} {button} {}",
                        (event.event_type() == gdk::EventType::ButtonPress) as u8
                    ));
                    input.input("pointer_frame");
                }
            }
            gdk::EventType::Scroll => {
                if let Some(e) = event.downcast_ref::<gdk::ScrollEvent>() {
                    let (dx, dy) = e.deltas();
                    input.input(&format!("pointer_axis {time} 0 {:.3}", dy * 15.0));
                    input.input(&format!("pointer_axis {time} 1 {:.3}", dx * 15.0));
                    input.input("pointer_frame");
                }
            }
            gdk::EventType::TouchBegin => {
                let n = next_touch.get();
                next_touch.set(n + 1);
                touches.borrow_mut().insert(event.event_sequence(), n);
                input.input(&format!("touch_down {time} {n} {x:.2} {y:.2} {id}"));
                input.input("touch_frame");
            }
            gdk::EventType::TouchUpdate => {
                if let Some(n) = touches.borrow().get(&event.event_sequence()) {
                    input.input(&format!("touch_motion {time} {n} {x:.2} {y:.2}"));
                    input.input("touch_frame");
                }
            }
            gdk::EventType::TouchEnd => {
                if let Some(n) = touches.borrow_mut().remove(&event.event_sequence()) {
                    input.input(&format!("touch_up {time} {n}"));
                    input.input("touch_frame");
                }
            }
            gdk::EventType::TouchCancel => {
                touches.borrow_mut().clear();
                input.input("touch_cancel");
            }
            _ => return glib::Propagation::Proceed,
        }
        glib::Propagation::Stop
    });
    picture.add_controller(controller);
    let motion = gtk::EventControllerMotion::new();
    let weak = picture.downgrade();
    let input = hub.clone();
    let dimensions = size.clone();
    let protocol = interop.clone();
    motion.connect_enter(move |_, x, y| {
        if let Some(p) = weak.upgrade() {
            let (bw, bh) = dimensions.get();
            let x = x * bw as f64 / p.width().max(1) as f64;
            let y = y * bh as f64 / p.height().max(1) as f64;
            input.input(&format!("pointer_enter {x:.2} {y:.2} {id}"));
            input.input("pointer_frame");
            if let Some(p) = protocol.borrow_mut().as_mut() {
                p.focus(true);
            }
        }
    });
    let input = hub.clone();
    let protocol = interop.clone();
    motion.connect_leave(move |_| {
        input.input(&format!("pointer_leave {id}"));
        input.input("pointer_frame");
        if let Some(p) = protocol.borrow_mut().as_mut() {
            p.focus(false);
        }
    });
    picture.add_controller(motion);
    let key = gtk::EventControllerKey::new();
    key.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = window.downgrade();
    let protocol = interop.clone();
    let input = hub.clone();
    key.connect_key_pressed(move |c, sym, code, mods| {
        if sym == gdk::Key::F11 {
            if let Some(w) = weak.upgrade() {
                toggle(&w);
            }
            return glib::Propagation::Stop;
        }
        if sym == gdk::Key::Escape
            && let Some(p) = protocol.borrow_mut().as_mut()
        {
            p.cancel();
        }
        key_line(&input, c, code, true, mods);
        glib::Propagation::Stop
    });
    key.connect_key_released(move |c, sym, code, mods| {
        if sym != gdk::Key::F11 {
            key_line(&hub, c, code, false, mods);
        }
    });
    window.add_controller(key);
    let motion = gtk::EventControllerMotion::new();
    motion.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = window.downgrade();
    let bars = toolbar.clone();
    let height = header.clone();
    motion.connect_motion(move |controller, _, y| {
        // Revealing the bar changes layout and synthesizes stationary motion.
        // That must not immediately hide a bar just revealed by touch.
        if controller.current_event_time() == 0 {
            return;
        }
        if weak.upgrade().is_some_and(|w| w.is_fullscreen()) {
            bars.set_reveal_top_bars(
                y <= if bars.reveals_top_bars() {
                    height.height().max(4) as f64
                } else {
                    4.0
                },
            );
        }
    });
    window.add_controller(motion);
    let gesture = gtk::GestureClick::new();
    gesture.set_touch_only(true);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let bars = toolbar.clone();
    let height = header.clone();
    let weak = window.downgrade();
    gesture.connect_pressed(move |g, _, _, y| {
        if y <= 4.0 && weak.upgrade().is_some_and(|w| w.is_fullscreen()) {
            bars.set_reveal_top_bars(true);
            g.set_state(gtk::EventSequenceState::Claimed);
        } else if y > height.height() as f64 && weak.upgrade().is_some_and(|w| w.is_fullscreen()) {
            bars.set_reveal_top_bars(false);
        }
    });
    window.add_controller(gesture);
    let protocol = interop.clone();
    window.connect_is_active_notify(move |w| {
        if !w.is_active()
            && let Some(p) = protocol.borrow_mut().as_mut()
        {
            p.cancel();
        }
    });
}

fn key_line(
    hub: &Hub,
    controller: &gtk::EventControllerKey,
    code: u32,
    pressed: bool,
    mods: gdk::ModifierType,
) {
    // GDK keycodes on Wayland are XKB codes, evdev + 8. This spike handles
    // physical keys and GDK modifiers; compose/IME and repeat parity are unported.
    let code = code.saturating_sub(8);
    let time = controller.current_event_time();
    let mut mask = 0;
    for (flag, bit) in [
        (gdk::ModifierType::SHIFT_MASK, 0),
        (gdk::ModifierType::LOCK_MASK, 1),
        (gdk::ModifierType::CONTROL_MASK, 2),
        (gdk::ModifierType::ALT_MASK, 3),
        (gdk::ModifierType::SUPER_MASK, 6),
    ] {
        if mods.contains(flag) {
            mask |= 1 << bit;
        }
    }
    hub.input(&format!("modifiers {mask} 0 0 0"));
    let line = format!("key {time} {code} {}", pressed as u8);
    crate::bench::input(&line);
    hub.input(&line);
}

// glib-rs 0.22 no longer wraps glib-unix. The source and its non-Send closure
// are created, dispatched and destroyed on the single GTK thread.
fn watch_fd(fd: i32, callback: impl FnMut() -> glib::ControlFlow + 'static) {
    type Callback = Box<dyn FnMut() -> glib::ControlFlow>;
    unsafe extern "C" fn dispatch(_: i32, _: u32, data: *mut std::ffi::c_void) -> i32 {
        let callback = unsafe { &mut *data.cast::<Callback>() };
        (callback() == glib::ControlFlow::Continue) as i32
    }
    unsafe extern "C" fn destroy(data: *mut std::ffi::c_void) {
        drop(unsafe { Box::from_raw(data.cast::<Callback>()) });
    }
    unsafe extern "C" {
        fn g_unix_fd_add_full(
            priority: i32,
            fd: i32,
            condition: u32,
            callback: unsafe extern "C" fn(i32, u32, *mut std::ffi::c_void) -> i32,
            data: *mut std::ffi::c_void,
            notify: unsafe extern "C" fn(*mut std::ffi::c_void),
        ) -> u32;
    }
    let callback: Box<Callback> = Box::new(Box::new(callback));
    unsafe {
        g_unix_fd_add_full(0, fd, 1, dispatch, Box::into_raw(callback).cast(), destroy);
    }
}

// The gtk-rs release-closure helper loses its allocation when GDK rejects an
// import. Keep ownership explicit so the unsupported-format path releases too.
fn build_texture(
    builder: gdk::DmabufTextureBuilder,
    fd: OwnedFd,
    events: Events,
    buffer: u32,
) -> Result<gdk::Texture> {
    use glib::translate::{ToGlibPtr, from_glib_full};
    struct Lease {
        _fd: OwnedFd,
        events: Events,
        buffer: u32,
    }
    impl Drop for Lease {
        fn drop(&mut self) {
            release(&self.events, self.buffer);
        }
    }
    unsafe extern "C" fn destroy(data: *mut std::ffi::c_void) {
        drop(unsafe { Box::from_raw(data.cast::<Lease>()) });
    }
    let data = Box::into_raw(Box::new(Lease {
        _fd: fd,
        events,
        buffer,
    }));
    let mut error = std::ptr::null_mut();
    let texture = unsafe {
        gdk::ffi::gdk_dmabuf_texture_builder_build(
            builder.to_glib_none().0,
            Some(destroy),
            data.cast(),
            &mut error,
        )
    };
    if texture.is_null() {
        drop(unsafe { Box::from_raw(data) });
        if !error.is_null() {
            return Err(unsafe { from_glib_full::<_, glib::Error>(error) }.into());
        }
        anyhow::bail!("GDK rejected dma-buf texture");
    }
    Ok(unsafe { from_glib_full(texture) })
}
