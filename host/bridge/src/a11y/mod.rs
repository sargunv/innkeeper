//! The accessibility service (id 7): Android apps' AccessibilityNodeInfo
//! trees on the desktop's AT-SPI bus, so screen readers and other assistive
//! technologies see them as they see native apps. Messages: [`proto`];
//! snapshots: [`wire`]; the mapping to AccessKit: [`tree`].
//!
//! The system app's accessibility service sends a snapshot per window
//! (full, then partial ones as apps change); each window with a tree gets
//! one `accesskit_unix::Adapter`, which shows it as a frame of the bridge's
//! AT-SPI application, named by the window's title. AT-SPI actions come back
//! as ACTION events, which the system app performs on the node. Whether
//! anything runs at all follows the desktop's AT-SPI switch
//! (`org.a11y.Status`): ACTIVE tells the system app, which enables its
//! Android service only while it is on.
//!
//! Keyboard focus comes from the seat ([`crate::input::Hub`]): AccessKit
//! sends focus events only for the focused window, as AT-SPI expects.
//!
//! Limits: every Android window is a frame of one AT-SPI application named
//! after this process (`linuxapk-bridge`), since AccessKit has one per
//! process. Extents are relative to the window, because Wayland tells no
//! client where its windows are, so a screen reader's mouse review can't find
//! Android nodes under the pointer.

pub mod proto;
pub mod tree;
pub mod wire;

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use accesskit::{ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate};

use crate::ipc::Subscribers;
use crate::ipc::blob;
use crate::ipc::server::{Events, Peer, Reply, Service};
use crate::ipc::wire::errno::*;
use proto::{Request, ev, limits};
use tree::WindowTree;

/// A window's tree and its adapter.
struct Window {
    conn: u64,
    tree: Arc<Mutex<WindowTree>>,
    adapter: Mutex<accesskit_unix::Adapter>,
}

/// Bridge-wide state, created once in `main.rs`.
pub struct Shared {
    /// False for test sessions on the user's own bus: never active.
    presence: bool,
    active: AtomicBool,
    watching: OnceLock<()>,
    subscribers: Subscribers,
    windows: Mutex<HashMap<u32, Arc<Window>>>,
    /// The window with keyboard focus.
    focus: Mutex<Option<u32>>,
    next_conn: AtomicU64,
}

impl Shared {
    pub fn new(presence: bool) -> Arc<Shared> {
        if !presence {
            crate::info!("a11y: test session: never active");
        }
        Arc::new(Shared {
            presence,
            active: AtomicBool::new(false),
            watching: OnceLock::new(),
            subscribers: Subscribers::default(),
            windows: Mutex::new(HashMap::new()),
            focus: Mutex::new(None),
            next_conn: AtomicU64::new(0),
        })
    }

    /// The seat's keyboard focus moved (to no window of ours: `None`).
    pub fn keyboard_focus(&self, window: Option<u32>) {
        *self.focus.lock().unwrap() = window;
        for (id, w) in self.windows.lock().unwrap().iter() {
            w.adapter
                .lock()
                .unwrap()
                .update_window_focus_state(Some(*id) == window);
        }
    }

    /// Starts following `org.a11y.Status` the first time a client asks.
    fn watch(self: &Arc<Self>) {
        if !self.presence {
            return;
        }
        self.watching.get_or_init(|| {
            let s = self.clone();
            let spawned = std::thread::Builder::new()
                .name("a11y-status".into())
                .spawn(move || {
                    if let Err(e) = s.follow_status() {
                        crate::warn!("a11y: no AT-SPI status: {e:#}");
                    }
                });
            if let Err(e) = spawned {
                crate::warn!("a11y: spawn: {e}");
            }
        });
    }

    fn follow_status(&self) -> anyhow::Result<()> {
        let conn = zbus::blocking::Connection::session()?;
        let status = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy>::new(&conn)
            .destination("org.a11y.Bus")?
            .path("/org/a11y/bus")?
            .interface("org.a11y.Status")?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?;
        let props = zbus::blocking::fdo::PropertiesProxy::builder(&conn)
            .destination("org.a11y.Bus")?
            .path("/org/a11y/bus")?
            .build()?;
        let read = || {
            ["IsEnabled", "ScreenReaderEnabled"]
                .into_iter()
                .any(|p| status.get_property::<bool>(p).unwrap_or(false))
        };
        let changes = props.receive_properties_changed()?;
        self.set_active(read());
        for _ in changes {
            self.set_active(read());
        }
        Ok(())
    }

    fn set_active(&self, on: bool) {
        if self.active.swap(on, Ordering::AcqRel) == on {
            return;
        }
        crate::info!(
            "a11y: assistive technologies {}",
            if on { "on" } else { "off" }
        );
        if !on {
            // The system app turns its service off and sends nothing more.
            self.windows.lock().unwrap().clear();
        }
        self.subscribers
            .broadcast(ev::ACTIVE, &proto::active_event(on), &[], None);
    }
}

struct Activation(Arc<Mutex<WindowTree>>);

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.0.lock().unwrap().full_update()
    }
}

struct Deactivation;

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {}
}

struct Actions {
    window: u32,
    tree: Arc<Mutex<WindowTree>>,
    events: Events,
}

