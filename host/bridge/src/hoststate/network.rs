//! The host's network: available, metered, connectivity. The chain GLib's
//! GNetworkMonitor and Nucleus use, portal first:
//!
//! 1. the NetworkMonitor portal (session bus): `GetStatus` (v3), else
//!    `GetAvailable`, `GetMetered` and `GetConnectivity` (v2); re-read on its
//!    `changed` signal;
//! 2. NetworkManager (system bus): `State`, `Connectivity`, `Metered`;
//!    re-read on `PropertiesChanged`.
//!
//! Both repeat identical notifications; `Topic::set` drops those.

use std::collections::HashMap;

use super::proto::Network;
use super::{Topic, get_all, is_missing, signals};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const MONITOR: &str = "org.freedesktop.portal.NetworkMonitor";
const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

/// `Network` field values (proto.rs).
const UNKNOWN: u8 = 0;
const NO: u8 = 1;
const YES: u8 = 2;

pub fn watch(session: Option<Connection>, system: Option<Connection>, topic: &Topic<Network>) {
    if let Some(conn) = session {
        let changes = signals(&conn, PORTAL_PATH, MONITOR, "changed");
        match portal(&conn) {
            Ok(n) => {
                crate::info!("hoststate: network from the NetworkMonitor portal");
                topic.set(n, Network::encode);
                let Ok(changes) = changes else { return };
                for _ in changes {
                    if let Ok(n) = portal(&conn) {
                        topic.set(n, Network::encode);
                    }
                }
                return crate::warn!("hoststate: NetworkMonitor signal stream ended");
            }
            Err(e) if is_missing(&e) => {}
            // Refused (a sandbox without network access): no fallback.
            Err(e) => return crate::warn!("hoststate: NetworkMonitor: {e}"),
        }
    }
    let Some(conn) = system else { return };
    let changes = signals(&conn, NM_PATH, PROPERTIES, "PropertiesChanged");
    match get_all(&conn, NM, NM_PATH, NM) {
        Ok(p) => {
            crate::info!("hoststate: network from NetworkManager");
            topic.set(from_nm(&p), Network::encode);
        }
        Err(e) => return crate::info!("hoststate: no network monitor ({e}): network unknown"),
    }
    let Ok(changes) = changes else { return };
    for _ in changes {
        if let Ok(p) = get_all(&conn, NM, NM_PATH, NM) {
            topic.set(from_nm(&p), Network::encode);
        }
    }
    crate::warn!("hoststate: NetworkManager signal stream ended");
}

fn portal(conn: &Connection) -> zbus::Result<Network> {
    let p = Proxy::new(conn, PORTAL, PORTAL_PATH, MONITOR)?;
    let (available, metered, connectivity) =
        match p.call::<_, _, HashMap<String, OwnedValue>>("GetStatus", &()) {
            Ok(st) => {
                let b = |k: &str| st.get(k).and_then(|v| bool::try_from(v).ok());
                let Some(a) = b("available") else {
                    return Err(zbus::Error::Failure("GetStatus without available".into()));
                };
                let c = st.get("connectivity").and_then(|v| u32::try_from(v).ok());
                (a, b("metered").unwrap_or(false), c.unwrap_or(0))
            }
            Err(_) => {
                // v2: one call each. GetAvailable failing is the answer.
                let a: bool = p.call("GetAvailable", &())?;
                let m: bool = p.call("GetMetered", &()).unwrap_or(false);
                let c: u32 = p.call("GetConnectivity", &()).unwrap_or(0);
                (a, m, c)
            }
        };
    Ok(from_portal(available, metered, connectivity))
}

/// Portal connectivity: 1 local, 2 limited, 3 captive portal, 4 full.
pub fn from_portal(available: bool, metered: bool, connectivity: u32) -> Network {
    Network {
        available: if available { YES } else { NO },
        metered: if metered { YES } else { NO },
        connectivity: match connectivity {
            1..=4 => connectivity as u8 + 1,
            _ if available => UNKNOWN,
            _ => 1,
        },
    }
}

/// NetworkManager's `State` (NMState), `Connectivity` (0 unknown, 1 none,
/// 2 portal, 3 limited, 4 full) and `Metered` (NMMetered: 0 unknown, 1 yes,
/// 2 no, 3 guess yes, 4 guess no).
pub fn from_nm(p: &HashMap<String, OwnedValue>) -> Network {
    const CONNECTED_LOCAL: u32 = 50;
    const CONNECTED_SITE: u32 = 60;
    let u = |k: &str| p.get(k).and_then(|v| u32::try_from(v).ok()).unwrap_or(0);
    let state = u("State");
    Network {
        // A default route exists from CONNECTED_SITE on, like the portal's
        // "available".
        available: if state == 0 {
            UNKNOWN
        } else if state >= CONNECTED_SITE {
            YES
        } else {
            NO
        },
        metered: match u("Metered") {
            1 | 3 => YES,
            2 | 4 => NO,
            _ => UNKNOWN,
        },
        connectivity: match u("Connectivity") {
            1 if state >= CONNECTED_LOCAL => 2,
            1 => 1,
            2 => 4,
            3 => 3,
            4 => 5,
            _ if state != 0 && state < CONNECTED_LOCAL => 1,
            _ => UNKNOWN,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zbus::zvariant::Value;

    #[test]
    fn mapping() {
        assert_eq!(
            from_portal(true, true, 4),
            Network {
                available: YES,
                metered: YES,
                connectivity: 5
            }
        );
        assert_eq!(from_portal(false, false, 0).connectivity, 1);
        let nm: HashMap<String, OwnedValue> = [
            ("State", Value::from(70u32)),
            ("Connectivity", Value::from(4u32)),
            ("Metered", Value::from(3u32)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.try_to_owned().unwrap()))
        .collect();
        assert_eq!(
            from_nm(&nm),
            Network {
                available: YES,
                metered: YES,
                connectivity: 5
            }
        );
        assert_eq!(from_nm(&HashMap::new()), Network::default());
    }
}
