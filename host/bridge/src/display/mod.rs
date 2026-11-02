//! The display service (id 2) shows the composer's HWC virtual displays in
//! host windows. The wire spec is in [`proto`].
//!
//! Each composer connection gets its own host Wayland connection and thread
//! ([`wl`]), whose seat feeds the input service. The service connection's
//! thread only validates requests and forwards them.

pub mod proto;
pub mod screens;
pub(crate) mod wl;

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use calloop::channel::Sender;

use crate::input::proto::WindowRequest;
use crate::input::{Hub, PendingWindow};
use crate::ipc::server::{Events, Peer, Reply, Service};
use crate::ipc::wire::{Writer, errno::*};
use proto::Request;
use wl::Cmd;

/// The xdg-foreign handle of the window showing Android display `window`, if
/// the compositor exported it. Portal dialogs take it as `wayland:<handle>`.
pub fn foreign_handle(window: u32) -> Option<String> {
    wl::foreign_handles(window)
}

/// Per-connection caps.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub displays: usize,
    pub buffers: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            displays: 32,
            buffers: 32 * 4,
        }
    }
}

pub struct DisplayService {
    hub: Arc<Hub>,
    /// This connection's key in the hub.
    conn: u64,
    cmd: Sender<Cmd>,
    events: Events,
    thread: Option<JoinHandle<()>>,
    limits: Limits,
    /// HWC display -> the window it shows.
    displays: HashMap<u32, u32>,
    /// Displays whose own window the user closed ([`screens`]). Their frames
    /// are released at once.
    headless: HashSet<u32>,
    /// Buffer -> its HWC display.
    buffers: HashMap<u32, u32>,
    label: String,
}

impl DisplayService {
    /// Connects to the host compositor (`$WAYLAND_DISPLAY`).
    pub fn new(
        limits: Limits,
        hub: Arc<Hub>,
        peer: &Peer,
        events: Events,
        shared: Arc<crate::system::Shared>,
    ) -> Result<DisplayService> {
        let conn = wayland_client::Connection::connect_to_env()
            .context("connecting to the host Wayland compositor")?;
        let (tx, rx) = calloop::channel::channel();
        let ender = events.clone();
        let service_events = events.clone();
        let seat_hub = hub.clone();
        let thread = std::thread::Builder::new()
            .name("display".into())
            .spawn(move || {
                if let Err(e) = wl::run(conn, rx, events, seat_hub, shared) {
                    crate::warn!("display: {e:#}");
                }
                // The service can't work without its compositor connection.
                // Dropping the client makes it reconnect and add its displays
                // again.
                ender.shutdown();
            })?;
        Ok(DisplayService {
            conn: hub.add_display_connection(tx.clone()),
            hub,
            cmd: tx,
            events: service_events,
            thread: Some(thread),
            limits,
            displays: HashMap::new(),
            headless: HashSet::new(),
            buffers: HashMap::new(),
            label: peer.label.clone(),
        })
    }

    fn send(&self, c: Cmd) -> Reply {
        match self.cmd.send(c) {
            Ok(()) => Reply::ok(),
            Err(_) => Reply::err(-EIO),
        }
    }

    fn add_display(&mut self, display: u32, width: u32, height: u32) -> Reply {
        if self.has(display) || self.displays.len() >= self.limits.displays {
            return Reply::err(-EBUSY);
        }
        let Some(pending) = self.hub.claim(width, height) else {
            return Reply::err(-ENOENT);
        };
        self.show(display, pending)
    }

    /// Whether `display` exists.
    fn has(&self, display: u32) -> bool {
        self.displays.contains_key(&display) || self.headless.contains(&display)
    }

