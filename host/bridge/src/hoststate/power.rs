//! The battery and the power saver.
//!
//! Battery: UPower's `DisplayDevice` (the composite of every battery:
//! `IsPresent`, `State`, `Percentage`, `TimeToEmpty`, `TimeToFull`) and its
//! `OnBattery`, re-read on each `PropertiesChanged`. Without UPower, or
//! without a battery, the host is on AC and full ([`Battery::AC_ONLY`]).
//!
//! Power saver: the PowerProfileMonitor portal's `power-saver-enabled`.

use super::proto::Battery;
use super::{Topic, get_all, signals, signals_under};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use std::collections::HashMap;

const UPOWER: &str = "org.freedesktop.UPower";
const UPOWER_PATH: &str = "/org/freedesktop/UPower";
const DISPLAY_DEVICE: &str = "/org/freedesktop/UPower/devices/DisplayDevice";
const DEVICE_IFACE: &str = "org.freedesktop.UPower.Device";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PROFILE_MONITOR: &str = "org.freedesktop.portal.PowerProfileMonitor";

pub fn watch_battery(system: Option<Connection>, topic: &Topic<Battery>) {
    let Some(conn) = system else {
        return topic.set(Battery::AC_ONLY, Battery::encode);
    };
    // Subscribed before the first read, so no change slips between them.
    // OnBattery changes on the root object, the rest on the devices.
    let changes = signals_under(&conn, UPOWER_PATH, PROPERTIES, "PropertiesChanged");
    match read_battery(&conn) {
        Ok(b) => topic.set(b, Battery::encode),
        Err(e) => {
            crate::info!("hoststate: no UPower ({e}): AC power, no battery");
            return topic.set(Battery::AC_ONLY, Battery::encode);
        }
    }
    let Ok(changes) = changes else {
        return crate::warn!("hoststate: UPower signals unavailable");
    };
    for _ in changes {
        if let Ok(b) = read_battery(&conn) {
            topic.set(b, Battery::encode);
        }
    }
    crate::warn!("hoststate: UPower signal stream ended");
}

fn read_battery(conn: &Connection) -> zbus::Result<Battery> {
    let dev = get_all(conn, UPOWER, DISPLAY_DEVICE, DEVICE_IFACE)?;
    let root = get_all(conn, UPOWER, UPOWER_PATH, UPOWER)?;
    Ok(battery_from(&dev, &root))
}

/// UPower's properties as a [`Battery`]; missing or malformed values read as
/// unknown.
pub fn battery_from(
    dev: &HashMap<String, OwnedValue>,
    root: &HashMap<String, OwnedValue>,
) -> Battery {
    let b =
        |m: &HashMap<String, OwnedValue>, k: &str| m.get(k).and_then(|v| bool::try_from(v).ok());
    let on_battery = b(root, "OnBattery").unwrap_or(false);
    if !b(dev, "IsPresent").unwrap_or(false) {
        return Battery {
            ac: true,
            ..Battery::AC_ONLY
        };
    }
    let state = dev
        .get("State")
        .and_then(|v| u32::try_from(v).ok())
        .filter(|s| *s <= 6)
        .unwrap_or(0) as u8;
    let level = dev
        .get("Percentage")
        .and_then(|v| f64::try_from(v).ok())
        .filter(|p| p.is_finite())
        .map(|p| p.clamp(0.0, 100.0).round() as u8)
        .unwrap_or(100);
    let secs = |k: &str| {
        dev.get(k)
            .and_then(|v| i64::try_from(v).ok())
            .map(|s| s.clamp(0, u32::MAX as i64) as u32)
            .unwrap_or(0)
    };
    Battery {
        present: true,
        state,
        ac: !on_battery,
        level,
        time_to_empty: secs("TimeToEmpty"),
        time_to_full: secs("TimeToFull"),
    }
}

pub fn watch_saver(session: Option<Connection>, topic: &Topic<bool>, encode: fn(&bool) -> Vec<u8>) {
    let Some(conn) = session else { return };
    let changes = signals(&conn, PORTAL_PATH, PROPERTIES, "PropertiesChanged");
    match portal_saver(&conn) {
        Ok(on) => topic.set(on, encode),
        Err(e) => return crate::warn!("hoststate: PowerProfileMonitor: {e}"),
    }
    let Ok(changes) = changes else { return };
    for msg in changes {
        let Ok(msg) = msg else { continue };
        let body = msg.body();
        let Ok((iface, _, _)) =
            body.deserialize::<(String, HashMap<String, OwnedValue>, Vec<String>)>()
        else {
            continue;
        };
        if iface == PROFILE_MONITOR
            && let Ok(on) = portal_saver(&conn)
        {
            topic.set(on, encode);
        }
    }
    crate::warn!("hoststate: power saver: portal signal stream ended");
}

fn portal_saver(conn: &Connection) -> zbus::Result<bool> {
    let p = zbus::blocking::Proxy::new(conn, PORTAL, PORTAL_PATH, PROFILE_MONITOR)?;
    p.get_property::<bool>("power-saver-enabled")
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    fn map(kv: &[(&str, Value<'static>)]) -> HashMap<String, OwnedValue> {
        kv.iter()
            .map(|(k, v)| ((*k).to_owned(), v.try_to_owned().unwrap()))
            .collect()
    }

    #[test]
    fn upower() {
        let dev = map(&[
            ("IsPresent", Value::from(true)),
            ("State", Value::from(2u32)),
            ("Percentage", Value::from(41.6f64)),
            ("TimeToEmpty", Value::from(5400i64)),
            ("TimeToFull", Value::from(0i64)),
        ]);
        let b = battery_from(&dev, &map(&[("OnBattery", Value::from(true))]));
        assert_eq!(
            b,
            Battery {
                present: true,
                state: 2,
                ac: false,
                level: 42,
                time_to_empty: 5400,
                time_to_full: 0,
            }
        );
        // No battery: AC and full, whatever else UPower says.
        let none = map(&[
            ("IsPresent", Value::from(false)),
            ("Percentage", Value::from(0.0f64)),
        ]);
        assert_eq!(battery_from(&none, &HashMap::new()), Battery::AC_ONLY);
        // Out-of-range values read as unknown or clamp.
        let odd = map(&[
            ("IsPresent", Value::from(true)),
            ("State", Value::from(9u32)),
            ("Percentage", Value::from(140.0f64)),
            ("TimeToEmpty", Value::from(-5i64)),
        ]);
        let b = battery_from(&odd, &HashMap::new());
        assert_eq!((b.state, b.level, b.time_to_empty, b.ac), (0, 100, 0, true));
    }
}
