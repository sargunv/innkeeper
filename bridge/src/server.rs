//! The listening socket and per-connection threads.
//!
//! Each connection is checked with `SO_PEERCRED` against an allow-list of
//! host uids, must start with HELLO naming one service, and is then handed
//! to that service's handler. Further services (notifications, portals,
//! MPRIS, clipboard) plug in as new `service::*` ids in `open_service`.

use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, Result, bail};
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketAddrUnix, SocketFlags, SocketType,
};

use crate::audio::{AudioService, Backend, Limits};
use crate::proto::{self, MAX_MSG, Request, libc_errno::*, service};

/// A reply to one request: status (0 or -errno), body, fds to pass.
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
}

/// A service bound to one connection.
pub trait Service: Send {
    fn handle(&mut self, req: Request) -> Reply;
}

#[derive(Debug, Clone)]
pub struct Config {
    /// Host uids allowed to connect (SO_PEERCRED).
    pub allow_uids: Vec<u32>,
    pub backend: Backend,
    pub limits: Limits,
    pub max_clients: usize,
}

/// Creates the listening socket at `path`, mode 0666: access control is the
/// directory it lives in (the session launcher makes that root:audio 0750
/// inside the session) plus the uid allow-list. An existing socket file
/// there is replaced, so a restarted bridge takes over; any other file is an
/// error.
pub fn listen(path: &Path) -> Result<OwnedFd> {
    use std::os::unix::fs::FileTypeExt;
    if let Ok(m) = std::fs::symlink_metadata(path) {
        if !m.file_type().is_socket() {
            bail!("{} exists and is not a socket", path.display());
        }
        std::fs::remove_file(path)?;
    }
    let fd = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )?;
    let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o111));
    let r = rustix::net::bind(&fd, &SocketAddrUnix::new(path)?);
    rustix::process::umask(old);
    r.with_context(|| format!("bind {}", path.display()))?;
    rustix::net::listen(&fd, 16)?;
    Ok(fd)
}

/// Accept loop; never returns unless accept fails hard.
pub fn serve(listener: OwnedFd, cfg: Arc<Config>) -> Result<()> {
    let clients = Arc::new(AtomicUsize::new(0));
    let streams = Arc::new(AtomicUsize::new(0));
    loop {
        let sock = match rustix::net::accept_with(&listener, SocketFlags::CLOEXEC) {
            Ok(s) => s,
            Err(rustix::io::Errno::INTR | rustix::io::Errno::CONNABORTED) => continue,
            Err(e) => return Err(e).context("accept"),
        };
        let cred = match rustix::net::sockopt::socket_peercred(&sock) {
            Ok(c) => c,
            Err(e) => {
                crate::warn!("peer credentials: {e}");
                continue;
            }
        };
        let uid = cred.uid.as_raw();
        if !cfg.allow_uids.contains(&uid) {
            crate::warn!(
                "refused connection from uid {uid} (pid {})",
                cred.pid.as_raw_nonzero()
            );
            continue;
        }
        if clients.fetch_add(1, Ordering::AcqRel) >= cfg.max_clients {
            clients.fetch_sub(1, Ordering::AcqRel);
            crate::warn!(
                "refused connection from pid {}: too many clients",
                cred.pid.as_raw_nonzero()
            );
            continue;
        }
        let peer = format!("pid {} uid {uid}", cred.pid.as_raw_nonzero());
        let (cfg, clients, streams) = (cfg.clone(), clients.clone(), streams.clone());
        let spawned = std::thread::Builder::new()
            .name("client".into())
            .spawn(move || {
                if let Err(e) = client(sock, &cfg, &peer, streams) {
                    crate::info!("{peer}: {e:#}");
                }
                clients.fetch_sub(1, Ordering::AcqRel);
            });
        if let Err(e) = spawned {
            crate::warn!("spawn client thread: {e}");
        }
    }
}

