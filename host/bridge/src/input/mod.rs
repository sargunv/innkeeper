//! The input service (id 6): the Linux.apk System app's window requests, the
//! host seat as input lines, and the host's input methods for its IME. The
//! wire spec is in [`proto`].
//!
//! The display service owns the Wayland connection and the windows; it hands
//! seat events to [`seat`] and [`text_input`], whose lines go to every input
//! connection through the [`Hub`].

pub(crate) mod gestures;
mod hub;
pub mod ime;
pub mod proto;
pub(crate) mod seat;
pub(crate) mod tablet;
pub(crate) mod text_input;
pub mod xkb;

use std::os::fd::OwnedFd;
use std::sync::Arc;

pub use hub::Hub;

use crate::ipc::server::{Events, Peer, Reply, Service};
use crate::ipc::wire::errno::*;
use proto::Request;

pub struct InputService {
    hub: Arc<Hub>,
    subscription: u64,
    events: Events,
    label: String,
}

impl InputService {
    pub fn new(hub: Arc<Hub>, peer: &Peer, events: Events) -> InputService {
        let subscription = hub.subscribe(events.clone());
        InputService {
            hub,
            subscription,
            events,
            label: peer.label.clone(),
        }
    }
}

fn status(r: Result<(), i32>) -> Reply {
    match r {
        Ok(()) => Reply::ok(),
        Err(e) => Reply::err(e),
    }
}

impl Service for InputService {
    fn handle(&mut self, ty: u16, body: &[u8], fds: Vec<OwnedFd>) -> Reply {
        if !fds.is_empty() {
            return Reply::err(-EPROTO);
        }
        let req = match proto::decode(ty, body) {
            Ok(r) => r,
            Err(e) => return Reply::err(e.status()),
        };
        match req {
            Request::WindowRequest(mut r) => {
                r.app_id = linuxapk_core::desktop::desktop_app_id(&r.app_id);
                crate::info!(
                    "input: window {} requested: {}x{} @ {} dpi, {}",
                    r.window,
                    r.width,
                    r.height,
                    r.dpi,
                    r.app_id
                );
                status(self.hub.request(r))
            }
            Request::WindowActivate { window } => status(self.hub.activate(window)),
            Request::WindowDestroy { window } => {
                self.hub.destroy(window);
                Reply::ok()
            }
            Request::TextInput(r) => {
                self.hub.text_input(r);
                Reply::ok()
            }
        }
    }

    fn connected(&mut self) {
        self.hub.replay(&self.events);
    }
}

impl Drop for InputService {
    fn drop(&mut self) {
        self.hub.unsubscribe(self.subscription);
        crate::info!("{}: input connection closed", self.label);
    }
}
