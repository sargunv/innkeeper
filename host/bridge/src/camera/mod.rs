//! The camera service (id 9). It streams the desktop's camera to the camera
//! HAL through the Camera portal. The messages are in [`proto`].
//!
//! Android's `CAMERA` permission is the first consent: the HAL starts a
//! stream only when cameraserver opens the camera for an app that holds it.
//! The bridge then asks the portal on first use, as location does
//! (`hoststate::location`). A denial holds for the bridge's lifetime, so
//! Android's apps see the camera as unavailable for the session. The portal's
//! PipeWire connection carries the frames, which [`frame::Ring`] hands to the
//! HAL as NV12.
//!
//! - `portal`: `IsCameraPresent`, `AccessCamera`, `OpenPipeWireRemote`
//! - `pw`: the PipeWire capture stream, with libpipewire loaded at run time
//! - `pod`: the SPA formats the stream offers and receives
//! - `frame`: the frame ring and the conversion to NV12

pub mod frame;
pub mod pod;
pub mod portal;
pub mod proto;
pub mod pw;

use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use crate::ipc::server::{Events, Peer, Reply, Service};
use crate::ipc::wire::Writer;
use crate::ipc::wire::errno::*;
use proto::{Request, ev, status};

/// Bridge-wide camera state, created once in `main.rs`.
pub struct Camera {
    allow_portals: bool,
    session: OnceLock<Option<zbus::blocking::Connection>>,
    inner: Mutex<Inner>,
    next_client: AtomicU64,
}

#[derive(Default)]
struct Inner {
    /// The connection that streams, if any.
    owner: Option<u64>,
    /// Bumped by every start and stop, so a stale stream thread knows to
    /// give up.
    generation: u64,
    capture: Option<pw::Capture>,
    /// The user allowed the camera in this bridge's lifetime.
    granted: bool,
    /// The user said no. The bridge doesn't ask again.
    denied: bool,
}

impl Camera {
    /// `allow_portals` is false in a test session, which gets no camera.
    pub fn new(allow_portals: bool) -> Arc<Camera> {
        Arc::new(Camera {
            allow_portals,
            session: OnceLock::new(),
            inner: Mutex::default(),
            next_client: AtomicU64::new(1),
        })
    }

    fn session(&self) -> Option<zbus::blocking::Connection> {
        if !self.allow_portals {
            return None;
        }
        self.session
            .get_or_init(|| {
                zbus::blocking::Connection::session()
                    .map_err(|e| crate::warn!("camera: no session bus: {e}"))
                    .ok()
            })
            .clone()
    }

    fn current(&self, generation: u64) -> bool {
        self.inner.lock().unwrap().generation == generation
    }

    /// Ends `generation`'s stream, if it is still the current one, and
    /// tells its client why.
    fn end(&self, generation: u64, events: &Events, st: u8, why: &str) {
        let capture = {
            let mut i = self.inner.lock().unwrap();
            if st == status::DENIED {
                i.denied = true;
            }
            if i.generation != generation {
                return;
            }
            crate::info!("camera: {why} (status {st})");
            i.generation += 1;
            i.owner = None;
            i.capture.take()
        };
        let _ = events.try_send(ev::STATUS, &proto::encode_status(st), &[]);
        // This may run on the stream's own thread, which dropping joins.
        if let Some(c) = capture {
            spawn("camera-stop", move || drop(c));
        }
    }

    fn stop(&self, client: u64) {
        let capture = {
            let mut i = self.inner.lock().unwrap();
            if i.owner != Some(client) {
                return;
            }
            i.owner = None;
            i.generation += 1;
            i.capture.take()
        };
        crate::info!("camera: stopped");
        drop(capture);
    }

