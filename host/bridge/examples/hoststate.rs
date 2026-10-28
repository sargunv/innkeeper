//! Talks to a running bridge's host state service the way the Health HAL and
//! the Linux.apk System app do, for testing the desktop side without
//! Android:
//!
//!   cargo run -p linuxapk-bridge --example hoststate -- SOCKET [location]
//!
//! Subscribes to the battery, power saver and network (and with `location`,
//! starts location) and prints every event until killed.

use anyhow::{Context, Result, bail};
use linuxapk_bridge::hoststate::proto::{self as p, Battery, Fix, Network, Request, ev, feature};
use linuxapk_bridge::ipc::proto::{Hello, service};
use linuxapk_bridge::ipc::transport;
use linuxapk_bridge::ipc::wire::{self, MAX_MSG};
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        bail!("usage: hoststate SOCKET [location]");
    };
    let sock = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::connect(&sock, &SocketAddrUnix::new(path.as_str())?)?;
    let hello = Hello {
        major: 1,
        minor: 0,
        service: service::HOSTSTATE,
        name: "hoststate example".into(),
    };
    transport::send(&sock, &hello.encode(1), &[])?;
    transport::send(
        &sock,
        &p::encode(
            2,
            &Request::Start {
                features: feature::ALL,
            },
        ),
        &[],
    )?;
    if args.get(1).map(String::as_str) == Some("location") {
        transport::send(
            &sock,
            &p::encode(3, &Request::LocationStart { precise: true }),
            &[],
        )?;
    }
    let mut buf = [0u8; MAX_MSG + 1];
    loop {
        let pkt = transport::recv(&sock, &mut buf)?.context("bridge closed")?;
        let b = &buf[..pkt.len];
        if let Some((tag, status, body)) = wire::decode_reply(b) {
            println!("reply {tag}: {status} {body:02x?}");
            continue;
        }
        let h = wire::decode_header(b)?;
        let body = &b[wire::HDR..];
        match h.ty {
            ev::BATTERY => println!("battery {:?}", Battery::decode(body)),
            ev::POWER_SAVER => println!("power saver {}", body[0]),
            ev::NETWORK => println!("network {:?}", Network::decode(body)),
            ev::LOCATION => println!("location {:?}", Fix::decode(body)),
            ev::LOCATION_STATUS => println!("location status {}", body[0]),
            t => println!("event {t:#06x}"),
        }
    }
}
