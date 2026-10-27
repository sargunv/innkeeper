//! The display service (id 2): host windows for the composer (the Android
//! HWC HAL). See `docs/bridge.md`, "Display service", and
//! `docs/windowing.md`.
//!
//! Each composer connection gets its own host Wayland connection and thread
//! ([`wl`]); this connection thread only validates requests and forwards them.

pub mod ime;
pub mod proto;
mod wl;

use std::os::fd::{AsFd, OwnedFd};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use calloop::channel::Sender;

use crate::ipc::server::{Events, Peer, Reply, Service};
use crate::ipc::wire::errno::*;
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
    pub windows: usize,
    pub buffers: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            windows: 32,
            buffers: 32 * 4,
        }
    }
}

pub struct DisplayService {
    cmd: Sender<Cmd>,
    thread: Option<JoinHandle<()>>,
    limits: Limits,
    windows: std::collections::HashSet<u32>,
    buffers: std::collections::HashMap<u32, u32>, // buffer -> window
    label: String,
}

impl DisplayService {
    /// Connects to the host compositor (`$WAYLAND_DISPLAY`).
    pub fn new(limits: Limits, peer: &Peer, events: Events) -> Result<DisplayService> {
        let conn = wayland_client::Connection::connect_to_env()
            .context("connecting to the host Wayland compositor")?;
        let (tx, rx) = calloop::channel::channel();
        let ender = events.clone();
        let label = peer.label.clone();
        let thread = std::thread::Builder::new()
            .name("display".into())
            .spawn(move || {
                if let Err(e) = wl::run(conn, rx, events) {
                    crate::warn!("display: {e:#}");
                }
                // Without its compositor connection the service is useless:
                // drop the client, which reconnects and asks for its windows
                // again.
                ender.shutdown();
            })?;
        Ok(DisplayService {
            cmd: tx,
            thread: Some(thread),
            limits,
            windows: Default::default(),
            buffers: Default::default(),
            label,
        })
    }

    fn send(&self, c: Cmd) -> Reply {
        match self.cmd.send(c) {
            Ok(()) => Reply::ok(),
            Err(_) => Reply::err(-EIO),
        }
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
            Request::BufferAdd(_) => 1,
            Request::Frame { .. } => 1,
            _ => 0,
        };
        if fds.len() > max_fds {
            return Reply::err(-EPROTO);
        }
        match req {
            Request::WindowCreate(s) => {
                if self.windows.contains(&s.window) {
                    return Reply::err(-EBUSY);
                }
                if self.windows.len() >= self.limits.windows {
                    return Reply::err(-EBUSY);
                }
                let id = s.window;
                let (tx, rx) = std::sync::mpsc::channel();
                if self.cmd.send(Cmd::Create(s, tx)).is_err() {
                    return Reply::err(-EIO);
                }
                match rx.recv_timeout(std::time::Duration::from_secs(5)) {
                    Ok(0) => {
                        self.windows.insert(id);
                        Reply::ok()
                    }
                    Ok(e) => Reply::err(e),
                    Err(_) => Reply::err(-EIO),
                }
            }
            Request::WindowDestroy { window } => {
                if !self.windows.remove(&window) {
                    return Reply::err(-ENOENT);
                }
                self.buffers.retain(|_, w| *w != window);
                self.send(Cmd::Destroy(window))
            }
            Request::WindowSetTitle { window, title } if self.windows.contains(&window) => {
                self.send(Cmd::Title(window, title))
            }
            Request::WindowSetMinSize {
                window,
                width,
                height,
            } if self.windows.contains(&window) => self.send(Cmd::MinSize(window, width, height)),
            Request::WindowActivate { window } if self.windows.contains(&window) => {
                self.send(Cmd::Activate(window))
            }
            Request::BufferAdd(b) => {
                if !self.windows.contains(&b.window) {
                    return Reply::err(-ENOENT);
                }
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
                self.buffers.insert(b.buffer, b.window);
                self.send(Cmd::BufferAdd(b, fd))
            }
            Request::BufferRemove { buffer } => {
                if self.buffers.remove(&buffer).is_none() {
                    return Reply::err(-ENOENT);
                }
                self.send(Cmd::BufferRemove(buffer))
            }
            Request::Frame {
                window,
                buffer,
                damage,
            } => {
                if self.buffers.get(&buffer) != Some(&window) {
                    return Reply::err(-ENOENT);
                }
                // An acquire fence, if any, is ignored: the composer only
                // sends frames it has finished writing.
                drop(fds);
                self.send(Cmd::Frame {
                    window,
                    buffer,
                    damage,
                })
            }
            Request::TextInput(r) => self.send(Cmd::TextInput(r)),
            _ => Reply::err(-ENOENT),
        }
    }
}

impl Drop for DisplayService {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Quit);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        crate::info!("{}: display connection closed", self.label);
    }
}
