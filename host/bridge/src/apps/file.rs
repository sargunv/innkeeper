//! CLI client for a read-only documents grant, using the bridge's fd transport.

use std::fs::File;
use std::os::fd::{AsFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

use crate::documents::proto::{self as documents, Request};
use crate::ipc::{proto, transport, wire};

/// The CLI is already in the Flatpak. Only this fd (never the file's host path)
/// is imported into the running session's bridge.
pub fn grant(socket: &Path, file: &File, name: &str) -> Result<u64> {
    ensure!(
        crate::documents::proto::valid_name(name),
        "invalid file name"
    );
    let sock = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::sockopt::set_socket_timeout(
        &sock,
        rustix::net::sockopt::Timeout::Recv,
        Some(Duration::from_secs(10)),
    )?;
    rustix::net::sockopt::set_socket_timeout(
        &sock,
        rustix::net::sockopt::Timeout::Send,
        Some(Duration::from_secs(10)),
    )?;
    rustix::net::connect(&sock, &SocketAddrUnix::new(socket)?)?;
    let hello = proto::Hello {
        major: proto::MAJOR,
        minor: proto::MINOR,
        service: proto::service::DOCUMENTS,
        name: "linuxapk Open With".into(),
    };
    transport::send(&sock, &hello.encode(1), &[])?;
    reply(&sock, 1)?;
    transport::send(
        &sock,
        &documents::encode(2, &Request::Import { name: name.into() }),
        &[file.as_fd()],
    )?;
    let body = reply(&sock, 2)?;
    let mut r = wire::Reader::new(&body);
    let grant = r.u64()?;
    r.end()?;
    Ok(grant)
}

fn reply(sock: &OwnedFd, tag: u32) -> Result<Vec<u8>> {
    let mut buf = [0; wire::MAX_MSG + 1];
    let packet = transport::recv(sock, &mut buf)?.context("bridge closed the connection")?;
    let h = wire::decode_header(&buf[..packet.len])?;
    ensure!(
        h.ty == wire::REPLY && h.tag == tag && packet.fds.is_empty(),
        "unexpected bridge reply"
    );
    let mut r = wire::Reader::new(&buf[wire::HDR..packet.len]);
    let status = r.i32()?;
    ensure!(
        status == 0,
        "file grant failed: {}",
        std::io::Error::from_raw_os_error(-status)
    );
    Ok(r.take(r.remaining())?.to_vec())
}
