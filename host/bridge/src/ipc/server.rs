//! The accept loop and per-connection threads.
//!
//! Each connection must come from our own uid, checked with `SO_PEERCRED`.
//! Every Android process runs as the host user. A connection must start with
//! HELLO naming one registered service, and is then handed to that service.
//! A service is anything implementing [`Service`]. To add one, register a
//! factory for its id in a [`Registry`], as `main.rs` does.
//! `docs/bridge-protocol.md` describes the conventions.

use std::os::fd::{BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};

use crate::ipc::proto::{self, Hello};
use crate::ipc::transport;
use crate::ipc::wire::{self, MAX_MSG, errno::*};

/// A reply to one request, with a status (0 or -errno), a body and fds to
/// pass.
pub struct Reply {
    pub status: i32,
    pub body: Vec<u8>,
    pub fds: Vec<OwnedFd>,
}

impl Reply {
    pub fn ok() -> Reply {
        Reply {
            status: 0,
            body: Vec::new(),
            fds: Vec::new(),
        }
    }
    pub fn err(status: i32) -> Reply {
        Reply {
            status,
            body: Vec::new(),
            fds: Vec::new(),
        }
    }
    pub fn with_body(body: Vec<u8>) -> Reply {
        Reply {
            status: 0,
            body,
            fds: Vec::new(),
        }
    }
}

/// Who is on the other end of a connection.
#[derive(Debug, Clone)]
pub struct Peer {
    /// Host uid and pid from SO_PEERCRED. Pid 0 means the peer is outside
    /// this PID namespace.
    pub uid: u32,
    pub pid: i32,
    /// Client name from HELLO.
    pub name: String,
    /// "pid N uid M", for logs.
    pub label: String,
}

impl Peer {
    /// A peer for unit tests.
    pub fn test() -> Peer {
        Peer {
            uid: 0,
            pid: 0,
            name: "test".into(),
            label: "test".into(),
        }
    }
}

/// Sends unsolicited events (types `wire::EVENT_MIN..=EVENT_MAX`, tag 0) to
/// one connection. It is cheap to clone, so a service can hand it to its own
/// threads. Each event is one packet, so events and replies can't interleave
/// mid-message. Their order relative to each other is not defined. Sending
/// after the client went away fails with an error.
#[derive(Clone)]
pub struct Events {
    sock: Arc<OwnedFd>,
}

impl Events {
    /// Creates events on `sock`, for tests.
    pub fn new(sock: Arc<OwnedFd>) -> Events {
        Events { sock }
    }

    /// Sends one event without waiting. When the client's socket buffer is
    /// full this fails with EAGAIN. A client that doesn't read loses events
    /// instead of stalling the service and every client sharing its locks.
    pub fn try_send(&self, ty: u16, body: &[u8], fds: &[BorrowedFd<'_>]) -> Result<()> {
        if !(wire::EVENT_MIN..=wire::EVENT_MAX).contains(&ty) {
            bail!("event type {ty:#06x} outside the event range");
        }
        let mut w = wire::Writer::message(ty, 0);
        w.0.extend_from_slice(body);
        if w.0.len() > MAX_MSG {
            bail!("event too long ({} bytes)", w.0.len());
        }
        transport::send_nonblocking(&self.sock, &w.0, fds)
    }

    /// Ends the connection, for example when the service's backend went
    /// away. The client sees EOF and may reconnect.
    pub fn shutdown(&self) {
        let _ = rustix::net::shutdown(&*self.sock, rustix::net::Shutdown::Both);
    }
}

/// A service bound to one connection. `handle` gets every request after
/// HELLO with its message type, its untrusted body and any fds the client
/// attached. Decode the body with `wire::Reader`. The fds close when dropped.
/// Dropping the service cleans up the connection.
pub trait Service: Send {
    fn handle(&mut self, ty: u16, body: &[u8], fds: Vec<OwnedFd>) -> Reply;

