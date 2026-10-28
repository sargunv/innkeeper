//! What the input and display services share: the window manager's queued
//! window requests, which display connection shows each window, and the
//! input connections that get the seat's lines.
//!
//! The window manager names windows by Android display id; the composer
//! names its HWC displays by its own ids. A request matches the first HWC
//! display of its size that the composer adds after it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use calloop::channel::Sender;

use super::ime;
use super::proto::{WindowRequest, event, ty};
use crate::display::wl::Cmd;
use crate::ipc::Subscribers;
use crate::ipc::server::Events;
use crate::ipc::wire::errno::*;

/// How long a request waits for its display.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Hub {
    state: Mutex<State>,
    clients: Subscribers,
}

#[derive(Default)]
struct State {
    queued: Vec<(WindowRequest, Instant)>,
    /// Window -> the display connection and HWC display showing it.
    shown: HashMap<u32, (u64, u32)>,
    /// Display connections: their Wayland threads.
    displays: HashMap<u64, Sender<Cmd>>,
    next_display: u64,
}

impl Hub {
    pub fn new() -> Arc<Hub> {
        Arc::default()
    }

    // ---- the input service's side

    pub fn subscribe(&self, events: Events) -> u64 {
        self.clients.add(events)
    }

    pub fn unsubscribe(&self, key: u64) {
        self.clients.remove(key)
    }

    /// Queues a request; one for a queued window replaces it.
    pub fn request(&self, r: WindowRequest) -> Result<(), i32> {
        let mut s = self.state.lock().unwrap();
        if s.shown.contains_key(&r.window) {
            return Err(-EBUSY);
        }
        let now = Instant::now();
        s.queued
            .retain(|(q, at)| q.window != r.window && now - *at < REQUEST_TIMEOUT);
        s.queued.push((r, now));
        Ok(())
    }

    pub fn activate(&self, window: u32) -> Result<(), i32> {
        let s = self.state.lock().unwrap();
        let (conn, _) = s.shown.get(&window).ok_or(-ENOENT)?;
        s.displays[conn]
            .send(Cmd::Activate(window))
            .map_err(|_| -EIO)
    }

    /// Drops a queued request, or closes a shown window.
    pub fn destroy(&self, window: u32) {
        let mut s = self.state.lock().unwrap();
        s.queued.retain(|(q, _)| q.window != window);
        if let Some((conn, _)) = s.shown.remove(&window) {
            let _ = s.displays[&conn].send(Cmd::Destroy(window));
            drop(s);
            self.unmapped(window);
        }
    }

    /// Goes to every display connection; the one whose seat has text-input
    /// focus applies it.
    pub fn text_input(&self, req: ime::Req) {
        for d in self.state.lock().unwrap().displays.values() {
            let _ = d.send(Cmd::TextInput(req.clone()));
        }
    }

    // ---- the display service's side

    pub fn add_display_connection(&self, cmd: Sender<Cmd>) -> u64 {
        let mut s = self.state.lock().unwrap();
        s.next_display += 1;
        let k = s.next_display;
        s.displays.insert(k, cmd);
        k
    }

    /// The connection is gone, and with it its windows.
    pub fn remove_display_connection(&self, conn: u64) {
        let gone: Vec<u32> = {
            let mut s = self.state.lock().unwrap();
            s.displays.remove(&conn);
            let gone = s
                .shown
                .iter()
                .filter(|(_, (c, _))| *c == conn)
                .map(|(w, _)| *w)
                .collect();
            s.shown.retain(|_, (c, _)| *c != conn);
            gone
        };
        for w in gone {
            self.unmapped(w);
        }
    }

    /// The request a new HWC display of this size answers, if any.
    pub fn claim(&self, width: u32, height: u32) -> Option<WindowRequest> {
        let mut s = self.state.lock().unwrap();
        let now = Instant::now();
        s.queued.retain(|(_, at)| now - *at < REQUEST_TIMEOUT);
        let i = s
            .queued
            .iter()
            .position(|(q, _)| q.width == width && q.height == height)?;
        Some(s.queued.remove(i).0)
    }

