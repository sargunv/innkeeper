//! The host state service (id 8). It gives Android the desktop's battery,
//! power saver, network and location. The messages are in [`proto`]. Each
//! kind of state is watched by a bridge-wide thread, which starts the first
//! time a client asks for it and sends updates to every connection that
//! subscribed.
//!
//! - `power`: UPower's `DisplayDevice` (the Health HAL's battery) and the
//!   power saver (the PowerProfileMonitor portal)
//! - `network`: the NetworkMonitor portal
//! - `location`: a shared Location portal session
//!
//! Battery, power saver and network are read-only state the desktop shows
//! any app. Test sessions disable all session portals (`allow_portals`),
//! including Location, which can ask the user for consent.

pub mod location;
pub mod network;
pub mod power;
pub mod proto;

use std::os::fd::OwnedFd;
use std::sync::{Arc, Mutex, Once, OnceLock};

use crate::ipc::Subscribers;
use crate::ipc::server::{Events, Peer, Reply, Service};
use crate::ipc::wire::errno::*;
use proto::{Battery, Network, Request, ev, feature};

/// One kind of state, with its last value and the connections that want it.
/// Identical updates are dropped, because portals can repeat themselves.
pub struct Topic<T> {
    what: &'static str,
    event: u16,
    state: Mutex<Option<T>>,
    subs: Subscribers,
    started: Once,
}

impl<T: Clone + PartialEq + std::fmt::Debug> Topic<T> {
    fn new(what: &'static str, event: u16) -> Topic<T> {
        Topic {
            what,
            event,
            state: Mutex::new(None),
            subs: Subscribers::default(),
            started: Once::new(),
        }
    }

    /// Subscribes `e` and sends it the current value, if known. `start` runs
    /// once per bridge, for the first subscriber.
    fn subscribe(&self, e: Events, encode: fn(&T) -> Vec<u8>, start: impl FnOnce()) -> u64 {
        self.started.call_once(start);
        let k = self.subs.add(e.clone());
        if let Some(v) = self.state.lock().unwrap().as_ref() {
            let _ = e.try_send(self.event, &encode(v), &[]);
        }
        k
    }

    pub fn set(&self, v: T, encode: fn(&T) -> Vec<u8>) {
        let body = {
            let mut st = self.state.lock().unwrap();
            if st.as_ref() == Some(&v) {
                return;
            }
            crate::info!("hoststate: {}: {v:?}", self.what);
            let body = encode(&v);
            *st = Some(v);
            body
        };
        self.subs.broadcast(self.event, &body, &[], None);
    }

    pub fn get(&self) -> Option<T> {
        self.state.lock().unwrap().clone()
    }
}

/// Bridge-wide state, created once in `main.rs`.
pub struct HostState {
    allow_portals: bool,
    system: OnceLock<Option<zbus::blocking::Connection>>,
    session: OnceLock<Option<zbus::blocking::Connection>>,
    pub battery: Topic<Battery>,
    pub saver: Topic<bool>,
    pub network: Topic<Network>,
    location: OnceLock<Arc<location::Hub>>,
    next_client: std::sync::atomic::AtomicU64,
}

fn encode_saver(on: &bool) -> Vec<u8> {
    proto::encode_u8(*on as u8)
}

impl HostState {
    /// `allow_portals` says whether desktop portals may be reached. A test
    /// session reaches none, not even for read-only power and network
    /// monitoring.
    pub fn new(allow_portals: bool) -> Arc<HostState> {
        Arc::new(HostState {
            allow_portals,
            system: OnceLock::new(),
            session: OnceLock::new(),
            battery: Topic::new("battery", ev::BATTERY),
            saver: Topic::new("power saver", ev::POWER_SAVER),
            network: Topic::new("network", ev::NETWORK),
            location: OnceLock::new(),
            next_client: Default::default(),
        })
    }

    fn system(&self) -> Option<zbus::blocking::Connection> {
        self.system
            .get_or_init(|| {
                zbus::blocking::Connection::system()
                    .map_err(|e| crate::warn!("hoststate: no system bus: {e}"))
                    .ok()
            })
            .clone()
    }

    fn session(&self) -> Option<zbus::blocking::Connection> {
        if !self.allow_portals {
            return None;
        }
        self.session
            .get_or_init(|| {
                zbus::blocking::Connection::session()
                    .map_err(|e| crate::warn!("hoststate: no session bus: {e}"))
                    .ok()
            })
            .clone()
    }

    fn location(&self) -> &Arc<location::Hub> {
        self.location.get_or_init(location::Hub::new)
    }
}

/// Spawns a named watcher thread.
fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        crate::warn!("spawn {name}: {e}");
    }
}

pub struct HostStateService {
    shared: Arc<HostState>,
    events: Events,
    label: String,
    client: u64,
    subs: Vec<(&'static str, u64)>,
    location: bool,
}

impl HostStateService {
    pub fn new(shared: Arc<HostState>, peer: &Peer, events: Events) -> HostStateService {
        let client = shared
            .next_client
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            + 1;
        HostStateService {
            shared,
            events,
            label: peer.label.clone(),
            client,
            subs: Vec::new(),
            location: false,
        }
    }

