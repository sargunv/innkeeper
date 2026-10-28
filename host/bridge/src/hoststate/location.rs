//! Location: the XDG Location portal, falling back to GeoClue directly.
//! Ported from Nucleus's `location/src/main/native/src/linux.rs`.
//!
//! The portal is preferred: it owns the desktop's consent dialog and reaches
//! GeoClue under its own identity. GeoClue is used directly only when the
//! portal is missing and the bridge isn't sandboxed, and never after the
//! portal said no, which would route around the user's decision.
//!
//! xdg-desktop-portal drives one GeoClue client for all its Location
//! sessions: a second session gets no initial fix, each session's thresholds
//! overwrite the client's, and closing any session stops the client under
//! all the others. So the bridge keeps one session ([`Hub`]) for every
//! client that asked (in practice the system app's providers), opened at the
//! most precise accuracy asked for, without thresholds, and closed when the
//! last client stops.
//!
//! A denial (`Response` 1) holds for the bridge's lifetime: later starts get
//! `DENIED` at once, so Android's apps see location as off for the session
//! while Android's own permission stays granted (#19).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Message;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::is_missing;
use super::proto::{Fix, ev, status};
use crate::ipc::server::Events;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_LOCATION: &str = "org.freedesktop.portal.Location";
const PORTAL_REQUEST: &str = "org.freedesktop.portal.Request";
const PORTAL_SESSION: &str = "org.freedesktop.portal.Session";
const ACCURACY_CITY: u32 = 2;
const ACCURACY_EXACT: u32 = 5;
/// xdg-desktop-portal's `NotAllowed` message when location is turned off on
/// the desktop (GNOME Privacy > Location, or a lockdown).
const SERVICES_DISABLED: &str = "Location services disabled";

const GEOCLUE: &str = "org.freedesktop.GeoClue2";
const GEOCLUE_MANAGER_PATH: &str = "/org/freedesktop/GeoClue2/Manager";
const GEOCLUE_MANAGER: &str = "org.freedesktop.GeoClue2.Manager";
const GEOCLUE_CLIENT: &str = "org.freedesktop.GeoClue2.Client";
const GEOCLUE_LOCATION: &str = "org.freedesktop.GeoClue2.Location";
const GEOCLUE_ACCURACY_CITY: u32 = 4;
const GEOCLUE_ACCURACY_EXACT: u32 = 8;
/// What GeoClue's agent shows and matches against its allow-list.
const DESKTOP_ID: &str = "dev.linuxapk.Manager";

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn token(kind: &str) -> String {
    format!(
        "linuxapk_{kind}_{}_{}",
        std::process::id(),
        NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
    )
}

/// Every client that wants fixes, served by one desktop session.
pub struct Hub {
    inner: Mutex<Inner>,
    /// The bridge's buses, only to probe for GeoClue.
    system: Option<Connection>,
}

struct Inner {
    members: Vec<(u64, Events, bool)>,
    run: Option<Run>,
    generation: u64,
    /// The running session's last fix, handed to clients that join it.
    last: Option<Fix>,
    /// The user said no; never ask again in this bridge.
    denied: bool,
}

/// The session the hub runs. `opened` is filled by its thread once the
/// backend is up, so closing can end the thread's connection.
struct Run {
    generation: u64,
    precise: bool,
    started: bool,
    opened: Arc<Mutex<Option<Opened>>>,
}

enum Opened {
    Portal(Connection, OwnedObjectPath),
    GeoClue(Connection, OwnedObjectPath),
}

impl Opened {
    fn close(self) {
        match self {
            Opened::Portal(c, session) => {
                if let Ok(p) = Proxy::new(&c, PORTAL, session.as_str(), PORTAL_SESSION) {
                    let _ = p.call::<_, _, ()>("Close", &());
                }
                let _ = c.close();
            }
            Opened::GeoClue(c, client) => {
                if let Ok(p) = Proxy::new(&c, GEOCLUE, client.as_str(), GEOCLUE_CLIENT) {
                    let _ = p.call::<_, _, ()>("Stop", &());
                }
                let _ = c.close();
            }
        }
    }
}

impl Run {
    fn close(self) {
        if let Some(o) = self.opened.lock().unwrap().take() {
            o.close();
        }
    }
}

fn send(e: &Events, ty: u16, body: &[u8]) {
    let _ = e.try_send(ty, body, &[]);
}