    /// The stream's thread: consent, the PipeWire connection, then the
    /// stream, which runs on its own thread until it ends or is stopped.
    fn run(
        self: Arc<Self>,
        generation: u64,
        events: Events,
        mut ring: frame::Ring,
        size: (u32, u32),
    ) {
        let Some(conn) = self.session() else {
            return self.end(generation, &events, status::UNAVAILABLE, "no session bus");
        };
        if !self.inner.lock().unwrap().granted {
            crate::info!("camera: asking the desktop");
            if let Err((st, why)) = portal::access(&conn) {
                return self.end(generation, &events, st, &why);
            }
            self.inner.lock().unwrap().granted = true;
        }
        if !self.current(generation) {
            return;
        }
        let remote = match portal::open_remote(&conn) {
            Ok(fd) => fd,
            Err((st, why)) => return self.end(generation, &events, st, &why),
        };
        // A STOP while the portal answered cancels this stream.
        if !self.current(generation) {
            return;
        }
        let me = self.clone();
        let ev = events.clone();
        let mut running = false;
        // A stopped or replaced stream's callbacks publish nothing, because
        // the connection may already have started another one.
        let capture = pw::Capture::start(remote, size.0, size.1, move |e| match e {
            pw::Event::Streaming | pw::Event::Frame(_) if !me.current(generation) => {}
            pw::Event::Streaming if !running => {
                running = true;
                crate::info!("camera: streaming");
                let _ = ev.try_send(ev::STATUS, &proto::encode_status(status::RUNNING), &[]);
            }
            pw::Event::Streaming => {}
            pw::Event::Frame(src) => {
                if let Some((slot, n)) = ring.write(src, monotonic_ns()) {
                    let _ = ev.try_send(ev::FRAME, &proto::encode_frame(slot, n), &[]);
                }
            }
            pw::Event::Ended(why) => me.end(generation, &ev, status::UNAVAILABLE, &why),
        });
        let capture = match capture {
            Ok(c) => c,
            Err(why) => return self.end(generation, &events, status::UNAVAILABLE, &why),
        };
        let mut i = self.inner.lock().unwrap();
        if i.generation == generation {
            i.capture = Some(capture);
        } else {
            drop(i);
            drop(capture);
        }
    }
}

fn monotonic_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) {
    if let Err(e) = std::thread::Builder::new().name(name.into()).spawn(f) {
        crate::warn!("spawn {name}: {e}");
    }
}

pub struct CameraService {
    shared: Arc<Camera>,
    events: Events,
    label: String,
    client: u64,
}

impl CameraService {
    pub fn new(shared: Arc<Camera>, peer: &Peer, events: Events) -> CameraService {
        let client = shared.next_client.fetch_add(1, Ordering::Relaxed);
        CameraService {
            shared,
            events,
            label: peer.label.clone(),
            client,
        }
    }

    fn info(&self) -> Reply {
        let present = self.shared.session().is_some_and(|c| portal::present(&c));
        crate::info!("camera: {}: present {present}", self.label);
        Reply::with_body(proto::encode_status(present as u8))
    }

    fn start(&self, width: u16, height: u16) -> Reply {
        if !self.shared.allow_portals {
            return Reply::err(-ENOSYS);
        }
        let generation = {
            let mut i = self.shared.inner.lock().unwrap();
            if i.denied {
                return Reply::err(-EACCES);
            }
            if i.owner.is_some() {
                return Reply::err(-EBUSY);
            }
            i.owner = Some(self.client);
            i.generation += 1;
            i.generation
        };
        let ring = match frame::Ring::new() {
            Ok(r) => r,
            Err(e) => {
                crate::warn!("camera: frame ring: {e}");
                self.shared.stop(self.client);
                return Reply::err(-ENOMEM);
            }
        };
        let fd: OwnedFd = match ring.fd().try_clone_to_owned() {
            Ok(fd) => fd,
            Err(e) => {
                crate::warn!("camera: frame ring: {e}");
                self.shared.stop(self.client);
                return Reply::err(-EIO);
            }
        };
        let mut w = Writer::default();
        w.u32(frame::SLOTS).u32(ring.slot_bytes() as u32);
        crate::info!("camera: {}: start {width}x{height}", self.label);
        let (shared, events) = (self.shared.clone(), self.events.clone());
        let size = (width as u32, height as u32);
        spawn("camera", move || shared.run(generation, events, ring, size));
        Reply {
            status: 0,
            body: w.into_vec(),
            fds: vec![fd],
        }
    }
}

