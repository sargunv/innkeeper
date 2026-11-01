//! Clipboard transfers shared by ext-data-control and the display service's
//! focus-scoped wl_data_device (GNOME). Test sessions never create this hub.
//!
//! - Host to Android: each new host selection is read up to a size cap, in
//!   the first type it offers among a file transfer key, a URI list, text
//!   and a PNG image. It is sent to every subscribed system-app connection
//!   as a `CLIP` event with a sealed memfd.
//! - Android to host: `CLIP_SET` becomes the bridge's data source. It also
//!   offers a private marker type, so the selection event that follows is
//!   recognised as the bridge's own and not echoed back.
//!
//! Sensitive clips (Android's `EXTRA_IS_SENSITIVE`) carry KDE's
//! `x-kde-passwordManagerHint: secret`, which keeps them out of history.
//! Host clips with that hint arrive flagged sensitive.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rustix::event::{PollFd, PollFlags};
use wayland_client::backend::ObjectId;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, event_created_child};
use wayland_protocols::ext::data_control::v1::client::{
    ext_data_control_device_v1::{self, ExtDataControlDeviceV1},
    ext_data_control_manager_v1::ExtDataControlManagerV1,
    ext_data_control_offer_v1::{self, ExtDataControlOfferV1},
    ext_data_control_source_v1::{self, ExtDataControlSourceV1},
};

use super::proto::{ClipKind, clip_event, ev, limits};
use crate::ipc::Subscribers;
use crate::ipc::blob::{self, DocWriter, Image};
use crate::ipc::server::Events;

pub mod files;

/// Marks our own selection, so it isn't echoed back to Android.
pub(crate) const MARKER: &str = "application/x-innkeeper-bridge";
pub(crate) const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";
const TEXT_TYPES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];
/// The longest a transfer may take in either direction.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(5);
/// The largest host image accepted, per side.
const MAX_IMAGE_SIDE: u32 = 2048;

/// Clipboard content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Content {
    Text(String),
    Image(Image),
    Files(files::ClipFiles),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    pub content: Content,
    pub sensitive: bool,
}

impl Clip {
    /// The `CLIP` event body and the memfd that goes with it.
    fn event(&self) -> Result<(Vec<u8>, OwnedFd)> {
        let (kind, bytes) = match &self.content {
            Content::Text(t) => (ClipKind::Text, t.as_bytes().to_vec()),
            Content::Files(f) => (ClipKind::File, f.encode()),
            Content::Image(i) => {
                let mut w = crate::ipc::wire::Writer::default();
                w.image(Some(i));
                (ClipKind::Image, w.into_vec())
            }
        };
        let fd = blob::create("innkeeper-clip", &bytes)?;
        Ok((clip_event(kind, self.sensitive, bytes.len() as u32), fd))
    }
}

enum Cmd {
    Set(Arc<Offered>),
}

/// What the bridge's data source serves, as bytes per MIME type.
pub(crate) struct Offered {
    pub(crate) types: HashMap<String, Arc<Vec<u8>>>,
    _file: Option<files::CachedFiles>,
}

pub struct ClipboardHub {
    cmd: Option<(Mutex<mpsc::Sender<Cmd>>, OwnedFd)>,
    fallback: Mutex<Vec<calloop::channel::Sender<Arc<Offered>>>>,
    offered: Mutex<Option<Arc<Offered>>>,
    cache: std::path::PathBuf,
    subs: Subscribers,
    current: Mutex<Option<Clip>>,
    epoch: AtomicU64,
}

