//! The host's network: available, metered, connectivity, through the
//! NetworkMonitor portal's `GetStatus` (v3), re-read on its `changed` signal.
//! `Topic::set` drops identical notifications.

use std::collections::HashMap;

use super::proto::Network;
use super::{Topic, signals};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::OwnedValue;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const MONITOR: &str = "org.freedesktop.portal.NetworkMonitor";

/// `Network` field values (proto.rs).
const UNKNOWN: u8 = 0;
const NO: u8 = 1;
const YES: u8 = 2;

pub fn watch(session: Option<Connection>, topic: &Topic<Network>) {
    let Some(conn) = session else { return };
    let changes = signals(&conn, PORTAL_PATH, MONITOR, "changed");
    match portal(&conn) {
        Ok(n) => topic.set(n, Network::encode),
        Err(e) => return crate::warn!("hoststate: NetworkMonitor: {e}"),
    }
    let Ok(changes) = changes else { return };
    for _ in changes {
        if let Ok(n) = portal(&conn) {
            topic.set(n, Network::encode);
        }
    }
    crate::warn!("hoststate: NetworkMonitor signal stream ended");
}

fn portal(conn: &Connection) -> zbus::Result<Network> {
    let p = Proxy::new(conn, PORTAL, PORTAL_PATH, MONITOR)?;
    let st: HashMap<String, OwnedValue> = p.call("GetStatus", &())?;
    let b = |k: &str| st.get(k).and_then(|v| bool::try_from(v).ok());
    let Some(available) = b("available") else {
        return Err(zbus::Error::Failure("GetStatus without available".into()));
    };
    let connectivity = st.get("connectivity").and_then(|v| u32::try_from(v).ok());
    Ok(from_portal(
        available,
        b("metered").unwrap_or(false),
        connectivity.unwrap_or(0),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
