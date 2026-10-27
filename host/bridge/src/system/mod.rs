//! The system service (id 3): the channel between the bridge and the
//! Linux.apk System app. It carries desktop settings to Android, Android
//! notifications, the clipboard both ways, web links to open, and media
//! sessions (`docs/bridge.md`, "System service"). Each feature lives in its
//! own module and talks to the desktop over D-Bus or Wayland:
//!
//! - `settings`: portal Settings (dark mode, accent, contrast, clock format,
//!   text scale), the time zone and the locale
//! - `notify`: `org.freedesktop.Notifications`
//! - `clipboard`: the Wayland clipboard through `ext-data-control-v1`
//! - `openuri`: the OpenURI portal (http and https only)
//! - `mpris`: MediaSessions as MPRIS players
//!
//! Only the system app's uid (and the static allow-list, which holds
//! Android root for tests) may open it: `main.rs` resolves that uid from the
//! owner of the app's data directory at connect time.

pub mod clipboard;
pub mod mpris;
pub mod notify;
pub mod openuri;
pub mod proto;
pub mod settings;

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
use proto::{ClipKind, Request, feature};
use settings::SettingsHub;

/// What a session may do on the real desktop. Test sessions (lane instances,
/// the compat runner) get log-only sinks, so nothing opens in the user's
/// browser and nothing appears in their notifications or media controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Policy {
    /// OpenURI really opens links (else: logged only).
    pub open_urls: bool,
    /// Notifications and MPRIS players are published (else: logged only).
    pub presence: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            open_urls: true,
            presence: true,
        }
    }
}

/// Bridge-wide state, created once in `main.rs`; each part starts the first
/// time a client asks for it.
pub struct Shared {
    art_dir: PathBuf,
    policy: Policy,
    dbus: OnceLock<Option<zbus::blocking::Connection>>,
    settings: OnceLock<Arc<SettingsHub>>,
    notify: OnceLock<Option<Arc<NotifyHub>>>,
    clipboard: OnceLock<Option<Arc<ClipboardHub>>>,
    mpris: OnceLock<Arc<MprisHub>>,
    next_client: std::sync::atomic::AtomicU64,
}

impl Shared {
    /// `art_dir`: where MPRIS artwork goes (not shared with Android).
    pub fn with_policy(art_dir: PathBuf, policy: Policy) -> Arc<Shared> {
        if policy != Policy::default() {
            crate::info!("system: test session: {policy:?} (log-only sinks)");
        }
        Arc::new(Shared {
            art_dir,
            policy,
            dbus: OnceLock::new(),
            settings: OnceLock::new(),
            notify: OnceLock::new(),
            clipboard: OnceLock::new(),
            mpris: OnceLock::new(),
            next_client: Default::default(),
        })
    }

    /// Takes down what outlives a connection on the desktop: notifications
    /// stay up after their sender exits, so close them. (MPRIS players and the
    /// clipboard source go away with the process.)
    pub fn shutdown(&self) {
        if let Some(Some(n)) = self.notify.get() {
            n.close_everything();
        }
    }

    fn dbus(&self) -> Option<&zbus::blocking::Connection> {
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

    fn settings(&self) -> &Arc<SettingsHub> {
        self.settings
            .get_or_init(|| SettingsHub::start(self.dbus().cloned()))
    }

    fn notify(&self) -> Option<&Arc<NotifyHub>> {
        self.notify
            .get_or_init(|| {
                let conn = self.dbus()?.clone();
                NotifyHub::start(conn)
                    .map_err(|e| crate::warn!("notify: no notification server: {e:#}"))
                    .ok()
            })
            .as_ref()
    }

    fn clipboard(&self) -> Option<&Arc<ClipboardHub>> {
        self.clipboard
            .get_or_init(|| {
                ClipboardHub::start()
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

pub struct SystemService {
    shared: Arc<Shared>,
    events: Events,
    label: String,
    /// This connection's key in the hubs.
    client: u64,
    settings_sub: Option<u64>,
    clip_sub: Option<u64>,
    uri_rate: RateLimit,
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
            uri_rate: RateLimit::default(),
        }
    }

    fn start(&mut self, want: u32) -> Reply {
        let mut have = feature::OPEN_URI | feature::MEDIA;
        if want & feature::SETTINGS != 0 && self.settings_sub.is_none() {
            self.settings_sub = Some(self.shared.settings().subscribe(self.events.clone()));
        }
        have |= feature::SETTINGS;
        if want & feature::NOTIFICATIONS != 0
            && (!self.shared.policy.presence || self.shared.notify().is_some())
        {
            have |= feature::NOTIFICATIONS;
        }
        if want & feature::CLIPBOARD != 0
            && let Some(c) = self.shared.clipboard()
        {
            if self.clip_sub.is_none() {
                self.clip_sub = Some(c.subscribe(self.events.clone()));
            }
            have |= feature::CLIPBOARD;
        }
        crate::info!(
            "system: {}: started (wants {want:#x}, has {have:#x})",
            self.label
        );
        let mut w = crate::ipc::wire::Writer::default();
        w.u32(have & want);
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
        if !self.shared.policy.presence {
            crate::info!(
                "notify: {}: not shown (test session): {} {:?}",
                self.label,
                note.package,
                crate::ipc::wire::truncate(&note.title, 80)
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

    fn open_uri(&mut self, len: u32, fd: &OwnedFd) -> Reply {
        let b = match blob::read(fd, len, proto::limits::URI as usize) {
            Ok(b) => b,
            Err(e) => return Reply::err(e),
        };
        let Ok(url) = String::from_utf8(b) else {
            return Reply::err(-EINVAL);
        };
        if !self.uri_rate.allow(Instant::now()) {
            crate::warn!("openuri: {}: rate limited", self.label);
            return Reply::err(-EAGAIN);
        }
        if !self.shared.policy.open_urls {
            if let Err(why) = openuri::validate(&url) {
                crate::warn!("openuri: {}: refused ({why})", self.label);
                return Reply::err(-EINVAL);
            }
            // The compat runner looks for this line (bridge.log).
            crate::info!(
                "openuri: {}: not opened (test session): {}",
                self.label,
                crate::ipc::wire::truncate(&url, 512)
            );
            return Reply::ok();
        }
        match openuri::open(self.shared.dbus(), &url, &self.label) {
            Ok(()) => Reply::ok(),
            Err(e) => Reply::err(e),
        }
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
        if !self.shared.policy.presence {
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
        // Exactly one fd for blob requests, none otherwise.
        if fds.len() != req.takes_fd() as usize {
            return Reply::err(-EPROTO);
        }
        let fd = fds.pop();
        let fd = || fd.as_ref().expect("checked above");
        match req {
            Request::Start { features } => self.start(features),
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
            Request::OpenUri { len } => self.open_uri(len, fd()),
            Request::Media { player, len } => self.media(player, len, fd()),
            Request::MediaClose { player } => {
                if self.shared.policy.presence {
                    self.shared.mpris().remove(self.client, player);
                }
                Reply::ok()
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
        crate::info!("system: {}: closed", self.label);
    }
}

/// An Android package name: `[A-Za-z0-9_.]`, 1 to 255 bytes. These end up in
/// D-Bus names and file names, so nothing else gets through.
pub fn valid_package(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 255
        && !p.starts_with('.')
        && p.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'.')
}