    /// Adds a display with a window of its own (DISPLAY_WINDOW). If the user
    /// closed that window, the display stays headless.
    fn add_window(&mut self, display: u32, request: WindowRequest) -> Reply {
        if self.has(display) || self.displays.len() + self.headless.len() >= self.limits.displays {
            return Reply::err(-EBUSY);
        }
        let window = self.hub.own_window();
        let attach = self
            .hub
            .screens
            .attach(self.conn, self.events.clone(), &request.app_id);
        let token = match attach {
            screens::Attach::Show { token } => token,
            screens::Attach::Headless => {
                crate::info!("display: display {display} stays headless, its window is closed");
                self.headless.insert(display);
                let mut w = Writer::default();
                w.u32(window);
                return Reply::with_body(w.into_vec());
            }
        };
        self.show(
            display,
            PendingWindow {
                request: WindowRequest {
                    window,
                    token,
                    ..request
                },
                color: 0,
                icon: None,
            },
        )
    }

    /// Opens `pending`'s window for `display`.
    fn show(&mut self, display: u32, pending: PendingWindow) -> Reply {
        let req = pending.request.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        if self.cmd.send(Cmd::Create(pending, tx)).is_err() {
            return Reply::err(-EIO);
        }
        match rx.recv_timeout(std::time::Duration::from_secs(5)) {
            Ok(0) => {}
            Ok(e) => return Reply::err(e),
            Err(_) => return Reply::err(-EIO),
        }
        self.displays.insert(display, req.window);
        self.hub.mapped(self.conn, display, &req);
        let mut w = Writer::default();
        w.u32(req.window);
        Reply::with_body(w.into_vec())
    }

    /// Sends RELEASE for `buffer`.
    fn release(&self, buffer: u32, invalid: bool) {
        if self
            .events
            .try_send(
                proto::ty::RELEASE,
                &proto::event::release(buffer, invalid),
                &[],
            )
            .is_err()
        {
            self.events.shutdown();
        }
    }

    fn layers(&mut self, display: u32, below: u32, layers: Vec<proto::Layer>) -> Reply {
        let headless = self.headless.contains(&display);
        let window = self.displays.get(&display).copied();
        if window.is_none() && !headless {
            return Reply::err(-ENOENT);
        }
        // A layer whose buffer isn't the display's is left out and released,
        // so the composer imports it again. A headless display shows none.
        let mut shown = Vec::with_capacity(layers.len());
        let mut below_shown = 0;
        for (i, l) in layers.into_iter().enumerate() {
            if self.buffers.get(&l.buffer) != Some(&display) {
                self.release(l.buffer, true);
            } else if headless {
                self.release(l.buffer, false);
            } else {
                below_shown += u32::from((i as u32) < below);
                shown.push(l);
            }
        }
        match window {
            Some(window) => self.send(Cmd::Layers {
                window,
                below: below_shown,
                layers: shown,
            }),
            None => Reply::ok(),
        }
    }

    fn remove_display(&mut self, display: u32) -> Reply {
        if self.headless.remove(&display) {
            self.buffers.retain(|_, d| *d != display);
            return Reply::ok();
        }
        let Some(window) = self.displays.remove(&display) else {
            return Reply::err(-ENOENT);
        };
        self.buffers.retain(|_, d| *d != display);
        if self.hub.unmap(self.conn, display, window) {
            return self.send(Cmd::Destroy(window));
        }
        Reply::ok()
    }
}

/// Size of a dma-buf (or any seekable fd), without mapping it.
fn fd_size(fd: &OwnedFd) -> Option<u64> {
    rustix::fs::seek(fd.as_fd(), rustix::fs::SeekFrom::End(0)).ok()
}