impl ActionHandler for Actions {
    fn do_action(&mut self, r: ActionRequest) {
        let Some(e) = self.tree.lock().unwrap().action(self.window, &r) else {
            crate::info!("a11y: window {}: {:?} not supported", self.window, r.action);
            return;
        };
        if let Err(err) = self.events.try_send(ev::ACTION, &e.encode(), &[]) {
            crate::warn!("a11y: action: {err:#}");
        }
    }
}

pub struct A11yService {
    shared: Arc<Shared>,
    events: Events,
    conn: u64,
    subscription: Option<u64>,
    label: String,
}

impl A11yService {
    pub fn new(shared: Arc<Shared>, peer: &Peer, events: Events) -> A11yService {
        let conn = shared.next_conn.fetch_add(1, Ordering::Relaxed) + 1;
        A11yService {
            shared,
            events,
            conn,
            subscription: None,
            label: peer.label.clone(),
        }
    }

    fn tree(&mut self, window: u32, len: u32, fd: &OwnedFd) -> Reply {
        if !self.shared.active.load(Ordering::Acquire) {
            return Reply::err(-EAGAIN);
        }
        let bytes = match blob::read(fd, len, limits::TREE_BLOB as usize) {
            Ok(b) => b,
            Err(e) => return Reply::err(e),
        };
        let snap = match wire::decode(&bytes) {
            Ok(s) => s,
            Err(e) => {
                crate::warn!("a11y: window {window}: bad snapshot: {e}");
                return Reply::err(-EINVAL);
            }
        };
        let existing = self.shared.windows.lock().unwrap().get(&window).cloned();
        let existing = existing.filter(|w| w.conn == self.conn);
        match existing {
            Some(w) => {
                let update = match w.tree.lock().unwrap().apply(&snap) {
                    Ok(u) => u,
                    Err(e) => {
                        crate::warn!("a11y: window {window}: {e}");
                        return Reply::err(-EINVAL);
                    }
                };
                // The tree lock is released first: AccessKit's activation
                // takes the adapter's lock, then ours.
                w.adapter.lock().unwrap().update_if_active(|| update);
                Reply::ok()
            }
            None => {
                if snap.partial {
                    return Reply::err(-ENOENT);
                }
                let mut windows = self.shared.windows.lock().unwrap();
                if windows.values().filter(|w| w.conn == self.conn).count() >= limits::WINDOWS {
                    return Reply::err(-EBUSY);
                }
                let mut t = WindowTree::default();
                if let Err(e) = t.apply(&snap) {
                    crate::warn!("a11y: window {window}: {e}");
                    return Reply::err(-EINVAL);
                }
                let tree = Arc::new(Mutex::new(t));
                let mut adapter = accesskit_unix::Adapter::new(
                    Activation(tree.clone()),
                    Actions {
                        window,
                        tree: tree.clone(),
                        events: self.events.clone(),
                    },
                    Deactivation,
                );
                adapter
                    .update_window_focus_state(*self.shared.focus.lock().unwrap() == Some(window));
                crate::info!(
                    "a11y: window {window} ({:?}): {} nodes",
                    snap.title,
                    snap.nodes.len()
                );
                windows.insert(
                    window,
                    Arc::new(Window {
                        conn: self.conn,
                        tree,
                        adapter: Mutex::new(adapter),
                    }),
                );
                Reply::ok()
            }
        }
    }

    fn remove(&self, window: u32) {
        let mut windows = self.shared.windows.lock().unwrap();
        if windows.get(&window).is_some_and(|w| w.conn == self.conn) {
            windows.remove(&window);
        }
    }
}

impl Service for A11yService {
    fn handle(&mut self, ty: u16, body: &[u8], mut fds: Vec<OwnedFd>) -> Reply {
        let req = match proto::decode(ty, body) {
            Ok(r) => r,
            Err(e) => return Reply::err(e.status()),
        };
        let want_fd = matches!(req, Request::Tree { .. });
        if fds.len() != want_fd as usize {
            return Reply::err(-EPROTO);
        }
        match req {
            Request::Start => {
                self.shared.watch();
                if self.subscription.is_none() {
                    self.subscription = Some(self.shared.subscribers.add(self.events.clone()));
                }
                let on = self.shared.active.load(Ordering::Acquire);
                Reply::with_body(proto::active_event(on))
            }
            Request::Tree { window, len } => self.tree(window, len, &fds.pop().unwrap()),
            Request::Remove { window } => {
                self.remove(window);
                Reply::ok()
            }
        }
    }
}

impl Drop for A11yService {
    fn drop(&mut self) {
        if let Some(k) = self.subscription {
            self.shared.subscribers.remove(k);
        }
        self.shared
            .windows
            .lock()
            .unwrap()
            .retain(|_, w| w.conn != self.conn);
        crate::info!("{}: a11y connection closed", self.label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_start_stays_inactive_without_watching_the_desktop() {
        let shared = Shared::new(false);
        let (socket, _peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let mut service = A11yService::new(
            shared.clone(),
            &Peer::test(),
            Events::new(Arc::new(socket.into())),
        );
        let reply = service.handle(proto::ty::START, &[], vec![]);
        assert_eq!(reply.status, 0);
        assert_eq!(reply.body, [0]);
        assert!(shared.watching.get().is_none());
    }
}