impl ClipboardHub {
    /// Uses data control when available, otherwise the display connection.
    pub fn start(cache: &std::path::Path) -> Result<Arc<ClipboardHub>> {
        let conn = Connection::connect_to_env().context("connecting to the host compositor")?;
        let (globals, queue) = registry_queue_init::<State>(&conn)?;
        let qh = queue.handle();
        let manager: Option<ExtDataControlManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let seat: wl_seat::WlSeat = globals.bind(&qh, 1..=1, ()).context("no wl_seat")?;

        let (wake_r, wake_w) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
        )?;
        let (tx, rx) = mpsc::channel();
        let hub = Arc::new(ClipboardHub {
            cmd: manager.as_ref().map(|_| (Mutex::new(tx), wake_w)),
            fallback: Mutex::new(Vec::new()),
            offered: Mutex::new(None),
            cache: files::fresh_cache(cache)?,
            subs: Subscribers::default(),
            current: Mutex::new(None),
            epoch: AtomicU64::new(0),
        });
        let Some(manager) = manager else {
            crate::info!("clipboard: wl_data_device (while a window has focus)");
            return Ok(hub);
        };
        let device = manager.get_data_device(&seat, &qh, ());
        let mut state = State {
            hub: Arc::downgrade(&hub),
            manager,
            device,
            offers: HashMap::new(),
            source: None,
        };
        std::thread::Builder::new()
            .name("clipboard".into())
            .spawn(move || {
                if let Err(e) = run(conn, queue, &mut state, rx, wake_r) {
                    crate::warn!("clipboard: {e:#}");
                }
            })?;
        crate::info!("clipboard: ext-data-control ready");
        Ok(hub)
    }

    /// Subscribes `e` and sends it the current host clipboard, if any.
    pub fn subscribe(&self, e: Events) -> u64 {
        if let Some(clip) = self.current.lock().unwrap().clone()
            && let Ok((body, fd)) = clip.event()
        {
            let _ = e.try_send(ev::CLIP, &body, &[fd.as_fd()]);
        }
        self.subs.add(e)
    }

    pub fn unsubscribe(&self, k: u64) {
        self.subs.remove(k);
    }

    /// Makes `clip` the host selection.
    pub fn set(&self, clip: Clip) -> Result<()> {
        let mut types: HashMap<String, Arc<Vec<u8>>> = HashMap::new();
        let mut file = None;
        match &clip.content {
            Content::Files(f) => {
                let cached = files::CachedFiles::create(&self.cache, f)?;
                if let Some(key) = cached.portal_key() {
                    types.insert(files::PORTAL_TYPE.into(), Arc::new(key));
                }
                types.insert("text/uri-list".into(), Arc::new(cached.uri_list()));
                types.insert(
                    "x-special/gnome-copied-files".into(),
                    Arc::new(cached.gnome_copied_files()),
                );
                file = Some(cached);
            }
            Content::Text(t) => {
                let b = Arc::new(t.as_bytes().to_vec());
                for ty in TEXT_TYPES {
                    types.insert(ty.into(), b.clone());
                }
            }
            Content::Image(i) => {
                types.insert("image/png".into(), Arc::new(encode_png(i)?));
            }
        }
        types.insert(MARKER.into(), Arc::new(Vec::new()));
        if clip.sensitive {
            types.insert(PASSWORD_HINT.into(), Arc::new(b"secret".to_vec()));
        }
        let mut current = self.current.lock().unwrap();
        self.epoch.fetch_add(1, Ordering::Relaxed);
        *current = Some(clip);
        let offered = Arc::new(Offered { types, _file: file });
        // Serialize attach and publication so a newly attached display cannot
        // miss the latest selection between subscribing and taking its snapshot.
        let mut latest = self.offered.lock().unwrap();
        *latest = Some(offered.clone());
        if let Some((cmd, wake)) = &self.cmd {
            cmd.lock()
                .unwrap()
                .send(Cmd::Set(offered.clone()))
                .context("clipboard thread gone")?;
            let _ = rustix::io::write(wake, &[1]);
        } else {
            self.fallback
                .lock()
                .unwrap()
                .retain(|tx| tx.send(offered.clone()).is_ok());
        }
        Ok(())
    }

    pub(crate) fn attach(&self, tx: calloop::channel::Sender<Arc<Offered>>) -> bool {
        if self.cmd.is_some() {
            return false;
        }
        let latest = self.offered.lock().unwrap();
        if let Some(offered) = latest.as_ref() {
            let _ = tx.send(offered.clone());
        }
        self.fallback.lock().unwrap().push(tx);
        true
    }

    /// Supersedes in-flight transfers on a new selection or publication. The
    /// `current` lock orders this against accepting a completed read and
    /// against set().
    pub(crate) fn invalidate_reads(&self) -> u64 {
        let _current = self.current.lock().unwrap();
        self.epoch.fetch_add(1, Ordering::Relaxed).wrapping_add(1)
    }

    fn host_changed(&self, epoch: u64, clip: Clip) {
        let mut current = self.current.lock().unwrap();
        if self.epoch.load(Ordering::Relaxed) != epoch || current.as_ref() == Some(&clip) {
            return;
        }
        // The last Android offer must not be replayed after a host selection
        // replaces it, even when the display connection is recreated. The
        // offer's source keeps any in-flight copy alive.
        *self.offered.lock().unwrap() = None;
        crate::info!(
            "clipboard: host selection ({}{})",
            match &clip.content {
                Content::Text(t) => format!("{} bytes of text", t.len()),
                Content::Files(f) => format!("{} files", f.0.len()),
                Content::Image(i) => format!("{}x{} image", i.width, i.height),
            },
            if clip.sensitive { ", sensitive" } else { "" }
        );
        match clip.event() {
            Ok((body, fd)) => self.subs.broadcast(ev::CLIP, &body, &[fd.as_fd()], None),
            Err(e) => crate::warn!("clipboard: memfd: {e}"),
        }
        *current = Some(clip);
    }
}

