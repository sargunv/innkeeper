//! The system service (id 3) is the channel between the bridge and the
//! Innkeeper System app. It carries desktop settings to Android, Android
//! notifications, the clipboard in both directions, web links to open, and
//! media sessions. [`proto`] defines the messages. Each feature has its own
//! module and talks to the desktop over D-Bus or Wayland:
//!
//! - `settings`: portal Settings (dark mode, accent, contrast, clock format,
//!   text scale), the time zone and the locale
//! - `notify`: `org.freedesktop.Notifications`
//! - `clipboard`: the Wayland clipboard through `ext-data-control-v1`
//! - `openuri`: external URI validation and the handoff rate limit
//! - `handoff`: the OpenURI portal, files, desktop share targets,
//!   FileChooser SaveFile and Email
//! - `mpris`: MediaSessions as MPRIS players
//! - `lifecycle`: the report of what keeps the session up, written to
//!   `<run-dir>/lifecycle` for the launcher's idle shutdown (docs/session.md,
//!   "Lifecycle"), and the indicator for apps running in the background

pub mod clipboard;
pub mod handoff;
mod launcher_entry;
mod lifecycle;
pub mod mpris;
pub mod notify;
pub mod openuri;
pub mod proto;
pub mod settings;

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::ipc::blob::{self, Doc};
use crate::ipc::server::{Events, Peer, Reply, Service};
use crate::ipc::wire::errno::*;
use clipboard::{Clip, ClipboardHub, Content};
use mpris::{Media, MprisHub};
use notify::{Note, NotifyHub};
use openuri::RateLimit;
use proto::{ClipKind, Lifecycle, PlaybackApp, Request};
use settings::SettingsHub;

/// Bridge-wide state, created once in `main.rs`. Each part starts the first
/// time a client asks for it.
pub struct Shared {
    art_dir: PathBuf,
    handoffs: Arc<handoff::Handoffs>,
    /// The `<run-dir>/lifecycle` report.
    lifecycle: lifecycle::Lifecycles,
    input: Arc<crate::input::Hub>,
    /// `$XDG_CACHE_HOME/innkeeper-<profile>/clipboard`, which holds files
    /// copied in Android and served to the desktop.
    clipboard_cache: PathBuf,
    /// False in a test session, which has no desktop effects. That includes
    /// clipboard reads and user-facing portals.
    desktop: bool,
    dbus: OnceLock<Option<zbus::blocking::Connection>>,
    settings: OnceLock<Arc<SettingsHub>>,
    notify: OnceLock<Option<Arc<NotifyHub>>>,
    clipboard: OnceLock<Option<Arc<ClipboardHub>>>,
    mpris: OnceLock<Arc<MprisHub>>,
    next_client: std::sync::atomic::AtomicU64,
    /// Audio connections whose playback streams take the playing app's name.
    /// An entry's weak pointer goes stale when its connection closes.
    playback: std::sync::Mutex<Vec<PlaybackSlot>>,
}

/// One audio connection. `app` labels streams opened later, and `streams`
/// receives the label for streams already open.
struct PlaybackSlot {
    app: Arc<std::sync::Mutex<Option<crate::audio::stream::PlaybackApp>>>,
    streams: std::sync::Weak<std::sync::Mutex<HashMap<u32, crate::audio::stream::StreamHandle>>>,
}

impl Shared {
    /// `run_dir` is the session's runtime dir, not the socket dir that
    /// Android shares. MPRIS artwork goes in `art/` and the lifecycle report
    /// in `lifecycle`.
    pub fn new(
        run_dir: &std::path::Path,
        desktop: bool,
        input: Arc<crate::input::Hub>,
        profile: &innkeeper_profile::profile::ProfileName,
    ) -> Arc<Shared> {
        if !desktop {
            crate::info!("system: test session (log-only sinks)");
        }
        let shared = Arc::new(Shared {
            art_dir: run_dir.join("art"),
            handoffs: handoff::Handoffs::new(input.clone(), profile),
            lifecycle: lifecycle::Lifecycles::new(run_dir.join("lifecycle")),
            input: input.clone(),
            clipboard_cache: innkeeper_profile::paths::xdg_home("XDG_CACHE_HOME", ".cache")
                .join(format!("innkeeper-{profile}/clipboard")),
            desktop,
            dbus: OnceLock::new(),
            settings: OnceLock::new(),
            notify: OnceLock::new(),
            clipboard: OnceLock::new(),
            mpris: OnceLock::new(),
            next_client: Default::default(),
            playback: std::sync::Mutex::new(Vec::new()),
        });
        let weak = Arc::downgrade(&shared);
        input.watch_windows(move || {
            if let Some(s) = weak.upgrade() {
                s.write_lifecycle();
            }
        });
        shared
    }