impl Service for CameraService {
    fn handle(&mut self, t: u16, body: &[u8], fds: Vec<OwnedFd>) -> Reply {
        let req = match proto::decode(t, body) {
            Ok(r) => r,
            Err(e) => return Reply::err(e.status()),
        };
        if !fds.is_empty() {
            return Reply::err(-EPROTO);
        }
        match req {
            Request::Info => self.info(),
            Request::Start { width, height } => self.start(width, height),
            Request::Stop => {
                self.shared.stop(self.client);
                Reply::ok()
            }
        }
    }
}

impl Drop for CameraService {
    fn drop(&mut self) {
        self.shared.stop(self.client);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;

    fn service(allow: bool) -> (CameraService, OwnedFd) {
        let (a, b) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        let events = Events::new(Arc::new(a));
        (
            CameraService::new(Camera::new(allow), &Peer::test(), events),
            b,
        )
    }

    fn start(s: &mut CameraService) -> Reply {
        s.handle(proto::ty::START, &[0x80, 2, 0xe0, 1, 0, 0, 0, 0], vec![])
    }

    #[test]
    fn test_sessions_have_no_camera() {
        let (mut s, _peer) = service(false);
        let r = s.handle(proto::ty::INFO, &[], vec![]);
        assert_eq!((r.status, r.body), (0, vec![0, 0, 0, 0]));
        assert_eq!(start(&mut s).status, -ENOSYS);
        assert!(s.shared.session.get().is_none());
    }

    #[test]
    fn requests_with_fds_or_bad_bodies_are_refused() {
        let (mut s, peer) = service(false);
        let fd = peer.as_fd().try_clone_to_owned().unwrap();
        assert_eq!(s.handle(proto::ty::STOP, &[], vec![fd]).status, -EPROTO);
        assert_eq!(s.handle(proto::ty::START, &[1], vec![]).status, -EINVAL);
        assert_eq!(s.handle(proto::ty::STOP, &[], vec![]).status, 0);
    }

    #[test]
    fn one_stream_at_a_time_and_denial_sticks() {
        let cam = Camera::new(true);
        let (mut a, _pa) = service(true);
        let (mut b, _pb) = service(true);
        a.shared = cam.clone();
        b.shared = cam.clone();
        b.client = a.client + 1;
        // Hold the stream without a thread: mark a as the owner.
        cam.inner.lock().unwrap().owner = Some(a.client);
        assert_eq!(start(&mut b).status, -EBUSY);
        // b's stop doesn't end a's stream.
        b.handle(proto::ty::STOP, &[], vec![]);
        assert_eq!(cam.inner.lock().unwrap().owner, Some(a.client));
        a.handle(proto::ty::STOP, &[], vec![]);
        assert_eq!(cam.inner.lock().unwrap().owner, None);
        // A denial for a stream that was already replaced still sticks.
        let g = cam.inner.lock().unwrap().generation;
        cam.end(
            g + 5,
            &a.events,
            status::DENIED,
            "the user denied the camera",
        );
        assert_eq!(start(&mut b).status, -EACCES);
    }

    #[test]
    fn ending_a_stream_frees_it_and_tells_the_client() {
        let (mut s, peer) = service(true);
        let g = {
            let mut i = s.shared.inner.lock().unwrap();
            i.owner = Some(s.client);
            i.generation += 1;
            i.generation
        };
        s.shared
            .end(g, &s.events, status::UNAVAILABLE, "the camera went away");
        let mut buf = [0u8; 64];
        let n = rustix::net::recv(&peer, &mut buf, rustix::net::RecvFlags::DONTWAIT).unwrap();
        let h = crate::ipc::wire::decode_header(&buf[..n.0]).unwrap();
        assert_eq!(h.ty, ev::STATUS);
        assert_eq!(
            &buf[crate::ipc::wire::HDR..n.0],
            &[status::UNAVAILABLE, 0, 0, 0]
        );
        // A stale end is ignored.
        s.shared.end(g, &s.events, status::UNAVAILABLE, "again");
        assert!(rustix::net::recv(&peer, &mut buf, rustix::net::RecvFlags::DONTWAIT).is_err());
        assert_eq!(s.shared.inner.lock().unwrap().owner, None);
        let _ = &mut s;
    }
}
