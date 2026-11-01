//! Closing and opening the bridge's own windows (DISPLAY_WINDOW). Such a
//! window shows a guest's whole screen, as OpenHarmony's does, so closing it
//! leaves the guest running.
//!
//! A close ends the window's display connection. The composer connects again
//! and asks for its window, and the display stays headless while its app id
//! is closed. [`Screens::show`] opens it again: it ends the headless
//! connections, and the composer's next DISPLAY_WINDOW gets a window with the
//! caller's activation token.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::ipc::server::Events;

#[derive(Default)]
pub struct Screens(Mutex<State>);

#[derive(Default)]
struct State {
    /// Display connections that asked for an own window, by their hub key.
    conns: HashMap<u64, Conn>,
    /// The app ids of windows that the user closed.
    closed: HashSet<String>,
    /// The activation token for the next window of an app id.
    tokens: HashMap<String, String>,
}

struct Conn {
    events: Events,
    app_id: String,
    headless: bool,
}

/// How a display connection's DISPLAY_WINDOW goes.
#[derive(Debug, PartialEq, Eq)]
pub enum Attach {
    /// Show the window, activated with the token if it isn't empty.
    Show { token: String },
    /// The user closed the window, so the display stays headless.
    Headless,
}

impl Screens {
    /// Records that display connection `conn` asks for a window of its own
    /// with `app_id`.
    pub fn attach(&self, conn: u64, events: Events, app_id: &str) -> Attach {
        let mut s = self.0.lock().unwrap();
        let headless = s.closed.contains(app_id);
        s.conns.insert(
            conn,
            Conn {
                events,
                app_id: app_id.into(),
                headless,
            },
        );
        if headless {
            Attach::Headless
        } else {
            let token = s.tokens.remove(app_id).unwrap_or_default();
            Attach::Show { token }
        }
    }

    /// Forgets a closed display connection.
    pub fn detach(&self, conn: u64) {
        self.0.lock().unwrap().conns.remove(&conn);
    }

    /// Closes the window of display connection `conn`. Returns false if the
    /// connection has no window of its own.
    pub fn close(&self, conn: u64) -> bool {
        let mut s = self.0.lock().unwrap();
        let Some(c) = s.conns.get(&conn).filter(|c| !c.headless) else {
            return false;
        };
        let app_id = c.app_id.clone();
        c.events.shutdown();
        s.closed.insert(app_id);
        true
    }

    /// Opens every closed window again, activated with `token` if it isn't
    /// empty. Returns false if none was closed.
    pub fn show(&self, token: &str) -> bool {
        let mut s = self.0.lock().unwrap();
        if s.closed.is_empty() {
            return false;
        }
        for app_id in std::mem::take(&mut s.closed) {
            if !token.is_empty() {
                s.tokens.insert(app_id, token.into());
            }
        }
        for c in s.conns.values().filter(|c| c.headless) {
            c.events.shutdown();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::OwnedFd;
    use std::sync::Arc;

    /// Events on one end of a socket pair, and the other end.
    fn events() -> (Events, OwnedFd) {
        let (server, client) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::NONBLOCK,
            None,
        )
        .unwrap();
        (Events::new(Arc::new(server)), client)
    }

    /// Whether the connection's server end has shut down.
    fn ended(client: &OwnedFd) -> bool {
        let mut b = [0u8; 8];
        rustix::net::recv(client, &mut b, rustix::net::RecvFlags::DONTWAIT)
            .is_ok_and(|(n, _)| n == 0)
    }

    const OHOS: &str = "dev.sargunv.innkeeper.ohos";

    #[test]
    fn a_closed_screen_stays_headless_until_shown() {
        let screens = Screens::default();
        let (e, first) = events();
        let show = Attach::Show {
            token: String::new(),
        };
        assert_eq!(screens.attach(1, e, OHOS), show);
        assert!(!screens.show("t"));
        assert!(screens.close(1));
        assert!(ended(&first));
        screens.detach(1);
        // The composer connects again and gets no window.
        let (e, second) = events();
        assert_eq!(screens.attach(2, e, OHOS), Attach::Headless);
        // A headless display has no window to close.
        assert!(!screens.close(2));
        assert!(!ended(&second));
        assert!(screens.show("token"));
        assert!(ended(&second));
        screens.detach(2);
        let (e, _third) = events();
        assert_eq!(
            screens.attach(3, e, OHOS),
            Attach::Show {
                token: "token".into()
            }
        );
        // The token goes to one window.
        let (e, _) = events();
        assert_eq!(screens.attach(4, e, OHOS), show);
    }

    #[test]
    fn only_connections_with_own_windows_close() {
        let screens = Screens::default();
        assert!(!screens.close(1));
        let (e, other) = events();
        screens.attach(1, e, "dev.sargunv.innkeeper.other");
        let (e, _) = events();
        screens.attach(2, e, OHOS);
        assert!(screens.close(2));
        // Another app id's window stays open.
        assert!(!ended(&other));
        let (e, _) = events();
        assert_eq!(
            screens.attach(3, e, "dev.sargunv.innkeeper.other"),
            Attach::Show {
                token: String::new()
            }
        );
    }
}