    /// Writes the report for the launcher with the bridge's own windows
    /// added, and shows the indicator while only background-allowed apps
    /// keep the session up.
    fn write_lifecycle(&self) -> Reply {
        let own = self.input.own_windows();
        let mut state = self.lifecycle.state.lock().unwrap();
        let Some(r) = &state.report else {
            return Reply::ok();
        };
        let text = lifecycle::text(r, own);
        let path = &self.lifecycle.path;
        let tmp = path.with_extension("tmp");
        let changed = std::fs::read_to_string(path).ok().as_deref() != Some(text.as_str());
        if let Err(e) = std::fs::write(&tmp, &text).and_then(|()| std::fs::rename(&tmp, path)) {
            crate::warn!("lifecycle: write {}: {e}", path.display());
            return Reply::err(-EIO);
        }
        if changed {
            crate::info!("lifecycle: {}", text.trim_end().replace('\n', " "));
        }
        let want = lifecycle::indicator(r, own).map(|labels| (r.client, labels));
        if want == state.indicator {
            return Reply::ok();
        }
        if !self.desktop {
            crate::info!(
                "lifecycle: background indicator {:?} (test session)",
                want.as_ref().map(|w| &w.1)
            );
        } else if let Some(hub) = self.notify() {
            if let Some((client, _)) = &state.indicator
                && want.as_ref().is_none_or(|w| w.0 != *client)
            {
                hub.close(*client, INDICATOR_ID);
            }
            if let Some((client, labels)) = &want {
                let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
                if let Err(e) =
                    hub.post(*client, &r.events, INDICATOR_ID, &background_note(&labels))
                {
                    crate::warn!("lifecycle: indicator: {e:#}");
                }
            }
        }
        state.indicator = want;
        Reply::ok()
    }

    /// Registers an audio connection whose streams carry the playing app's
    /// name. Returns the app that labels streams the connection opens later.
    pub fn register_playback(
        &self,
        streams: &Arc<std::sync::Mutex<HashMap<u32, crate::audio::stream::StreamHandle>>>,
    ) -> Arc<std::sync::Mutex<Option<crate::audio::stream::PlaybackApp>>> {
        let app = Arc::new(std::sync::Mutex::new(None));
        self.playback.lock().unwrap().push(PlaybackSlot {
            app: app.clone(),
            streams: Arc::downgrade(streams),
        });
        app
    }

    fn set_playback_app(&self, app: Option<crate::audio::stream::PlaybackApp>) {
        let mut slots = self.playback.lock().unwrap();
        slots.retain(|s| s.streams.strong_count() > 0);
        // Copy the senders out first so that the sends happen without
        // holding the connection's stream map, which OPEN also locks.
        let mut send = Vec::new();
        for slot in slots.iter() {
            *slot.app.lock().unwrap() = app.clone();
            if let Some(streams) = slot.streams.upgrade() {
                send.extend(streams.lock().unwrap().values().map(|s| s.sender()));
            }
        }
        drop(slots);
        for stream in &send {
            stream.send(crate::audio::stream::Ctl::App(app.clone()));
        }
    }

    /// Removes desktop state that would outlive the bridge. Notifications stay
    /// up after their sender exits, so they are closed here. MPRIS players
    /// and the clipboard source go away with the process.
    pub fn shutdown(&self) {
        self.handoffs.clear();
        if let Some(Some(n)) = self.notify.get() {
            n.close_everything();
        }
    }

    fn dbus(&self) -> Option<&zbus::blocking::Connection> {
        if !self.desktop {
            return None;
        }
        self.dbus
            .get_or_init(|| match zbus::blocking::Connection::session() {
                Ok(c) => Some(c),
                Err(e) => {
                    crate::warn!("system: no session bus: {e}");
                    None
                }
            })
            .as_ref()
    }

    pub fn refresh_preferences(&self) {
        self.settings().refresh_preferences();
    }

    fn settings(&self) -> &Arc<SettingsHub> {
        self.settings
            .get_or_init(|| SettingsHub::start(self.dbus().cloned()))
    }

    fn notify(&self) -> Option<&Arc<NotifyHub>> {
        if !self.desktop {
            return None;
        }
        self.notify
            .get_or_init(|| {
                let conn = self.dbus()?.clone();
                NotifyHub::start(conn)
                    .map_err(|e| crate::warn!("notify: no notification server: {e:#}"))
                    .ok()
            })
            .as_ref()
    }