impl Hub {
    pub fn new(_session: Option<Connection>, system: Option<Connection>) -> Arc<Hub> {
        Arc::new(Hub {
            inner: Mutex::new(Inner {
                members: Vec::new(),
                run: None,
                generation: 0,
                last: None,
                denied: false,
            }),
            system,
        })
    }

    /// Adds (or updates) `client`. It is served by the running session if
    /// that is precise enough, else by a new one that replaces it.
    pub fn join(self: &Arc<Self>, client: u64, events: Events, precise: bool) {
        let mut replaced = None;
        {
            let mut h = self.inner.lock().unwrap();
            if h.denied {
                send(&events, ev::LOCATION_STATUS, &[status::DENIED, 0, 0, 0]);
                return;
            }
            h.members.retain(|m| m.0 != client);
            h.members.push((client, events.clone(), precise));
            let want = h.members.iter().any(|m| m.2);
            match &h.run {
                Some(r) if r.precise || !want => {
                    if r.started {
                        send(&events, ev::LOCATION_STATUS, &[status::RUNNING, 0, 0, 0]);
                        if let Some(f) = h.last {
                            send(&events, ev::LOCATION, &f.encode());
                        }
                    }
                }
                _ => {
                    h.generation += 1;
                    h.last = None;
                    let generation = h.generation;
                    let opened = Arc::new(Mutex::new(None));
                    replaced = h.run.replace(Run {
                        generation,
                        precise: want,
                        started: false,
                        opened: opened.clone(),
                    });
                    let hub = self.clone();
                    super::spawn("hoststate-location", move || {
                        hub.run(generation, want, opened)
                    });
                }
            }
        }
        // Closed after the swap, so its Stop reaches GeoClue before the new
        // session's Start does anything.
        if let Some(r) = replaced {
            r.close();
        }
    }

    pub fn leave(&self, client: u64) {
        let idle = {
            let mut h = self.inner.lock().unwrap();
            h.members.retain(|m| m.0 != client);
            if h.members.is_empty() {
                h.last = None;
                h.run.take()
            } else {
                None
            }
        };
        if let Some(r) = idle {
            crate::info!("hoststate: location: idle, session closed");
            r.close();
        }
    }

    fn runs(&self, generation: u64) -> bool {
        self.inner
            .lock()
            .unwrap()
            .run
            .as_ref()
            .is_some_and(|r| r.generation == generation)
    }

    fn started(&self, generation: u64) {
        let mut h = self.inner.lock().unwrap();
        let Some(r) = h.run.as_mut().filter(|r| r.generation == generation) else {
            return;
        };
        r.started = true;
        crate::info!("hoststate: location: allowed, running");
        for m in &h.members {
            send(&m.1, ev::LOCATION_STATUS, &[status::RUNNING, 0, 0, 0]);
        }
    }

    fn fix(&self, generation: u64, f: Fix) {
        let mut h = self.inner.lock().unwrap();
        if !h.run.as_ref().is_some_and(|r| r.generation == generation) {
            return;
        }
        crate::debug!("hoststate: location fix {f:?}");
        h.last = Some(f);
        let body = f.encode();
        for m in &h.members {
            send(&m.1, ev::LOCATION, &body);
        }
    }

    /// The session is gone: every member hears why and must start again.
    fn fail(&self, generation: u64, st: u8, why: &str) {
        let run = {
            let mut h = self.inner.lock().unwrap();
            if !h.run.as_ref().is_some_and(|r| r.generation == generation) {
                return;
            }
            if st == status::DENIED {
                h.denied = true;
            }
            crate::info!("hoststate: location: {why} (status {st})");
            for m in std::mem::take(&mut h.members) {
                send(&m.1, ev::LOCATION_STATUS, &[st, 0, 0, 0]);
            }
            h.last = None;
            h.run.take()
        };
        if let Some(r) = run {
            r.close();
        }
    }

    /// The session's thread: opens a backend, then reads it until it ends
    /// or the hub moves on.
    fn run(self: Arc<Self>, generation: u64, precise: bool, opened: Arc<Mutex<Option<Opened>>>) {
        match Portal::open(precise) {
            Ok(p) => {
                if !self.adopt(
                    generation,
                    &opened,
                    Opened::Portal(p.conn.clone(), p.session.clone()),
                ) {
                    return;
                }
                p.run(&self, generation)
            }
            Err(PortalError::Missing(why)) if !sandboxed() && self.geoclue_present() => {
                crate::info!("hoststate: location: no portal ({why}); GeoClue directly");
                match GeoClue::open(precise) {
                    Ok(g) => {
                        if !self.adopt(
                            generation,
                            &opened,
                            Opened::GeoClue(g.conn.clone(), g.client.clone()),
                        ) {
                            return;
                        }
                        g.run(&self, generation)
                    }
                    Err((st, why)) => self.fail(generation, st, &why),
                }
            }
            Err(PortalError::Missing(why)) => self.fail(
                generation,
                status::UNAVAILABLE,
                &format!("no backend ({why})"),
            ),
            Err(PortalError::Failed(st, why)) => self.fail(generation, st, &why),
        }
    }

