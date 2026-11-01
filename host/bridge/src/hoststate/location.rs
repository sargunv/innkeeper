//! Location through the XDG Location portal. The portal owns the desktop's
//! consent dialog and reaches GeoClue under its own identity.
//!
//! xdg-desktop-portal drives a single GeoClue client for all its Location
//! sessions. A second session gets no initial fix, each session's thresholds
//! overwrite the client's, and closing any session stops the client for all
//! the others. The bridge therefore keeps a single session ([`Hub`]) for every
//! client that asked, which in practice means the system app's providers. The
//! session is opened at the most precise accuracy asked for, without
//! thresholds, and closed when the last client stops.
//!
//! A denial (`Response` 1) holds for the bridge's lifetime. Later starts get
//! `DENIED` at once, so Android's apps see location as off for the session
//! while Android's own permission stays granted.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::message::Message;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

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
/// the desktop, in GNOME Privacy > Location or by a lockdown.
const SERVICES_DISABLED: &str = "Location services disabled";

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn token(kind: &str) -> String {
    format!(
        "innkeeper_{kind}_{}_{}",
        std::process::id(),
        NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
    )
}

/// Every client that wants fixes, served by a single desktop session.
pub struct Hub {
    inner: Mutex<Inner>,
}

struct Inner {
    members: Vec<(u64, Events, bool)>,
    run: Option<Run>,
    generation: u64,
    /// The running session's last fix, handed to clients that join it.
    last: Option<Fix>,
    /// The user said no. The bridge doesn't ask again for its lifetime.
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

struct Opened(Connection, OwnedObjectPath);

impl Opened {
    fn close(self) {
        let Self(c, session) = self;
        if let Ok(p) = Proxy::new(&c, PORTAL, session.as_str(), PORTAL_SESSION) {
            let _ = p.call::<_, _, ()>("Close", &());
        }
        let _ = c.close();
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
    pub fn new() -> Arc<Hub> {
        Arc::new(Hub {
            inner: Mutex::new(Inner {
                members: Vec::new(),
                run: None,
                generation: 0,
                last: None,
                denied: false,
            }),
        })
    }

    /// Adds or updates `client`. The running session serves it if that
    /// session is precise enough. Otherwise a new session replaces it.
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
        // The old session is closed after the swap so that its Close reaches
        // the portal before the new session's Start does anything.
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

    /// Called when the session is gone. Every member hears why and must
    /// start again. The user's answer counts even if its session was replaced
    /// or closed while the dialog was up. In that case it ends the current
    /// session too.
    fn fail(&self, generation: u64, st: u8, why: &str) {
        let run = {
            let mut h = self.inner.lock().unwrap();
            let sticky = st == status::DENIED || st == status::DISABLED;
            if !sticky && !h.run.as_ref().is_some_and(|r| r.generation == generation) {
                return;
            }
            if sticky {
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

    /// The session's thread. It opens a backend, then reads it until it ends
    /// or the hub moves on.
    fn run(self: Arc<Self>, generation: u64, precise: bool, opened: Arc<Mutex<Option<Opened>>>) {
        match Portal::open(precise) {
            Ok(p) => {
                if !self.adopt(
                    generation,
                    &opened,
                    Opened(p.conn.clone(), p.session.clone()),
                ) {
                    return;
                }
                p.run(&self, generation)
            }
            Err((st, why)) => self.fail(generation, st, &why),
        }
    }

    /// Hands the opened backend to the run. If the hub moved on meanwhile,
    /// the backend is closed here instead.
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
}

struct Portal {
    conn: Connection,
    messages: MessageIterator,
    session: OwnedObjectPath,
    request: OwnedObjectPath,
    request_token: String,
}

impl Portal {
    /// Connects and creates a session. Nothing is shown to the user yet. The
    /// connection is its own, so closing it ends this thread.
    fn open(precise: bool) -> Result<Portal, (u8, String)> {
        let failed = |e: zbus::Error| (classify(&e), e.to_string());
        let conn = Connection::session().map_err(failed)?;
        let proxy = Proxy::new(&conn, PORTAL, PORTAL_PATH, PORTAL_LOCATION).map_err(failed)?;
        // Subscribe before any call so that neither the Response nor an early
        // fix can be missed.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(PORTAL)
            .and_then(|b| b.path_namespace(PORTAL_PATH))
            .map(|b| b.build())
            .map_err(failed)?;
        let messages = MessageIterator::for_match_rule(rule, &conn, Some(64)).map_err(failed)?;
        // No distance or time threshold is set, because it would apply to the
        // portal's shared GeoClue client.
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
                .map_err(|e| (status::UNAVAILABLE, e.to_string()))?;
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
            // There is no parent window, so the dialog names the bridge's app
            // id.
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
                // GNOME shows its dialog only for the focused app. Otherwise it
                // answers 2 at once, as it does when GeoClue fails.
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
                        parse_fix(&v).map_or(Event::Other, Event::Fix)
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

/// Parses a Location portal dictionary. Unknown speed and heading are -1,
/// unknown altitude is -1 or -DBL_MAX, and `Timestamp` is `(seconds, µs)`.
pub fn parse_fix(v: &HashMap<String, OwnedValue>) -> Option<Fix> {
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
            .filter(|a| *a != -1.0 && *a > -1.0e300)
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
            | "org.freedesktop.DBus.Error.AccessDenied" => return status::DENIED,
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
        let f = parse_fix(&v).unwrap();
        assert_eq!((f.latitude, f.longitude), (52.52, 13.405));
        assert!(f.altitude.is_nan());
        assert_eq!((f.accuracy, f.speed, f.bearing), (25.0, -1.0, 90.0));
        assert_eq!(f.time_ms, 1_800_000_000_250);
        let mut bad = v.clone();
        bad.insert(
            "Latitude".into(),
            Value::from(120.0f64).try_to_owned().unwrap(),
        );
        assert!(parse_fix(&bad).is_none());
        bad.remove("Latitude");
        assert!(parse_fix(&bad).is_none());
    }
}