/// Receives one packet. Returns None on EOF. Any fds the peer attached are
/// closed (no request in v1 carries fds); `had_fds` reports them.
fn recv_packet(sock: &OwnedFd, buf: &mut [u8; MAX_MSG + 1]) -> Result<Option<(usize, bool)>> {
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(4))];
    let mut anc = RecvAncillaryBuffer::new(&mut space);
    let msg = loop {
        match rustix::net::recvmsg(
            sock,
            &mut [IoSliceMut::new(buf)],
            &mut anc,
            RecvFlags::CMSG_CLOEXEC,
        ) {
            Err(rustix::io::Errno::INTR) => continue,
            r => break r?,
        }
    };
    let mut had_fds = false;
    for m in anc.drain() {
        if let RecvAncillaryMessage::ScmRights(fds) = m {
            for fd in fds {
                had_fds = true;
                drop(fd);
            }
        }
    }
    if msg.bytes == 0 {
        return Ok(None);
    }
    if msg.flags.contains(ReturnFlags::TRUNC) || msg.bytes > MAX_MSG {
        bail!("oversized packet");
    }
    if msg.flags.contains(ReturnFlags::CTRUNC) {
        bail!("too many fds attached");
    }
    Ok(Some((msg.bytes, had_fds)))
}

pub fn send_reply(sock: &OwnedFd, tag: u32, r: &Reply) -> Result<()> {
    let bytes = proto::reply(tag, r.status, &r.body);
    let borrowed: Vec<_> = r.fds.iter().map(|f| f.as_fd()).collect();
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(4))];
    let mut anc = SendAncillaryBuffer::new(&mut space);
    if !borrowed.is_empty() && !anc.push(SendAncillaryMessage::ScmRights(&borrowed)) {
        bail!("too many fds in reply");
    }
    rustix::net::sendmsg(
        sock,
        &[std::io::IoSlice::new(&bytes)],
        &mut anc,
        SendFlags::NOSIGNAL,
    )?;
    Ok(())
}

fn open_service(
    id: u32,
    cfg: &Config,
    peer: &str,
    streams: Arc<AtomicUsize>,
) -> Option<Box<dyn Service>> {
    match id {
        service::AUDIO => Some(Box::new(AudioService::new(
            cfg.backend.clone(),
            cfg.limits.clone(),
            streams,
            peer.to_owned(),
        ))),
        _ => None,
    }
}

fn client(sock: OwnedFd, cfg: &Config, peer: &str, streams: Arc<AtomicUsize>) -> Result<()> {
    let mut buf = [0u8; MAX_MSG + 1];
    let mut svc: Option<Box<dyn Service>> = None;
    while let Some((n, had_fds)) = recv_packet(&sock, &mut buf)? {
        let (tag, req) = match proto::decode(&buf[..n]) {
            Ok(v) => v,
            Err((Some(tag), e)) => {
                crate::debug!("{peer}: bad request: {e:?}");
                send_reply(&sock, tag, &Reply::err(e.status()))?;
                continue;
            }
            Err((None, e)) => bail!("unreadable header: {e:?}"),
        };
        if had_fds {
            send_reply(&sock, tag, &Reply::err(-EPROTO))?;
            continue;
        }
        match (&mut svc, req) {
            (
                None,
                Request::Hello {
                    major,
                    minor,
                    service,
                    name,
                },
            ) => {
                if major != proto::MAJOR {
                    send_reply(&sock, tag, &Reply::err(-EPROTO))?;
                    bail!("protocol {major}.{minor} not supported");
                }
                let Some(s) = open_service(service, cfg, peer, streams.clone()) else {
                    send_reply(&sock, tag, &Reply::err(-ENOSYS))?;
                    bail!("unknown service {service}");
                };
                crate::info!("{peer}: \"{name}\" ({major}.{minor}) service {service}");
                svc = Some(s);
                send_reply(
                    &sock,
                    tag,
                    &Reply {
                        status: 0,
                        body: proto::hello_body(),
                        fds: Vec::new(),
                    },
                )?;
            }
            (None, _) => {
                send_reply(&sock, tag, &Reply::err(-EPROTO))?;
                bail!("request before HELLO");
            }
            (Some(_), Request::Hello { .. }) => send_reply(&sock, tag, &Reply::err(-EPROTO))?,
            (Some(s), req) => {
                let r = s.handle(req);
                send_reply(&sock, tag, &r)?;
            }
        }
    }
    Ok(())
}