    /// Hands the opened backend to the run, unless the hub moved on
    /// meanwhile (then it is closed here).
    fn adopt(&self, generation: u64, slot: &Arc<Mutex<Option<Opened>>>, o: Opened) -> bool {
        let h = self.inner.lock().unwrap();
        if !h.run.as_ref().is_some_and(|r| r.generation == generation) {
            drop(h);
            o.close();
            return false;
        }
        *slot.lock().unwrap() = Some(o);
        true
    }

    fn geoclue_present(&self) -> bool {
        let Some(c) = &self.system else { return false };
        let Ok(p) = zbus::blocking::fdo::DBusProxy::new(c) else {
            return false;
        };
        let name = zbus::names::BusName::try_from(GEOCLUE).expect("valid name");
        p.name_has_owner(name.clone()).unwrap_or(false)
            || p.list_activatable_names()
                .is_ok_and(|n| n.iter().any(|n| n.as_str() == GEOCLUE))
    }
}

/// A sandboxed bridge must stay on the portal: a system-bus call to GeoClue
/// would bypass its permission decision.
fn sandboxed() -> bool {
    std::path::Path::new("/.flatpak-info").exists()
        || std::env::var_os("FLATPAK_ID").is_some()
        || std::env::var_os("SNAP").is_some()
}

enum PortalError {
    /// No portal, or one without the Location interface: GeoClue may be
    /// tried instead.
    Missing(String),
    Failed(u8, String),
}

struct Portal {
    conn: Connection,
    messages: MessageIterator,
    session: OwnedObjectPath,
    request: OwnedObjectPath,
    request_token: String,
}

impl Portal {
    /// Connects (a connection of its own, so closing it ends this thread),
    /// checks the interface and creates a session; nothing the user sees.
    fn open(precise: bool) -> Result<Portal, PortalError> {
        let failed = |e: zbus::Error| PortalError::Failed(classify(&e), e.to_string());
        let conn = Connection::session().map_err(|e| PortalError::Missing(e.to_string()))?;
        let proxy = Proxy::new(&conn, PORTAL, PORTAL_PATH, PORTAL_LOCATION).map_err(failed)?;
        match proxy.get_property::<u32>("version") {
            Ok(v) if v >= 1 => {}
            Ok(v) => return Err(PortalError::Missing(format!("Location portal version {v}"))),
            Err(e) => {
                return Err(if is_missing(&e) {
                    PortalError::Missing(e.to_string())
                } else {
                    failed(e)
                });
            }
        }
        // Subscribed before any call, so neither the Response nor an early
        // fix can be missed.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(PORTAL)
            .and_then(|b| b.path_namespace(PORTAL_PATH))
            .map(|b| b.build())
            .map_err(failed)?;
        let messages = MessageIterator::for_match_rule(rule, &conn, Some(64)).map_err(failed)?;
        // No distance or time threshold: they would apply to the portal's
        // one GeoClue client.
        let mut opts: HashMap<&str, Value> = HashMap::new();
        opts.insert("session_handle_token", Value::from(token("session")));
        opts.insert(
            "accuracy",
            Value::from(if precise {
                ACCURACY_EXACT
            } else {
                ACCURACY_CITY
            }),
        );
        let session: OwnedObjectPath = proxy.call("CreateSession", &opts).map_err(failed)?;
        let request_token = token("request");
        let sender = conn
            .unique_name()
            .map(|n| n.trim_start_matches(':').replace('.', "_"))
            .unwrap_or_default();
        let request =
            OwnedObjectPath::try_from(format!("{PORTAL_PATH}/request/{sender}/{request_token}"))
                .map_err(|e| PortalError::Failed(status::UNAVAILABLE, e.to_string()))?;
        Ok(Portal {
            conn,
            messages,
            session,
            request,
            request_token,
        })
    }