pub fn encode_png(i: &Image) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, i.width as u32, i.height as u32);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header()?.write_image_data(&i.rgba)?;
    Ok(out)
}

/// Decodes a host PNG to RGBA8, refusing anything over the size cap.
pub fn decode_png(b: &[u8]) -> Result<Image> {
    let mut dec = png::Decoder::new(std::io::Cursor::new(b));
    dec.set_transformations(
        png::Transformations::normalize_to_color8() | png::Transformations::ALPHA,
    );
    dec.set_limits(png::Limits {
        bytes: (MAX_IMAGE_SIDE * MAX_IMAGE_SIDE * 4) as usize,
    });
    let mut r = dec.read_info()?;
    let (w, h) = (r.info().width, r.info().height);
    anyhow::ensure!(
        w > 0 && h > 0 && w <= MAX_IMAGE_SIDE && h <= MAX_IMAGE_SIDE,
        "image {w}x{h} too large"
    );
    let mut buf = vec![0; r.output_buffer_size().context("image size")?];
    let info = r.next_frame(&mut buf)?;
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::GrayscaleAlpha => buf
            .as_chunks::<2>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[0], p[0], p[1]])
            .collect(),
        t => anyhow::bail!("unexpected color type {t:?}"),
    };
    anyhow::ensure!(rgba.len() == (w * h * 4) as usize, "short image");
    Ok(Image {
        width: w as u16,
        height: h as u16,
        rgba,
    })
}

struct State {
    hub: std::sync::Weak<ClipboardHub>,
    manager: ExtDataControlManagerV1,
    device: ExtDataControlDeviceV1,
    /// MIME types of each live offer.
    offers: HashMap<ObjectId, Vec<String>>,
    source: Option<(ExtDataControlSourceV1, Arc<Offered>)>,
}

fn run(
    conn: Connection,
    mut queue: wayland_client::EventQueue<State>,
    state: &mut State,
    rx: mpsc::Receiver<Cmd>,
    wake: OwnedFd,
) -> Result<()> {
    let qh = queue.handle();
    loop {
        queue.dispatch_pending(state)?;
        queue.flush()?;
        if state.hub.upgrade().is_none() {
            return Ok(());
        }
        let guard = queue.prepare_read();
        let readable = {
            let wl = guard.as_ref().map(|g| g.connection_fd());
            let mut fds = vec![PollFd::new(&wake, PollFlags::IN)];
            if let Some(fd) = wl.as_ref() {
                fds.push(PollFd::new(fd, PollFlags::IN));
            }
            match rustix::event::poll(&mut fds, None) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => return Err(e.into()),
            }
            fds.get(1).is_some_and(|f| !f.revents().is_empty())
        };
        if let Some(g) = guard
            && readable
        {
            match g.read() {
                Ok(_) => {}
                Err(wayland_client::backend::WaylandError::Io(e))
                    if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(e.into()),
            }
        }
        let mut b = [0u8; 64];
        while matches!(rustix::io::read(&wake, &mut b), Ok(n) if n > 0) {}
        while let Ok(Cmd::Set(offered)) = rx.try_recv() {
            let src = state.manager.create_data_source(&qh, ());
            for ty in offered.types.keys() {
                src.offer(ty.clone());
            }
            state.device.set_selection(Some(&src));
            if let Some((old, _)) = state.source.replace((src, offered)) {
                old.destroy();
            }
        }
        let _ = conn.flush();
    }
}

impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wl_seat::WlSeat,
        _: wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &ExtDataControlManagerV1,
        _: <ExtDataControlManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ExtDataControlDeviceV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ExtDataControlDeviceV1,
        event: ext_data_control_device_v1::Event,
        _: &(),
        conn: &Connection,
        _: &QueueHandle<Self>,
    ) {
        use ext_data_control_device_v1::Event;
        match event {
            Event::DataOffer { id } => {
                state.offers.insert(id.id(), Vec::new());
            }
            Event::Selection { id } => {
                let Some(offer) = id else {
                    if let Some(hub) = state.hub.upgrade() {
                        hub.invalidate_reads();
                    }
                    return;
                };
                let types = state.offers.get(&offer.id()).cloned().unwrap_or_default();
                // An offer is valid only until the next selection, so the
                // others are dropped.
                state.offers.retain(|k, _| *k == offer.id());
                if let Some(hub) = state.hub.upgrade() {
                    receive(&offer, &types, conn, hub);
                }
            }
            Event::PrimarySelection { id: Some(o) } => {
                state.offers.remove(&o.id());
                o.destroy();
            }
            Event::Finished => crate::warn!("clipboard: data device finished"),
            _ => {}
        }
    }

    event_created_child!(State, ExtDataControlDeviceV1, [
        ext_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ExtDataControlOfferV1, ()),
    ]);
}

/// Starts reading `offer` on a helper thread.
fn receive(
    offer: &ExtDataControlOfferV1,
    types: &[String],
    conn: &Connection,
    hub: Arc<ClipboardHub>,
) {
    receive_offer(types, conn, hub, |mime, fd| offer.receive(mime.into(), fd));
}

/// Reads a host offer with the MIME negotiation and bounded reader that both
/// Wayland protocols share.
pub(crate) fn receive_offer(
    types: &[String],
    conn: &Connection,
    hub: Arc<ClipboardHub>,
    receive: impl FnOnce(&str, std::os::fd::BorrowedFd<'_>),
) {
    let epoch = hub.invalidate_reads();
    if types.iter().any(|t| t == MARKER) {
        return;
    }
    let sensitive = types.iter().any(|t| t == PASSWORD_HINT);
    let Some(mime) = preferred_type(types) else {
        return;
    };
    let Ok((r, w)) = rustix::pipe::pipe_with(rustix::pipe::PipeFlags::CLOEXEC) else {
        return;
    };
    receive(mime, w.as_fd());
    drop(w);
    let _ = conn.flush();
    let mime = mime.to_owned();
    let _ = std::thread::Builder::new()
        .name("clipboard-read".into())
        .spawn(move || {
            let result = (|| -> Result<Content> {
                let max = if mime == "image/png" {
                    limits::CLIP_IMAGE
                } else {
                    limits::CLIP_TEXT
                };
                let bytes = read_capped(r, max as usize)?;
                match mime.as_str() {
                    "text/uri-list" => Ok(Content::Files(files::read_uri_list(&bytes)?)),
                    files::PORTAL_TYPE => Ok(Content::Files(files::retrieve(&bytes)?)),
                    "image/png" => Ok(Content::Image(decode_png(&bytes)?)),
                    _ => Ok(Content::Text(String::from_utf8(bytes)?)),
                }
            })();
            match result {
                Ok(content) => hub.host_changed(epoch, Clip { content, sensitive }),
                Err(e) => crate::warn!("clipboard: reading the host selection: {e:#}"),
            }
        });
}

pub(crate) fn preferred_type(types: &[String]) -> Option<&str> {
    [files::PORTAL_TYPE, "text/uri-list"]
        .into_iter()
        .chain(TEXT_TYPES)
        .chain(["image/png"])
        .find(|t| types.iter().any(|o| o == t))
}

pub(crate) fn serve(offered: &Offered, mime: String, fd: OwnedFd) {
    let Some(data) = offered.types.get(&mime).cloned() else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("clipboard-write".into())
        .spawn(move || {
            if let Err(e) = write_all_timeout(fd, &data) {
                crate::warn!("clipboard: serving {mime}: {e}");
            }
        });
}

/// Reads until EOF, at most `max` bytes and [`TRANSFER_TIMEOUT`].
fn read_capped(fd: OwnedFd, max: usize) -> Result<Vec<u8>> {
    rustix::fs::fcntl_setfl(&fd, rustix::fs::OFlags::NONBLOCK)?;
    let deadline = Instant::now() + TRANSFER_TIMEOUT;
    let mut f = std::fs::File::from(fd);
    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!left.is_zero(), "timed out");
        let mut p = [PollFd::new(&f, PollFlags::IN)];
        let ts = rustix::time::Timespec::try_from(left).unwrap_or_default();
        match rustix::event::poll(&mut p, Some(&ts)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
        match f.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                anyhow::ensure!(out.len() + n <= max, "over {max} bytes");
                out.extend_from_slice(&buf[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.into()),
        }
    }
}

