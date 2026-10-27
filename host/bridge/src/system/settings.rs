//! Desktop settings for Android: dark mode, accent color, contrast, reduced
//! motion, the clock format and text scale (portal Settings), plus the time
//! zone (`/etc/localtime`) and the locale (the session environment).
//!
//! One watcher thread per bridge. Each change is sent as a full `SETTINGS`
//! event to every subscribed system-app connection.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use zbus::zvariant::{OwnedValue, Structure, Value};

use super::proto::{Settings, ev};
use crate::ipc::Subscribers;
use crate::ipc::server::Events;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const SETTINGS_IFACE: &str = "org.freedesktop.portal.Settings";
const APPEARANCE: &str = "org.freedesktop.appearance";
const GNOME_IFACE: &str = "org.gnome.desktop.interface";

pub struct SettingsHub {
    state: Mutex<Settings>,
    subs: Subscribers,
}

impl SettingsHub {
    /// Reads the current settings and starts watching them.
    pub fn start(dbus: Option<zbus::blocking::Connection>) -> Arc<SettingsHub> {
        let hub = Arc::new(SettingsHub {
            state: Mutex::new(Settings::default()),
            subs: Subscribers::default(),
        });
        hub.update(|s| {
            s.timezone = timezone();
            s.locales = locales();
        });
        if let Some(conn) = dbus {
            let h = hub.clone();
            spawn("settings-portal", move || h.watch_portal(conn));
        }
        let h = hub.clone();
        spawn("settings-tz", move || {
            loop {
                std::thread::sleep(Duration::from_secs(5));
                h.update(|s| s.timezone = timezone());
            }
        });
        hub
    }

    pub fn current(&self) -> Settings {
        self.state.lock().unwrap().clone()
    }

    /// Subscribes `e` and sends it the current settings.
    pub fn subscribe(&self, e: Events) -> u64 {
        let body = self.current().encode();
        let _ = e.try_send(ev::SETTINGS, &body, &[]);
        self.subs.add(e)
    }

    pub fn unsubscribe(&self, k: u64) {
        self.subs.remove(k);
    }

    fn update(&self, f: impl FnOnce(&mut Settings)) {
        let body = {
            let mut st = self.state.lock().unwrap();
            let mut next = st.clone();
            f(&mut next);
            if next == *st {
                return;
            }
            crate::info!("settings: {next:?}");
            *st = next;
            st.encode()
        };
        self.subs.broadcast(ev::SETTINGS, &body, &[], None);
    }

    fn watch_portal(&self, conn: zbus::blocking::Connection) {
        let proxy = match zbus::blocking::Proxy::new(&conn, PORTAL, PORTAL_PATH, SETTINGS_IFACE) {
            Ok(p) => p,
            Err(e) => return crate::warn!("settings: portal proxy: {e}"),
        };
        // Subscribe before the first read, so no change slips between them.
        let changes = proxy.receive_signal("SettingChanged");
        let all: zbus::Result<HashMap<String, HashMap<String, OwnedValue>>> =
            proxy.call("ReadAll", &(vec![APPEARANCE, GNOME_IFACE],));
        match all {
            Ok(all) => self.update(|s| {
                for (ns, kv) in &all {
                    for (k, v) in kv {
                        apply(s, ns, k, v);
                    }
                }
            }),
            Err(e) => crate::warn!("settings: portal ReadAll: {e}"),
        }
        let changes = match changes {
            Ok(c) => c,
            Err(e) => return crate::warn!("settings: SettingChanged: {e}"),
        };
        for msg in changes {
            let body = msg.body();
            let Ok((ns, key, value)) = body.deserialize::<(String, String, OwnedValue)>() else {
                continue;
            };
            self.update(|s| apply(s, &ns, &key, &value));
        }
        crate::warn!("settings: portal signal stream ended");
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        crate::warn!("spawn {name}: {e}");
    }
}

/// Applies one portal setting. Unknown keys and malformed values are ignored.
fn apply(s: &mut Settings, ns: &str, key: &str, v: &Value<'_>) {
    // Some portal versions wrap values in an extra variant.
    if let Value::Value(inner) = v {
        return apply(s, ns, key, inner);
    }
    match (ns, key) {
        (APPEARANCE, "color-scheme") => {
            if let Ok(n) = u32::try_from(v) {
                s.color_scheme = if n <= 2 { n as u8 } else { 0 };
            }
        }
        (APPEARANCE, "accent-color") => s.accent = accent(v),
        (APPEARANCE, "contrast") => {
            if let Ok(n) = u32::try_from(v) {
                s.contrast = (n == 1) as u8;
            }
        }
        (APPEARANCE, "reduced-motion") => {
            if let Ok(n) = u32::try_from(v) {
                s.reduced_motion = (n == 1) as u8;
            }
        }
        (GNOME_IFACE, "clock-format") => {
            if let Ok(f) = <&str>::try_from(v) {
                s.clock = match f {
                    "12h" => 1,
                    "24h" => 2,
                    _ => 0,
                };
            }
        }
        (GNOME_IFACE, "text-scaling-factor") => {
            if let Ok(f) = f64::try_from(v) {
                s.text_scale = text_scale(f);
            }
        }
        _ => {}
    }
}

/// `(ddd)` sRGB in [0, 1]; out-of-range components mean "unset" (portal spec).
fn accent(v: &Value<'_>) -> Option<[u8; 3]> {
    let st: &Structure = v.downcast_ref().ok()?;
    let f = st.fields();
    if f.len() != 3 {
        return None;
    }
    let mut rgb = [0u8; 3];
    for (o, c) in rgb.iter_mut().zip(f) {
        let c = f64::try_from(c).ok()?;
        if !(0.0..=1.0).contains(&c) {
            return None;
        }
        *o = (c * 255.0).round() as u8;
    }
    Some(rgb)
}