impl Service for DisplayService {
    fn handle(&mut self, ty: u16, body: &[u8], mut fds: Vec<OwnedFd>) -> Reply {
        let req = match proto::decode(ty, body) {
            Ok(r) => r,
            Err(e) => return Reply::err(e.status()),
        };
        // BUFFER_ADD (the dma-buf), FRAME (an acquire fence) and CURSOR (the
        // image memfd) may carry one fd. Other requests carry none.
        let max_fds = match req {
            Request::BufferAdd(_) | Request::Frame { .. } | Request::Cursor { .. } => 1,
            _ => 0,
        };
        if fds.len() > max_fds {
            return Reply::err(-EPROTO);
        }
        match req {
            Request::ImageRelease { display, token } => {
                if let Some(window) = self.displays.get(&display) {
                    self.hub.image_released(*window, token);
                }
                Reply::ok()
            }
            Request::Cursor {
                display,
                width,
                height,
                x,
                y,
                hotspot,
            } => {
                if self.headless.contains(&display) {
                    return Reply::ok();
                }
                let Some(&window) = self.displays.get(&display) else {
                    return Reply::err(-ENOENT);
                };
                let pixels = if width == 0 && height == 0 && fds.is_empty() {
                    Vec::new()
                } else {
                    let Some(fd) = fds.pop() else {
                        return Reply::err(-EINVAL);
                    };
                    if width == 0 || height == 0 {
                        return Reply::err(-EINVAL);
                    }
                    match crate::ipc::blob::read(&fd, width * height * 4, 512 * 512 * 4) {
                        Ok(p) => p,
                        Err(e) => return Reply::err(e),
                    }
                };
                self.send(Cmd::Cursor {
                    window,
                    image: wl::cursor::Image {
                        width,
                        height,
                        x,
                        y,
                        hotspot,
                        pixels,
                    },
                })
            }
            Request::DisplayAdd {
                display,
                width,
                height,
            } => self.add_display(display, width, height),
            Request::DisplayRemove { display } => self.remove_display(display),
            Request::DisplayWindow {
                display,
                width,
                height,
                dpi,
                app_id,
                title,
            } => self.add_window(
                display,
                WindowRequest {
                    window: 0,
                    width,
                    height,
                    dpi,
                    min_width: 0,
                    min_height: 0,
                    app_id,
                    title,
                    token: String::new(),
                },
            ),
            Request::BufferAdd(b) => {
                let window = self.displays.get(&b.display).copied();
                if window.is_none() && !self.headless.contains(&b.display) {
                    return Reply::err(-ENOENT);
                }
                if self.buffers.contains_key(&b.buffer) || self.buffers.len() >= self.limits.buffers
                {
                    return Reply::err(-EBUSY);
                }
                let Some(fd) = fds.pop() else {
                    return Reply::err(-EINVAL);
                };
                // The compositor imports the dma-buf. A buffer that claims
                // more bytes than the fd holds is refused here.
                match fd_size(&fd) {
                    Some(n) if n >= b.bytes() => {}
                    _ => return Reply::err(-EINVAL),
                }
                self.buffers.insert(b.buffer, b.display);
                match window {
                    Some(window) => self.send(Cmd::BufferAdd(b, window, fd)),
                    None => Reply::ok(),
                }
            }
            Request::Layers {
                display,
                below,
                layers,
            } => self.layers(display, below, layers),
            Request::BufferRemove { buffer } => {
                let Some(display) = self.buffers.remove(&buffer) else {
                    return Reply::err(-ENOENT);
                };
                if self.headless.contains(&display) {
                    return Reply::ok();
                }
                self.send(Cmd::BufferRemove(buffer))
            }
            Request::Frame {
                display,
                buffer,
                damage,
            } => {
                if self.buffers.get(&buffer) != Some(&display) {
                    // A failed or removed import can still have a FRAME in
                    // flight. The request error alone does not return the
                    // frame's image lease, so RELEASE is sent as well.
                    self.release(buffer, true);
                    return Reply::err(-ENOENT);
                }
                // Any acquire fence is ignored, because the composer only
                // sends frames it has finished writing.
                drop(fds);
                if self.headless.contains(&display) {
                    self.release(buffer, false);
                    return Reply::ok();
                }
                if crate::log::debug() {
                    crate::log::trace(format_args!(
                        "receive ns={} buffer={buffer}",
                        crate::log::now()
                    ));
                }
                self.send(Cmd::Frame {
                    window: self.displays[&display],
                    buffer,
                    damage,
                })
            }
        }
    }
}

impl Drop for DisplayService {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.hub.remove_display_connection(self.conn);
        self.hub.screens.detach(self.conn);
        crate::info!("{}: display connection closed", self.label);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{
        transport,
        wire::{MAX_MSG, Reader},
    };