    pub(crate) fn clipboard(&self) -> Option<&Arc<ClipboardHub>> {
        if !self.desktop {
            return None;
        }
        self.clipboard
            .get_or_init(|| {
                ClipboardHub::start(&self.clipboard_cache)
                    .map_err(|e| crate::warn!("clipboard: unavailable: {e:#}"))
                    .ok()
            })
            .as_ref()
    }

    fn mpris(&self) -> &Arc<MprisHub> {
        self.mpris
            .get_or_init(|| MprisHub::new(self.art_dir.clone()))
    }
}

/// The background indicator's notification id. The system app numbers its
/// own notifications from 1, so the ids do not collide.
const INDICATOR_ID: u32 = u32::MAX;

/// The background indicator, a resident notification that names the apps
/// keeping the session running with no window open.
fn background_note(labels: &[&str]) -> Note {
    Note {
        package: "dev.sargunv.innkeeper.system".into(),
        app_name: "Innkeeper".into(),
        title: "Running in the background".into(),
        body: labels.join(", "),
        flags: notify::flags::ONGOING | notify::flags::SILENT,
        urgency: 0,
        progress: -1,
        actions: vec![],
        icon: None,
        picture: None,
    }
}

pub struct SystemService {
    shared: Arc<Shared>,
    events: Events,
    label: String,
    /// This connection's key in the hubs.
    client: u64,
    settings_sub: Option<u64>,
    clip_sub: Option<u64>,
    handoff_rate: RateLimit,
}

impl SystemService {
    pub fn new(shared: Arc<Shared>, peer: &Peer, events: Events) -> SystemService {
        let client = shared
            .next_client
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        SystemService {
            shared,
            events,
            label: peer.label.clone(),
            client,
            settings_sub: None,
            clip_sub: None,
            handoff_rate: RateLimit::default(),
        }
    }

    fn start(&mut self) -> Reply {
        if self.settings_sub.is_none() {
            self.settings_sub = Some(self.shared.settings().subscribe(self.events.clone()));
        }
        if let Some(c) = self.shared.clipboard()
            && self.clip_sub.is_none()
        {
            self.clip_sub = Some(c.subscribe(self.events.clone()));
        }
        let notifications = !self.shared.desktop || self.shared.notify().is_some();
        crate::info!(
            "system: {}: started (notifications {notifications})",
            self.label
        );
        let mut w = crate::ipc::wire::Writer::default();
        w.u8(notifications as u8);
        Reply::with_body(w.into_vec())
    }

    fn notify(&mut self, id: u32, len: u32, fd: &OwnedFd) -> Reply {
        let b = match blob::read(fd, len, proto::limits::NOTIFY_BLOB as usize) {
            Ok(b) => b,
            Err(e) => return Reply::err(e),
        };
        let note = match Note::decode(&b) {
            Ok(n) => n,
            Err(e) => return Reply::err(e.status()),
        };
        if !self.shared.desktop {
            crate::info!(
                "notify: {}: not shown (test session): {} {:?} progress={} badge={}",
                self.label,
                note.package,
                crate::ipc::wire::truncate(&note.title, 80),
                note.progress,
                note.flags & notify::flags::BADGE != 0
            );
            return Reply::ok();
        }
        let Some(hub) = self.shared.notify() else {
            return Reply::err(-ENOSYS);
        };
        if !hub.contains(self.client, id) && hub.count(self.client) >= notify::MAX_PER_CLIENT {
            return Reply::err(-EBUSY);
        }
        match hub.post(self.client, &self.events, id, &note) {
            Ok(host) => {
                crate::debug!("notify: {} -> {host} ({})", id, note.package);
                Reply::ok()
            }
            Err(e) => {
                crate::warn!("notify: {e:#}");
                Reply::err(-EIO)
            }
        }
    }

