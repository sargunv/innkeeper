//! The display service (id 2): the composer's HWC virtual displays shown in
//! host windows. The wire spec is in [`proto`].
//!
//! Each composer connection gets its own host Wayland connection and thread
//! ([`wl`]), whose seat feeds the input service; this connection thread only
//! validates requests and forwards them.

pub mod proto;
pub(crate) mod wl;

use std::collections::HashMap;
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
/// the compositor exported it (for portal dialogs: `wayland:<handle>`).
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
                // Without its compositor connection the service is useless:
                // drop the client, which reconnects and adds its displays
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
        if self.displays.contains_key(&display) || self.displays.len() >= self.limits.displays {
            return Reply::err(-EBUSY);
        }
        let Some(pending) = self.hub.claim(width, height) else {
            return Reply::err(-ENOENT);
        };
        self.show(display, pending)
    }

    /// A display with a window of its own (DISPLAY_WINDOW).
    fn add_window(&mut self, display: u32, request: WindowRequest) -> Reply {
        if self.displays.contains_key(&display) || self.displays.len() >= self.limits.displays {
            return Reply::err(-EBUSY);
        }
        let window = self.hub.own_window();
        self.show(
            display,
            PendingWindow {
                request: WindowRequest { window, ..request },
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

    fn remove_display(&mut self, display: u32) -> Reply {
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
        // Only BUFFER_ADD (the dma-buf) and FRAME (an acquire fence) carry fds.
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
            } => {
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
                let Some(&window) = self.displays.get(&b.display) else {
                    return Reply::err(-ENOENT);
                };
                if self.buffers.contains_key(&b.buffer) || self.buffers.len() >= self.limits.buffers
                {
                    return Reply::err(-EBUSY);
                }
                let Some(fd) = fds.pop() else {
                    return Reply::err(-EINVAL);
                };
                // The compositor imports the dma-buf; a buffer that claims
                // more bytes than the fd holds is refused here.
                match fd_size(&fd) {
                    Some(n) if n >= b.bytes() => {}
                    _ => return Reply::err(-EINVAL),
                }
                self.buffers.insert(b.buffer, b.display);
                self.send(Cmd::BufferAdd(b, window, fd))
            }
            Request::BufferRemove { buffer } => {
                if self.buffers.remove(&buffer).is_none() {
                    return Reply::err(-ENOENT);
                }
                self.send(Cmd::BufferRemove(buffer))
            }
            Request::Frame {
                display,
                buffer,
                damage,
            } => {
                if self.buffers.get(&buffer) != Some(&display) {
                    // A failed/removed import can still have a FRAME in flight.
                    // A request error alone does not return its image lease.
                    if self
                        .events
                        .try_send(
                            proto::ty::RELEASE,
                            &proto::event::release(buffer, true),
                            &[],
                        )
                        .is_err()
                    {
                        self.events.shutdown();
                    }
                    return Reply::err(-ENOENT);
                }
                // An acquire fence, if any, is ignored: the composer only
                // sends frames it has finished writing.
                drop(fds);
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

    #[test]
    fn frame_after_buffer_removal_still_returns_its_lease() {
        let (server, client) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::NONBLOCK,
            None,
        )
        .unwrap();
        let (cmd, _rx) = calloop::channel::channel();
        let mut service = DisplayService {
            hub: Hub::new(),
            conn: 1,
            cmd,
            events: Events::new(Arc::new(server)),
            thread: None,
            limits: Limits::default(),
            displays: HashMap::from([(100, 2)]),
            buffers: HashMap::from([(7, 100)]),
            label: "test".into(),
        };
        // Composer invalidates the failed import on the service before re-add.
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
        let mut bytes = [0; MAX_MSG + 1];
        let packet = transport::recv(&client, &mut bytes).unwrap().unwrap();
        let header = crate::ipc::wire::decode_header(&bytes[..packet.len]).unwrap();
        assert_eq!(header.ty, proto::ty::RELEASE);
        let mut body = Reader::new(&bytes[crate::ipc::wire::HDR..packet.len]);
        assert_eq!(body.u32().unwrap(), 7);
        assert_eq!(body.u32().unwrap(), 1); // invalid, not reusable
        body.end().unwrap();
        assert!(transport::recv(&client, &mut bytes).is_err()); // exactly one
    }
}