fn text_scale(f: f64) -> f32 {
    if f.is_finite() && (0.5..=3.0).contains(&f) {
        f as f32
    } else {
        0.0
    }
}

/// An IANA zone id: `Area/Location[/More]` or `UTC`, plain ASCII.
pub fn valid_tz(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && !t.starts_with('/')
        && !t.contains("..")
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b))
}

/// The host zone, from the `/etc/localtime` symlink (`.../zoneinfo/<id>`).
fn timezone() -> String {
    if let Some(tz) = std::env::var("TZ")
        .ok()
        .map(|t| t.trim_start_matches(':').to_owned())
        .filter(|t| valid_tz(t) && t.contains('/'))
    {
        return tz;
    }
    let Ok(target) = std::fs::read_link("/etc/localtime") else {
        return String::new();
    };
    let t = target.to_string_lossy();
    match t.rsplit_once("zoneinfo/") {
        Some((_, id)) if valid_tz(id) => id.to_owned(),
        _ => String::new(),
    }
}

/// The session's languages: `LANGUAGE` (a preference list), then `LC_ALL`,
/// `LC_MESSAGES`, `LANG`.
fn locales() -> String {
    let mut list: Vec<String> = Vec::new();
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            list.push(v);
            break;
        }
    }
    if let Ok(l) = std::env::var("LANGUAGE") {
        let mut pref: Vec<String> = l.split(':').map(str::to_owned).collect();
        pref.append(&mut list);
        list = pref;
    }
    bcp47_list(list.into_iter().filter_map(|p| posix_to_bcp47(&p))).unwrap_or_default()
}

/// `de_DE.UTF-8@euro` -> `de-DE`, `sr_RS@latin` -> `sr-Latn-RS`; `C` and
/// `POSIX` have no language.
pub fn posix_to_bcp47(p: &str) -> Option<String> {
    let (base, modifier) = match p.split_once('@') {
        Some((b, m)) => (b, Some(m)),
        None => (p, None),
    };
    let base = base.split('.').next()?;
    if base.is_empty() || base == "C" || base == "POSIX" {
        return None;
    }
    let (lang, region) = match base.split_once('_') {
        Some((l, r)) => (l, Some(r)),
        None => (base, None),
    };
    let script = match modifier {
        Some("latin") => Some("Latn"),
        Some("cyrillic") => Some("Cyrl"),
        _ => None,
    };
    let tag: Vec<&str> = [Some(lang), script, region].into_iter().flatten().collect();
    Some(tag.join("-"))
}

/// Joins well-formed tags (letters, digits and `-`; deduplicated) with
/// commas, within the protocol's name limit.
fn bcp47_list(tags: impl Iterator<Item = String>) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for t in tags {
        let t = t.trim().to_owned();
        let ok = !t.is_empty()
            && t.len() <= 35
            && t.as_bytes()[0].is_ascii_alphabetic()
            && t.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if ok && !out.contains(&t) && out.iter().map(|s| s.len() + 1).sum::<usize>() + t.len() <= 96
        {
            out.push(t);
        }
    }
    (!out.is_empty()).then(|| out.join(","))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_locales() {
        assert_eq!(posix_to_bcp47("de_DE.UTF-8@euro").as_deref(), Some("de-DE"));
        assert_eq!(posix_to_bcp47("sr_RS@latin").as_deref(), Some("sr-Latn-RS"));
        assert_eq!(posix_to_bcp47("fr").as_deref(), Some("fr"));
        assert_eq!(posix_to_bcp47("C.UTF-8"), None);
        assert_eq!(
            bcp47_list(
                ["en-US", "bad tag", "en-US", "fr"]
                    .map(String::from)
                    .into_iter()
            )
            .as_deref(),
            Some("en-US,fr")
        );
    }

    #[test]
    fn portal_values() {
        let mut s = Settings::default();
        apply(&mut s, APPEARANCE, "color-scheme", &Value::from(1u32));
        apply(
            &mut s,
            APPEARANCE,
            "accent-color",
            &Value::from((0.239216f64, 0.682353f64, 0.913725f64)),
        );
        apply(&mut s, GNOME_IFACE, "clock-format", &Value::from("24h"));
        apply(
            &mut s,
            GNOME_IFACE,
            "text-scaling-factor",
            &Value::from(1.25f64),
        );
        assert_eq!(s.color_scheme, 1);
        assert_eq!(s.accent, Some([0x3d, 0xae, 0xe9]));
        assert_eq!(s.clock, 2);
        assert_eq!(s.text_scale, 1.25);
        apply(
            &mut s,
            APPEARANCE,
            "accent-color",
            &Value::from((2.0f64, 0.5f64, 0.5f64)),
        );
        assert_eq!(s.accent, None);
        apply(&mut s, APPEARANCE, "color-scheme", &Value::from(7u32));
        assert_eq!(s.color_scheme, 0);
        apply(
            &mut s,
            GNOME_IFACE,
            "text-scaling-factor",
            &Value::from(40.0f64),
        );
        assert_eq!(s.text_scale, 0.0);
    }

    #[test]
    fn zones() {
        assert!(valid_tz("America/Los_Angeles"));
        assert!(valid_tz("Etc/GMT+8"));
        assert!(!valid_tz("../etc/passwd"));
        assert!(!valid_tz("/etc/x"));
        assert!(!valid_tz("A B"));
    }
}
