//! `SOCK_SEQPACKET` sockets with fd passing (`SCM_RIGHTS`), shared by all
//! services.

use std::io::{IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
use std::path::Path;

use anyhow::{Context, Result, bail};
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketAddrUnix, SocketFlags, SocketType,
};

use crate::ipc::wire::MAX_MSG;

/// Most fds one message may carry, in either direction.
pub const MAX_FDS: usize = 4;

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

/// Accepts one connection and returns it with the peer's credentials (host
/// ids, as seen from the bridge's namespaces).
pub fn accept(listener: &OwnedFd) -> rustix::io::Result<(OwnedFd, libc::ucred)> {
    let sock = rustix::net::accept_with(listener, SocketFlags::CLOEXEC)?;
    // A sibling Flatpak sandbox is outside our PID namespace: Linux reports
    // pid=0. rustix::UCred cannot represent that (its Pid is nonzero).
    let mut cred = MaybeUninit::<libc::ucred>::uninit();
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred and len point to writable storage of the stated size.
    let ret = unsafe {
        libc::getsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            cred.as_mut_ptr().cast(),
            &mut len,
        )
    };
    if ret != 0 {
        return Err(
            rustix::io::Errno::from_io_error(&std::io::Error::last_os_error())
                .expect("getsockopt errno"),
        );
    }
    if len as usize != std::mem::size_of::<libc::ucred>() {
        return Err(rustix::io::Errno::INVAL);
    }
    // SAFETY: getsockopt succeeded and filled the complete ucred.
    let cred = unsafe { cred.assume_init() };
    Ok((sock, cred))
}

/// One received message.
pub struct Packet {
    pub len: usize,
    /// Fds the peer attached (at most MAX_FDS; more ends the connection).
    pub fds: Vec<OwnedFd>,
}

/// Receives one message into `buf`. Returns None on EOF. Oversized packets
/// and too many fds are errors (the caller drops the connection).
pub fn recv(sock: &OwnedFd, buf: &mut [u8; MAX_MSG + 1]) -> Result<Option<Packet>> {
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
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
    let mut fds = Vec::new();
    for m in anc.drain() {
        if let RecvAncillaryMessage::ScmRights(f) = m {
            fds.extend(f);
        }
    }
    if msg.bytes == 0 {
        return Ok(None);
    }
    if msg.flags.contains(ReturnFlags::TRUNC) || msg.bytes > MAX_MSG {
        bail!("oversized packet");
    }
    if msg.flags.contains(ReturnFlags::CTRUNC) || fds.len() > MAX_FDS {
        bail!("too many fds attached");
    }
    Ok(Some(Packet {
        len: msg.bytes,
        fds,
    }))
}

/// Sends one message with up to MAX_FDS fds.
pub fn send(sock: &OwnedFd, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> Result<()> {
    if fds.len() > MAX_FDS {
        bail!("too many fds");
    }
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
    let mut anc = SendAncillaryBuffer::new(&mut space);
    if !fds.is_empty() && !anc.push(SendAncillaryMessage::ScmRights(fds)) {
        bail!("fds don't fit");
    }
    rustix::net::sendmsg(sock, &[IoSlice::new(bytes)], &mut anc, SendFlags::NOSIGNAL)?;
    Ok(())
}

/// Like [`send`], but fails with EAGAIN instead of waiting for a full
/// socket buffer (for events: a slow client must not stall the bridge).
pub fn send_nonblocking(sock: &OwnedFd, bytes: &[u8], fds: &[BorrowedFd<'_>]) -> Result<()> {
    if fds.len() > MAX_FDS {
        bail!("too many fds");
    }
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(MAX_FDS))];
    let mut anc = SendAncillaryBuffer::new(&mut space);
    if !fds.is_empty() && !anc.push(SendAncillaryMessage::ScmRights(fds)) {
        bail!("fds don't fit");
    }
    rustix::net::sendmsg(
        sock,
        &[IoSlice::new(bytes)],
        &mut anc,
        SendFlags::NOSIGNAL | SendFlags::DONTWAIT,
    )?;
    Ok(())
}

/// Sends `fds` owned by the caller (convenience for replies).
pub fn send_owned(sock: &OwnedFd, bytes: &[u8], fds: &[OwnedFd]) -> Result<()> {
    let b: Vec<_> = fds.iter().map(|f| f.as_fd()).collect();
    send(sock, bytes, &b)
}