    fn clip_set(&mut self, kind: ClipKind, sensitive: bool, len: u32, fd: &OwnedFd) -> Reply {
        let Some(hub) = self.shared.clipboard() else {
            return Reply::err(-ENOSYS);
        };
        let b = match blob::read(fd, len, kind.max_len() as usize) {
            Ok(b) => b,
            Err(e) => return Reply::err(e),
        };
        let content = match kind {
            ClipKind::File => match clipboard::files::ClipFiles::decode(&b) {
                Ok(f) => Content::Files(f),
                Err(_) => return Reply::err(-EINVAL),
            },
            ClipKind::Text => match String::from_utf8(b) {
                Ok(t) if !t.contains('\0') => Content::Text(t),
                _ => return Reply::err(-EINVAL),
            },
            ClipKind::Image => {
                let mut d = Doc::new(&b);
                match (d.image(2048), d.end()) {
                    (Ok(Some(i)), Ok(())) => Content::Image(i),
                    _ => return Reply::err(-EINVAL),
                }
            }
        };
        crate::info!(
            "clipboard: {}: set ({:?}, {len} bytes{})",
            self.label,
            kind,
            if sensitive { ", sensitive" } else { "" }
        );
        match hub.set(Clip { content, sensitive }) {
            Ok(()) => Reply::ok(),
            Err(e) => {
                crate::warn!("clipboard: {e:#}");
                Reply::err(-EIO)
            }
        }
    }

    /// Takes the system app's report of what keeps the session up.
    fn lifecycle(&mut self, l: &Lifecycle, background: &[(String, String)]) -> Reply {
        self.shared.lifecycle.state.lock().unwrap().report = Some(lifecycle::Report {
            client: self.client,
            events: self.events.clone(),
            lifecycle: *l,
            background: background.to_vec(),
        });
        self.shared.write_lifecycle()
    }

    fn media(&mut self, player: u32, len: u32, fd: &OwnedFd) -> Reply {
        let b = match blob::read(fd, len, proto::limits::MEDIA_BLOB as usize) {
            Ok(b) => b,
            Err(e) => return Reply::err(e),
        };
        let m = match Media::decode(&b) {
            Ok(m) => m,
            Err(e) => return Reply::err(e.status()),
        };
        if !self.shared.desktop {
            crate::debug!(
                "mpris: {}: not published (test session): {}",
                self.label,
                m.package
            );
            return Reply::ok();
        }
        let hub = self.shared.mpris();
        if !hub.contains(self.client, player) && hub.count(self.client) >= mpris::MAX_PER_CLIENT {
            return Reply::err(-EBUSY);
        }
        match hub.update(self.client, &self.events, player, m) {
            Ok(()) => Reply::ok(),
            Err(e) => {
                crate::warn!("mpris: {e:#}");
                Reply::err(-EIO)
            }
        }
    }
}

impl Service for SystemService {
    fn handle(&mut self, t: u16, body: &[u8], mut fds: Vec<OwnedFd>) -> Reply {
        let req = match proto::decode(t, body) {
            Ok(r) => r,
            Err(e) => return Reply::err(e.status()),
        };
        // Metadata blobs take one fd. HANDOFF can add up to three content fds.
        if fds.len() != req.fd_count() {
            return Reply::err(-EPROTO);
        }
        if let Request::Handoff {
            window,
            action,
            files,
            ask,
            len,
        } = req
        {
            let metadata = fds.remove(0);
            let bytes = match blob::read(&metadata, len, 64 << 10) {
                Ok(b) => b,
                Err(e) => return Reply::err(e),
            };
            let payload = match handoff::Payload::decode(&bytes, files) {
                Ok(p) => p,
                Err(e) => return Reply::err(e.status()),
            };
            if action == 1 && openuri::validate(&payload.text).is_err() {
                return Reply::err(-EINVAL);
            }
            if !self.handoff_rate.allow(Instant::now()) {
                return Reply::err(-EAGAIN);
            }
            if !self.shared.desktop {
                crate::info!(
                    "handoff: {}: not opened (test session): action {action}, {} files, {}",
                    self.label,
                    files,
                    crate::ipc::wire::truncate(&payload.text, 200)
                );
                return Reply::ok();
            }
            let Some(conn) = self.shared.dbus().cloned() else {
                return Reply::err(-ENOSYS);
            };
            let handoffs = self.shared.handoffs.clone();
            let Some(slot) = handoffs.slot() else {
                return Reply::err(-EBUSY);
            };
            return match std::thread::Builder::new()
                .name("desktop-handoff".into())
                .spawn(move || {
                    let result = handoffs.run(&conn, window, action, ask, payload, fds);
                    if let Err(e) = result {
                        crate::warn!("handoff: action {action} ended: {e}");
                    }
                    drop(slot);
                }) {
                Ok(_) => Reply::ok(),
                Err(_) => Reply::err(-EAGAIN),
            };
        }
        let fd = fds.pop();
        let fd = || fd.as_ref().expect("checked above");
        match req {
            Request::Handoff { .. } => unreachable!("handled above"),
            Request::Start => self.start(),
            Request::Notify { id, len } => self.notify(id, len, fd()),
            Request::NotifyClose { id } => {
                if let Some(h) = self.shared.notify() {
                    h.close(self.client, id);
                }
                Reply::ok()
            }
            Request::ClipSet {
                kind,
                sensitive,
                len,
            } => self.clip_set(kind, sensitive, len, fd()),
            Request::Media { player, len } => self.media(player, len, fd()),
            Request::MediaClose { player } => {
                if self.shared.desktop {
                    self.shared.mpris().remove(self.client, player);
                }
                Reply::ok()
            }
            Request::PlaybackApp(app) => {
                self.shared.set_playback_app(mixer_app(&app));
                Reply::ok()
            }
            Request::Lifecycle(l) => {
                let apps = if l.len == 0 {
                    Ok(vec![])
                } else {
                    blob::read(fd(), l.len, proto::limits::LIFECYCLE_BLOB as usize)
                        .and_then(|b| proto::background_apps(&b).map_err(|e| e.status()))
                };
                match apps {
                    Ok(apps) => self.lifecycle(&l, &apps),
                    Err(e) => Reply::err(e),
                }
            }
        }
    }
}