/// Writes `data` to a receiver, giving up after [`TRANSFER_TIMEOUT`].
fn write_all_timeout(fd: OwnedFd, data: &[u8]) -> Result<()> {
    rustix::fs::fcntl_setfl(&fd, rustix::fs::OFlags::NONBLOCK)?;
    let deadline = Instant::now() + TRANSFER_TIMEOUT;
    let mut off = 0;
    while off < data.len() {
        let left = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!left.is_zero(), "timed out");
        let mut p = [PollFd::new(&fd, PollFlags::OUT)];
        let ts = rustix::time::Timespec::try_from(left).unwrap_or_default();
        match rustix::event::poll(&mut p, Some(&ts)) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
        match rustix::io::write(&fd, &data[off..]) {
            Ok(n) => off += n,
            Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}

impl Dispatch<ExtDataControlOfferV1, ()> for State {
    fn event(
        state: &mut Self,
        offer: &ExtDataControlOfferV1,
        event: ext_data_control_offer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let ext_data_control_offer_v1::Event::Offer { mime_type } = event
            && let Some(t) = state.offers.get_mut(&offer.id())
            && t.len() < 64
        {
            t.push(mime_type);
        }
    }
}

impl Dispatch<ExtDataControlSourceV1, ()> for State {
    fn event(
        state: &mut Self,
        src: &ExtDataControlSourceV1,
        event: ext_data_control_source_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            ext_data_control_source_v1::Event::Send { mime_type, fd } => {
                let Some((cur, offered)) = state.source.as_ref() else {
                    return;
                };
                if cur != src {
                    return;
                }
                serve(offered, mime_type, fd);
            }
            ext_data_control_source_v1::Event::Cancelled => {
                if state.source.as_ref().is_some_and(|(s, _)| s == src) {
                    state.source = None;
                }
                src.destroy();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Clip {
        Clip {
            content: Content::Text(s.into()),
            sensitive: false,
        }
    }

    #[test]
    fn superseded_host_reads_cannot_replace_a_newer_clip() {
        let hub = ClipboardHub {
            cmd: None,
            fallback: Mutex::new(Vec::new()),
            offered: Mutex::new(None),
            cache: std::path::PathBuf::new(),
            subs: Subscribers::default(),
            current: Mutex::new(None),
            epoch: AtomicU64::new(0),
        };
        let old_host_read = hub.invalidate_reads();
        hub.set(text("newer Android copy")).unwrap();
        hub.host_changed(old_host_read, text("old host snapshot"));
        assert_eq!(
            *hub.current.lock().unwrap(),
            Some(text("newer Android copy"))
        );
        let first = hub.invalidate_reads();
        let second = hub.invalidate_reads();
        hub.host_changed(second, text("newer host copy"));
        hub.host_changed(first, text("slower old host copy"));
        assert_eq!(*hub.current.lock().unwrap(), Some(text("newer host copy")));
        assert!(hub.offered.lock().unwrap().is_none());
    }
}