    fn start(&mut self, want: u32) -> Reply {
        let s = self.shared.clone();
        let mut have = 0;
        let has = |subs: &[(&str, u64)], w: &str| subs.iter().any(|(k, _)| *k == w);
        if want & feature::BATTERY != 0 && !has(&self.subs, "battery") {
            let st = s.clone();
            let k = s
                .battery
                .subscribe(self.events.clone(), Battery::encode, || {
                    spawn("hoststate-battery", move || {
                        power::watch_battery(st.system(), &st.battery)
                    })
                });
            self.subs.push(("battery", k));
        }
        if want & feature::POWER_SAVER != 0 && !has(&self.subs, "saver") {
            let st = s.clone();
            let k = s.saver.subscribe(self.events.clone(), encode_saver, || {
                spawn("hoststate-saver", move || {
                    power::watch_saver(st.session(), &st.saver, encode_saver)
                })
            });
            self.subs.push(("saver", k));
        }
        if want & feature::NETWORK != 0 && !has(&self.subs, "network") {
            let st = s.clone();
            let k = s
                .network
                .subscribe(self.events.clone(), Network::encode, || {
                    spawn("hoststate-network", move || {
                        network::watch(st.session(), &st.network)
                    })
                });
            self.subs.push(("network", k));
        }
        have |= feature::BATTERY | feature::POWER_SAVER | feature::NETWORK;
        if self.shared.allow_portals {
            have |= feature::LOCATION;
        }
        crate::info!(
            "hoststate: {}: started (wants {want:#x}, has {have:#x})",
            self.label
        );
        let mut w = crate::ipc::wire::Writer::default();
        w.u32(have & want);
        Reply::with_body(w.into_vec())
    }
}

impl Service for HostStateService {
    fn handle(&mut self, t: u16, body: &[u8], fds: Vec<OwnedFd>) -> Reply {
        let req = match proto::decode(t, body) {
            Ok(r) => r,
            Err(e) => return Reply::err(e.status()),
        };
        if !fds.is_empty() {
            return Reply::err(-EPROTO);
        }
        match req {
            Request::Start { features } => self.start(features),
            Request::LocationStart { precise } => {
                if !self.shared.allow_portals {
                    return Reply::err(-ENOSYS);
                }
                crate::info!(
                    "hoststate: {}: location start (precise {precise})",
                    self.label
                );
                self.location = true;
                self.shared
                    .location()
                    .join(self.client, self.events.clone(), precise);
                Reply::ok()
            }
            Request::LocationStop => {
                if self.location {
                    crate::info!("hoststate: {}: location stop", self.label);
                    self.location = false;
                    self.shared.location().leave(self.client);
                }
                Reply::ok()
            }
        }
    }
}

impl Drop for HostStateService {
    fn drop(&mut self) {
        for (what, k) in &self.subs {
            match *what {
                "battery" => self.shared.battery.subs.remove(*k),
                "saver" => self.shared.saver.subs.remove(*k),
                _ => self.shared.network.subs.remove(*k),
            }
        }
        if self.location {
            self.shared.location().leave(self.client);
        }
        crate::info!("hoststate: {}: closed", self.label);
    }
}

/// Reads one property dictionary (`org.freedesktop.DBus.Properties.GetAll`).
fn get_all(
    conn: &zbus::blocking::Connection,
    dest: &str,
    path: &str,
    iface: &str,
) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>> {
    let p = zbus::blocking::fdo::PropertiesProxy::builder(conn)
        .destination(dest.to_owned())?
        .path(path.to_owned())?
        .build()?;
    let iface = zbus::names::InterfaceName::try_from(iface.to_owned())?;
    Ok(p.get_all(iface)?)
}

/// Signals with this interface and member on `path`. The match accepts any
/// sender because services restart under new unique names.
fn signals(
    conn: &zbus::blocking::Connection,
    path: &str,
    iface: &str,
    member: &str,
) -> zbus::Result<zbus::blocking::MessageIterator> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path(path.to_owned())?
        .interface(iface.to_owned())?
        .member(member.to_owned())?
        .build();
    zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(64))
}

/// Like [`signals`], for every object under `path`.
fn signals_under(
    conn: &zbus::blocking::Connection,
    path: &str,
    iface: &str,
    member: &str,
) -> zbus::Result<zbus::blocking::MessageIterator> {
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .path_namespace(path.to_owned())?
        .interface(iface.to_owned())?
        .member(member.to_owned())?
        .build();
    zbus::blocking::MessageIterator::for_match_rule(rule, conn, Some(64))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_never_connects_to_portals() {
        let state = HostState::new(false);
        assert!(state.session().is_none());
        assert!(state.session.get().is_none());
    }
}