    fn run(self, hub: &Hub, generation: u64) {
        let start = || -> zbus::Result<()> {
            let p = Proxy::new(&self.conn, PORTAL, PORTAL_PATH, PORTAL_LOCATION)?;
            let mut opts: HashMap<&str, Value> = HashMap::new();
            opts.insert("handle_token", Value::from(self.request_token.as_str()));
            // No parent window: the dialog names the bridge's app id.
            let _: OwnedObjectPath = p.call("Start", &(&self.session, "", opts))?;
            Ok(())
        };
        if let Err(e) = start() {
            return hub.fail(generation, classify(&e), &e.to_string());
        }
        for msg in self.messages.clone() {
            if !hub.runs(generation) {
                return;
            }
            let Ok(msg) = msg else { continue };
            match self.handle(&msg) {
                Event::Response(0) => hub.started(generation),
                Event::Response(1) => {
                    return hub.fail(generation, status::DENIED, "the user denied location");
                }
                // GNOME shows its dialog only for the focused app, and
                // answers 2 at once otherwise, as when GeoClue fails.
                Event::Response(c) => {
                    return hub.fail(
                        generation,
                        status::UNAVAILABLE,
                        &format!("the portal ended the request without an answer ({c})"),
                    );
                }
                Event::Fix(f) => hub.fix(generation, f),
                Event::Closed => {
                    return hub.fail(
                        generation,
                        status::UNAVAILABLE,
                        "the portal closed the session",
                    );
                }
                Event::Other => {}
            }
        }
        hub.fail(
            generation,
            status::UNAVAILABLE,
            "the portal connection closed",
        );
    }

    fn handle(&self, msg: &Message) -> Event {
        let h = msg.header();
        let iface = h.interface().map(|i| i.as_str().to_owned());
        let member = h.member().map(|m| m.as_str().to_owned());
        let path = h.path().map(|p| p.as_str().to_owned());
        match (iface.as_deref(), member.as_deref()) {
            (Some(PORTAL_REQUEST), Some("Response"))
                if path.as_deref() == Some(self.request.as_str()) =>
            {
                Event::Response(
                    msg.body()
                        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
                        .map(|(c, _)| c)
                        .unwrap_or(u32::MAX),
                )
            }
            (Some(PORTAL_LOCATION), Some("LocationUpdated")) => {
                match msg
                    .body()
                    .deserialize::<(OwnedObjectPath, HashMap<String, OwnedValue>)>()
                {
                    Ok((s, v)) if s == self.session => {
                        parse_fix(&v, -1.0).map_or(Event::Other, Event::Fix)
                    }
                    _ => Event::Other,
                }
            }
            (Some(PORTAL_SESSION), Some("Closed"))
                if path.as_deref() == Some(self.session.as_str()) =>
            {
                Event::Closed
            }
            _ => Event::Other,
        }
    }
}

enum Event {
    Response(u32),
    Fix(Fix),
    Closed,
    Other,
}

struct GeoClue {
    conn: Connection,
    client: OwnedObjectPath,
    messages: MessageIterator,
}

impl GeoClue {
    fn open(precise: bool) -> Result<GeoClue, (u8, String)> {
        let err = |e: zbus::Error| (classify(&e), e.to_string());
        let conn = Connection::system().map_err(|e| (status::UNAVAILABLE, e.to_string()))?;
        let m = Proxy::new(&conn, GEOCLUE, GEOCLUE_MANAGER_PATH, GEOCLUE_MANAGER).map_err(err)?;
        let client: OwnedObjectPath = m.call("GetClient", &()).map_err(err)?;
        let c = Proxy::new(&conn, GEOCLUE, client.to_string(), GEOCLUE_CLIENT).map_err(err)?;
        let set = |k: &str, v: Value<'_>| c.set_property(k, v).map_err(|e| err(e.into()));
        set("DesktopId", Value::from(DESKTOP_ID))?;
        set(
            "RequestedAccuracyLevel",
            Value::from(if precise {
                GEOCLUE_ACCURACY_EXACT
            } else {
                GEOCLUE_ACCURACY_CITY
            }),
        )?;
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(GEOCLUE)
            .and_then(|b| b.path(client.as_str()))
            .and_then(|b| b.interface(GEOCLUE_CLIENT))
            .and_then(|b| b.member("LocationUpdated"))
            .map(|b| b.build())
            .map_err(err)?;
        let messages = MessageIterator::for_match_rule(rule, &conn, Some(64)).map_err(err)?;
        drop(c);
        Ok(GeoClue {
            conn,
            client,
            messages,
        })
    }