    /// Called once the HELLO reply has been sent. The client drops events
    /// that arrive before the reply, so send the state a new connection needs
    /// from here.
    fn connected(&mut self) {}
}

type Factory = Box<dyn Fn(&Peer, Events) -> Result<Box<dyn Service>> + Send + Sync>;

/// Service ids to factories.
#[derive(Default)]
pub struct Registry {
    entries: Vec<(u32, &'static str, Factory)>,
}

impl Registry {
    pub fn register(
        &mut self,
        id: u32,
        name: &'static str,
        f: impl Fn(&Peer, Events) -> Result<Box<dyn Service>> + Send + Sync + 'static,
    ) {
        assert!(
            self.entries.iter().all(|e| e.0 != id),
            "service {id} registered twice"
        );
        self.entries.push((id, name, Box::new(f)));
    }
    pub fn names(&self) -> Vec<&'static str> {
        self.entries.iter().map(|e| e.1).collect()
    }
    fn open(&self, id: u32, peer: &Peer, events: Events) -> Option<Result<Box<dyn Service>>> {
        self.entries
            .iter()
            .find(|e| e.0 == id)
            .map(|e| (e.2)(peer, events))
    }
}

pub struct Config {
    pub max_clients: usize,
    pub services: Registry,
}

/// Runs the accept loop. It returns only if accept fails hard.
pub fn serve(listener: OwnedFd, cfg: Arc<Config>) -> Result<()> {
    let clients = Arc::new(AtomicUsize::new(0));
    let own = rustix::process::getuid().as_raw();
    loop {
        let (sock, cred) = match transport::accept(&listener) {
            Ok(v) => v,
            Err(rustix::io::Errno::INTR | rustix::io::Errno::CONNABORTED) => continue,
            Err(e) => return Err(e).context("accept"),
        };
        let (uid, pid) = (cred.uid, cred.pid);
        if uid != own {
            crate::warn!("refused connection from uid {uid} (pid {pid})");
            continue;
        }
        if clients.fetch_add(1, Ordering::AcqRel) >= cfg.max_clients {
            clients.fetch_sub(1, Ordering::AcqRel);
            crate::warn!("refused connection from pid {pid}: too many clients");
            continue;
        }
        let peer = Peer {
            uid,
            pid,
            name: String::new(),
            label: format!("pid {pid} uid {uid}"),
        };
        let (cfg, clients) = (cfg.clone(), clients.clone());
        let spawned = std::thread::Builder::new()
            .name("client".into())
            .spawn(move || {
                let label = peer.label.clone();
                if let Err(e) = client(sock, &cfg, peer) {
                    crate::info!("{label}: {e:#}");
                }
                clients.fetch_sub(1, Ordering::AcqRel);
            });
        if let Err(e) = spawned {
            crate::warn!("spawn client thread: {e}");
        }
    }
}

fn send_reply(sock: &OwnedFd, tag: u32, r: &Reply) -> Result<()> {
    transport::send_owned(sock, &wire::reply(tag, r.status, &r.body), &r.fds)
}

fn client(sock: OwnedFd, cfg: &Config, mut peer: Peer) -> Result<()> {
    let sock = Arc::new(sock);
    let mut buf = [0u8; MAX_MSG + 1];
    let mut svc: Option<Box<dyn Service>> = None;
    while let Some(pkt) = transport::recv(&sock, &mut buf)? {
        let msg = &buf[..pkt.len];
        let h = match wire::decode_header(msg) {
            Ok(h) => h,
            Err(e) => bail!("unreadable header: {e:?}"),
        };
        let body = &msg[wire::HDR..];
        let Some(s) = svc.as_mut() else {
            // The first message must be a well-formed HELLO for a known
            // service at our major version. Anything else ends the
            // connection.
            if !pkt.fds.is_empty() || h.ty != proto::HELLO {
                send_reply(&sock, h.tag, &Reply::err(-EPROTO))?;
                bail!("expected HELLO");
            }
            let hello = match Hello::decode(body) {
                Ok(v) => v,
                Err(e) => {
                    send_reply(&sock, h.tag, &Reply::err(e.status()))?;
                    bail!("bad HELLO: {e:?}");
                }
            };
            if hello.major != proto::MAJOR {
                send_reply(&sock, h.tag, &Reply::err(-EPROTO))?;
                bail!("protocol {}.{} not supported", hello.major, hello.minor);
            }
            peer.name = hello.name.clone();
            match cfg
                .services
                .open(hello.service, &peer, Events { sock: sock.clone() })
            {
                None => {
                    send_reply(&sock, h.tag, &Reply::err(-ENOSYS))?;
                    bail!("unknown service {}", hello.service);
                }
                Some(Err(e)) => {
                    send_reply(&sock, h.tag, &Reply::err(-EIO))?;
                    bail!("service {}: {e:#}", hello.service);
                }
                Some(Ok(s)) => svc = Some(s),
            }
            crate::info!(
                "{}: \"{}\" ({}.{}) service {}",
                peer.label,
                hello.name,
                hello.major,
                hello.minor,
                hello.service
            );
            send_reply(&sock, h.tag, &Reply::with_body(proto::hello_reply_body()))?;
            if let Some(s) = svc.as_mut() {
                s.connected();
            }
            continue;
        };
        let r = if h.ty == proto::HELLO || h.ty >= wire::EVENT_MIN {
            Reply::err(-EPROTO)
        } else {
            s.handle(h.ty, body, pkt.fds)
        };
        send_reply(&sock, h.tag, &r)?;
    }
    Ok(())
}