    /// A service with display 100 in window 2, display 101 headless, and
    /// buffers 7 and 8 on display 100 and 9 on display 101.
    fn service() -> (
        DisplayService,
        calloop::channel::Channel<Cmd>,
        rustix::fd::OwnedFd,
    ) {
        let (server, client) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::NONBLOCK,
            None,
        )
        .unwrap();
        let (cmd, rx) = calloop::channel::channel();
        let service = DisplayService {
            hub: Hub::new(),
            conn: 1,
            cmd,
            events: Events::new(Arc::new(server)),
            thread: None,
            limits: Limits::default(),
            displays: HashMap::from([(100, 2)]),
            headless: HashSet::from([101]),
            buffers: HashMap::from([(7, 100), (8, 100), (9, 101)]),
            label: "test".into(),
        };
        (service, rx, client)
    }

    /// The RELEASE events the client has, as (buffer, invalid).
    fn releases(client: &rustix::fd::OwnedFd) -> Vec<(u32, u32)> {
        let mut bytes = [0; MAX_MSG + 1];
        let mut out = Vec::new();
        while let Ok(Some(packet)) = transport::recv(client, &mut bytes) {
            let header = crate::ipc::wire::decode_header(&bytes[..packet.len]).unwrap();
            assert_eq!(header.ty, proto::ty::RELEASE);
            let mut body = Reader::new(&bytes[crate::ipc::wire::HDR..packet.len]);
            out.push((body.u32().unwrap(), body.u32().unwrap()));
        }
        out
    }

    fn layers(service: &mut DisplayService, display: u32, below: u32, buffers: &[u32]) -> i32 {
        let layers = buffers
            .iter()
            .map(|&buffer| proto::Layer {
                buffer,
                rect: [36, 558, 648, 486],
                crop: [0, 0, 320, 240],
            })
            .collect();
        let request = proto::encode(
            1,
            &Request::Layers {
                display,
                below,
                layers,
            },
        );
        service
            .handle(proto::ty::LAYERS, &request[crate::ipc::wire::HDR..], vec![])
            .status
    }

    #[test]
    fn layers_leave_out_other_displays_buffers() {
        let (mut service, rx, client) = service();
        // Buffer 9 is display 101's, and 5 is unknown. Both are released as
        // invalid, and the window gets the rest with one layer under its frame.
        assert_eq!(layers(&mut service, 100, 2, &[9, 7, 5, 8]), 0);
        assert_eq!(releases(&client), [(9, 1), (5, 1)]);
        match rx.try_recv() {
            Ok(Cmd::Layers {
                window: 2,
                below: 1,
                layers,
            }) => assert_eq!(layers.iter().map(|l| l.buffer).collect::<Vec<_>>(), [7, 8]),
            _ => panic!("no LAYERS for window 2"),
        }
        assert_eq!(layers(&mut service, 102, 0, &[]), -ENOENT);
    }

    #[test]
    fn a_headless_display_releases_its_layers() {
        let (mut service, rx, client) = service();
        assert_eq!(layers(&mut service, 101, 1, &[9]), 0);
        assert_eq!(releases(&client), [(9, 0)]);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn frame_after_buffer_removal_still_returns_its_lease() {
        let (mut service, _rx, client) = service();
        // The composer invalidates the failed import on the service before
        // adding the buffer again.
        assert_eq!(
            service
                .handle(proto::ty::BUFFER_REMOVE, &7u32.to_le_bytes(), vec![])
                .status,
            0
        );
        let request = proto::encode(
            1,
            &Request::Frame {
                display: 100,
                buffer: 7,
                damage: [0; 4],
            },
        );
        assert_eq!(
            service
                .handle(proto::ty::FRAME, &request[crate::ipc::wire::HDR..], vec![])
                .status,
            -ENOENT
        );
        // One RELEASE, which marks the import invalid.
        assert_eq!(releases(&client), [(7, 1)]);
    }
}