    /// GeoClue asks its agent (and through it the user) before answering
    /// `Start`, so a successful `Start` is the consent.
    fn run(self, hub: &Hub, generation: u64) {
        let start = Proxy::new(&self.conn, GEOCLUE, self.client.as_str(), GEOCLUE_CLIENT)
            .and_then(|p| p.call::<_, _, ()>("Start", &()));
        if let Err(e) = start {
            return hub.fail(generation, classify(&e), &e.to_string());
        }
        hub.started(generation);
        for msg in self.messages.clone() {
            if !hub.runs(generation) {
                return;
            }
            let Ok(msg) = msg else { continue };
            let Ok((_, new)) = msg
                .body()
                .deserialize::<(OwnedObjectPath, OwnedObjectPath)>()
            else {
                continue;
            };
            let Ok(v) = super::get_all(&self.conn, GEOCLUE, new.as_str(), GEOCLUE_LOCATION) else {
                continue;
            };
            // GeoClue marks an unknown altitude with -DBL_MAX.
            if let Some(f) = parse_fix(&v, f64::MIN) {
                hub.fix(generation, f);
            }
        }
        hub.fail(
            generation,
            status::UNAVAILABLE,
            "the GeoClue connection closed",
        );
    }
}

/// A portal or GeoClue location dictionary. Both use the same keys: unknown
/// speed and heading are -1, an unknown altitude is `unknown_altitude` (or
/// -DBL_MAX), `Timestamp` is `(seconds, µs)`.
pub fn parse_fix(v: &HashMap<String, OwnedValue>, unknown_altitude: f64) -> Option<Fix> {
    let num = |k: &str| {
        v.get(k)
            .and_then(|x| f64::try_from(x).ok())
            .filter(|x| x.is_finite())
    };
    let opt = |k: &str, max: f64| {
        num(k)
            .filter(|x| (0.0..max).contains(x))
            .map_or(-1.0, |x| x as f32)
    };
    let time_ms = v
        .get("Timestamp")
        .and_then(|t| t.try_clone().ok())
        .and_then(|t| <(u64, u64)>::try_from(t).ok())
        .map(|(s, us)| s.saturating_mul(1000).saturating_add(us / 1000))
        .unwrap_or_else(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64)
        });
    let f = Fix {
        latitude: num("Latitude")?,
        longitude: num("Longitude")?,
        altitude: num("Altitude")
            .filter(|a| *a != unknown_altitude && *a > -1.0e300)
            .unwrap_or(f64::NAN),
        accuracy: opt("Accuracy", 1e7),
        speed: opt("Speed", 1e4),
        bearing: opt("Heading", 360.0),
        time_ms,
    };
    f.valid().then_some(f)
}

/// The `LOCATION_STATUS` a failed call ends with.
fn classify(e: &zbus::Error) -> u8 {
    if let zbus::Error::MethodError(name, msg, _) = e {
        let disabled = msg
            .as_deref()
            .is_some_and(|m| m.contains(SERVICES_DISABLED));
        match name.as_str() {
            "org.freedesktop.portal.Error.NotAllowed" if disabled => return status::DISABLED,
            "org.freedesktop.portal.Error.NotAllowed"
            | "org.freedesktop.portal.Error.Cancelled"
            | "org.freedesktop.DBus.Error.AccessDenied"
            | "org.freedesktop.GeoClue2.Error.AccessDenied"
            | "org.freedesktop.GeoClue2.Error.NotAuthorized" => return status::DENIED,
            _ => {}
        }
    }
    status::UNAVAILABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixes() {
        let v: HashMap<String, OwnedValue> = [
            ("Latitude", Value::from(52.52f64)),
            ("Longitude", Value::from(13.405f64)),
            ("Accuracy", Value::from(25.0f64)),
            ("Altitude", Value::from(-f64::MAX)),
            ("Speed", Value::from(-1.0f64)),
            ("Heading", Value::from(90.0f64)),
            ("Timestamp", Value::from((1_800_000_000u64, 250_000u64))),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.try_to_owned().unwrap()))
        .collect();
        let f = parse_fix(&v, -1.0).unwrap();
        assert_eq!((f.latitude, f.longitude), (52.52, 13.405));
        assert!(f.altitude.is_nan());
        assert_eq!((f.accuracy, f.speed, f.bearing), (25.0, -1.0, 90.0));
        assert_eq!(f.time_ms, 1_800_000_000_250);
        let mut bad = v.clone();
        bad.insert(
            "Latitude".into(),
            Value::from(120.0f64).try_to_owned().unwrap(),
        );
        assert!(parse_fix(&bad, -1.0).is_none());
        bad.remove("Latitude");
        assert!(parse_fix(&bad, -1.0).is_none());
    }
}