impl Drop for SystemService {
    fn drop(&mut self) {
        if let Some(k) = self.settings_sub {
            self.shared.settings().unsubscribe(k);
        }
        if let (Some(k), Some(c)) = (self.clip_sub, self.shared.clipboard()) {
            c.unsubscribe(k);
        }
        if let Some(Some(h)) = self.shared.notify.get() {
            h.close_all(self.client);
        }
        if let Some(m) = self.shared.mpris.get() {
            m.remove_all(self.client);
        }
        // The launcher treats a missing report as busy until the system app
        // reconnects. The connection's notifications, the indicator among
        // them, closed above.
        let mut state = self.shared.lifecycle.state.lock().unwrap();
        if state
            .report
            .as_ref()
            .is_some_and(|r| r.client == self.client)
        {
            state.report = None;
            let _ = std::fs::remove_file(&self.shared.lifecycle.path);
        }
        if state.indicator.as_ref().is_some_and(|i| i.0 == self.client) {
            state.indicator = None;
        }
        crate::info!("system: {}: closed", self.label);
    }
}

/// The mixer label for a PLAYBACK_APP. An empty package restores the name,
/// icon and id the stream was opened with. The icon is the desktop entry id,
/// the same one `innkeeper-core`'s entries use.
fn mixer_app(app: &PlaybackApp) -> Option<crate::audio::stream::PlaybackApp> {
    if app.package.is_empty() {
        return None;
    }
    let id = innkeeper_core::desktop::app_id(&app.package);
    let name = if app.label.is_empty() {
        app.package.clone()
    } else {
        app.label.clone()
    };
    Some(crate::audio::stream::PlaybackApp {
        name,
        icon_name: id.clone(),
        id: id.clone(),
        binary: id,
    })
}

/// Checks for an Android package name: 1 to 255 bytes of `[A-Za-z0-9_.]`,
/// not starting with a dot. Package names end up in D-Bus names and file
/// names, so nothing else is accepted.
pub fn valid_package(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 255
        && !p.starts_with('.')
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_app_uses_the_desktop_icon_id() {
        let named = mixer_app(&PlaybackApp {
            package: "org.example.player".into(),
            label: "Example Player".into(),
        })
        .unwrap();
        assert_eq!(named.name, "Example Player");
        assert_eq!(named.icon_name, "dev.sargunv.Innkeeper.org.example.player");
        assert_eq!(named.id, named.icon_name);
        assert_eq!(named.binary, named.icon_name);
        let bare = mixer_app(&PlaybackApp {
            package: "org.example.player".into(),
            label: String::new(),
        })
        .unwrap();
        assert_eq!(bare.name, "org.example.player");
        assert!(mixer_app(&PlaybackApp::default()).is_none());
    }

    #[test]
    fn test_session_does_not_start_desktop_hubs() {
        let dir = crate::test_dir("system-test-policy");
        let shared = Shared::new(
            &dir,
            false,
            crate::input::Hub::new(),
            &innkeeper_profile::profile::ProfileName::new("t").unwrap(),
        );
        // Gating the hubs blocks clipboard publication and subscriptions.
        // Even a NOTIFY_CLOSE must not connect to the real notification
        // server.
        assert!(shared.clipboard().is_none());
        assert!(shared.notify().is_none());
        assert!(shared.dbus().is_none());
        assert!(shared.clipboard.get().is_none());
        assert!(shared.notify.get().is_none());
        assert!(shared.dbus.get().is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