    /// `display` of connection `conn` now shows the window `r` asked for.
    pub fn mapped(&self, conn: u64, display: u32, r: &WindowRequest) {
        self.state
            .lock()
            .unwrap()
            .shown
            .insert(r.window, (conn, display));
        crate::info!("input: window {} shows display {display}", r.window);
        let body = event::window_mapped(r.window);
        self.clients.broadcast(ty::WINDOW_MAPPED, &body, &[], None);
    }

    /// `display` went away. Returns true if it still showed `window`, which
    /// the caller then closes.
    pub fn unmap(&self, conn: u64, display: u32, window: u32) -> bool {
        let mut s = self.state.lock().unwrap();
        if s.shown.get(&window) != Some(&(conn, display)) {
            return false;
        }
        s.shown.remove(&window);
        drop(s);
        self.unmapped(window);
        true
    }

    fn unmapped(&self, window: u32) {
        let body = event::window_unmapped(window);
        self.clients
            .broadcast(ty::WINDOW_UNMAPPED, &body, &[], None);
    }

    /// One input line to every input connection.
    pub fn input(&self, line: &str) {
        self.clients
            .broadcast(ty::INPUT, &event::input(line), &[], None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(window: u32, width: u32) -> WindowRequest {
        WindowRequest {
            window,
            width,
            height: 800,
            dpi: 160,
            min_width: 0,
            min_height: 0,
            app_id: "dev.linuxapk.test".into(),
            title: "Test".into(),
        }
    }

    #[test]
    fn a_display_claims_the_request_of_its_size() {
        let hub = Hub::new();
        hub.request(request(5, 1280)).unwrap();
        hub.request(request(6, 1000)).unwrap();
        assert_eq!(hub.claim(1000, 800).map(|r| r.window), Some(6));
        assert_eq!(hub.claim(1000, 800), None);
        assert_eq!(hub.claim(1280, 800).map(|r| r.window), Some(5));
    }

    #[test]
    fn a_shown_window_is_busy_until_its_display_goes() {
        let hub = Hub::new();
        let (tx, rx) = calloop::channel::channel();
        let conn = hub.add_display_connection(tx);
        hub.request(request(5, 1280)).unwrap();
        let r = hub.claim(1280, 800).unwrap();
        hub.mapped(conn, 2, &r);
        assert_eq!(hub.request(request(5, 1280)), Err(-EBUSY));
        hub.activate(5).unwrap();
        assert!(matches!(rx.try_recv(), Ok(Cmd::Activate(5))));
        // Another display of this connection never showed it.
        assert!(!hub.unmap(conn, 3, 5));
        assert!(hub.unmap(conn, 2, 5));
        assert_eq!(hub.activate(5), Err(-ENOENT));
        hub.request(request(5, 1280)).unwrap();
    }

    #[test]
    fn destroy_drops_requests_and_closes_windows() {
        let hub = Hub::new();
        let (tx, rx) = calloop::channel::channel();
        let conn = hub.add_display_connection(tx);
        hub.request(request(5, 1280)).unwrap();
        hub.destroy(5);
        assert_eq!(hub.claim(1280, 800), None);
        hub.request(request(6, 1280)).unwrap();
        let r = hub.claim(1280, 800).unwrap();
        hub.mapped(conn, 2, &r);
        hub.destroy(6);
        assert!(matches!(rx.try_recv(), Ok(Cmd::Destroy(6))));
        // Its display going away later closes nothing more.
        assert!(!hub.unmap(conn, 2, 6));
    }

    #[test]
    fn a_closed_display_connection_unmaps_its_windows() {
        let hub = Hub::new();
        let (tx, _rx) = calloop::channel::channel();
        let conn = hub.add_display_connection(tx);
        hub.request(request(5, 1280)).unwrap();
        let r = hub.claim(1280, 800).unwrap();
        hub.mapped(conn, 2, &r);
        hub.remove_display_connection(conn);
        hub.request(request(5, 1280)).unwrap();
    }
}
