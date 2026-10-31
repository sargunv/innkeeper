//! Talks to a running bridge's system service the way the Innkeeper System
//! app does, for testing the desktop side without Android:
//!
//!   cargo run -p innkeeper-bridge --example system -- SOCKET listen
//!   ... SOCKET notify TITLE BODY      (with an action and an inline reply)
//!   ... SOCKET clip TEXT
//!   ... SOCKET open URL
//!   ... SOCKET media TITLE ARTIST     (a player that obeys play/pause)
//!
//! Every command then prints the events it gets for `INNKEEPER_WAIT` seconds
//! (default 30; `listen` runs until killed).

use std::os::fd::{AsFd, OwnedFd};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use innkeeper_bridge::ipc::blob::{self, Image};
use innkeeper_bridge::ipc::proto::{Hello, service};
use innkeeper_bridge::ipc::transport;
use innkeeper_bridge::ipc::wire::{self, MAX_MSG, Reader};
use innkeeper_bridge::system::mpris::{Media, actions, state};
use innkeeper_bridge::system::notify::{Action, Note, flags};
use innkeeper_bridge::system::proto::{
    self as p, ClipKind, Request, Settings, ev, feature, media_cmd,
};
use rustix::event::{PollFd, PollFlags};
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};

struct Client {
    sock: OwnedFd,
    tag: u32,
}

impl Client {
    fn send(&mut self, msg: impl FnOnce(u32) -> Vec<u8>, fd: Option<&OwnedFd>) -> Result<()> {
        self.tag += 1;
        let fds: Vec<_> = fd.iter().map(|f| f.as_fd()).collect();
        transport::send(&self.sock, &msg(self.tag), &fds)
    }

    /// Next packet within `ms`; None on timeout.
    fn recv(&self, ms: i64) -> Result<Option<(Vec<u8>, Vec<OwnedFd>)>> {
        let mut pfd = [PollFd::new(&self.sock, PollFlags::IN)];
        let t = rustix::time::Timespec {
            tv_sec: ms / 1000,
            tv_nsec: (ms % 1000) * 1_000_000,
        };
        if rustix::event::poll(&mut pfd, Some(&t))? == 0 {
            return Ok(None);
        }
        let mut buf = [0u8; MAX_MSG + 1];
        let p = transport::recv(&self.sock, &mut buf)?.context("bridge closed")?;
        Ok(Some((buf[..p.len].to_vec(), p.fds)))
    }

    /// Sends a request and waits for its reply, printing events meanwhile.
    fn call(&mut self, req: &Request, fd: Option<&OwnedFd>) -> Result<(i32, Vec<u8>)> {
        self.send(|t| p::encode(t, req), fd)?;
        loop {
            let (b, fds) = self.recv(10_000)?.context("no reply")?;
            if let Some((tag, st, body)) = wire::decode_reply(&b)
                && tag == self.tag
            {
                return Ok((st, body.to_vec()));
            }
            self.event(&b, fds)?;
        }
    }

    fn event(&mut self, b: &[u8], fds: Vec<OwnedFd>) -> Result<Option<(u8, u32, i64)>> {
        let h = wire::decode_header(b)?;
        let body = &b[wire::HDR..];
        let mut r = Reader::new(body);
        match h.ty {
            ev::SETTINGS => println!("SETTINGS {:?}", Settings::decode(body)?),
            ev::CLIP => {
                let (kind, sensitive, _, len) = (r.u8()?, r.u8()?, r.u16()?, r.u32()?);
                let data = blob::read(fds.first().context("no fd")?, len, 16 << 20)
                    .map_err(|e| anyhow::anyhow!("blob {e}"))?;
                if kind == 1 {
                    println!(
                        "CLIP text sensitive={sensitive} {:?}",
                        String::from_utf8_lossy(&data)
                    );
                } else {
                    println!(
                        "CLIP kind={kind} sensitive={sensitive} {} bytes",
                        data.len()
                    );
                }
            }
            ev::NOTIFY_EVENT => {
                let (id, what, action, _, len) = (r.u32()?, r.u8()?, r.u8()?, r.u16()?, r.u32()?);
                let text = match fds.first() {
                    Some(fd) => String::from_utf8_lossy(
                        &blob::read(fd, len, 1 << 16).map_err(|e| anyhow::anyhow!("blob {e}"))?,
                    )
                    .into_owned(),
                    None => String::new(),
                };
                println!("NOTIFY_EVENT id={id} what={what} action={action} text={text:?}");
            }
            ev::MEDIA_CMD => {
                let (player, cmd, _, _, arg) = (r.u32()?, r.u8()?, r.u8()?, r.u16()?, r.u64()?);
                println!("MEDIA_CMD player={player} cmd={cmd} arg={}", arg as i64);
                return Ok(Some((cmd, player, arg as i64)));
            }
            t => println!("event {t:#06x} ({} bytes)", body.len()),
        }
        Ok(None)
    }
}

fn memfd(bytes: &[u8]) -> Result<OwnedFd> {
    blob::create("example", bytes)
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (path, cmd) = match &args[..] {
        [p, c, ..] => (p, c.as_str()),
        _ => bail!("usage: system SOCKET listen|notify|clip|open|media ..."),
    };
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let sock = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::connect(&sock, &SocketAddrUnix::new(path.as_str())?)?;
    let mut c = Client { sock, tag: 0 };
    let hello = Hello {
        major: 1,
        minor: 0,
        service: service::SYSTEM,
        name: "system example".into(),
    };
    c.send(|t| hello.encode(t), None)?;
    let (b, _) = c.recv(5000)?.context("no HELLO reply")?;
    let (_, st, _) = wire::decode_reply(&b).context("bad reply")?;
    anyhow::ensure!(st == 0, "HELLO: {st}");
    let (st, body) = c.call(
        &Request::Start {
            features: feature::ALL,
        },
        None,
    )?;
    println!(
        "START: status {st}, features {:#x}",
        Reader::new(&body).u32()?
    );

    let mut media: Option<Media> = None;
    match cmd {
        "listen" => {}
        "notify" => {
            let note = Note {
                package: "dev.sargunv.innkeeper.example".into(),
                app_name: "Innkeeper example".into(),
                title: arg(2),
                body: arg(3),
                flags: flags::CLICKABLE,
                urgency: 1,
                progress: -1,
                actions: vec![
                    Action {
                        label: "Mark as read".into(),
                        reply: false,
                        placeholder: String::new(),
                    },
                    Action {
                        label: "Reply".into(),
                        reply: true,
                        placeholder: "Write a reply".into(),
                    },
                ],
                icon: Some(Image {
                    width: 32,
                    height: 32,
                    rgba: [0x3d, 0xdc, 0x84, 0xff].repeat(32 * 32),
                }),
                picture: None,
            };
            let b = note.encode();
            let st = c.call(
                &Request::Notify {
                    id: 1,
                    len: b.len() as u32,
                },
                Some(&memfd(&b)?),
            )?;
            println!("NOTIFY: {}", st.0);
        }
        "clip" => {
            let t = arg(2);
            let st = c.call(
                &Request::ClipSet {
                    kind: ClipKind::Text,
                    sensitive: false,
                    len: t.len() as u32,
                },
                Some(&memfd(t.as_bytes())?),
            )?;
            println!("CLIP_SET: {}", st.0);
        }
        "open" => {
            let u = arg(2);
            let st = c.call(
                &Request::OpenUri {
                    len: u.len() as u32,
                },
                Some(&memfd(u.as_bytes())?),
            )?;
            println!("OPEN_URI: {}", st.0);
        }
        "media" => {
            let m = Media {
                package: "dev.sargunv.innkeeper.example".into(),
                app_name: "Innkeeper example".into(),
                title: arg(2),
                artist: arg(3),
                album: "Examples".into(),
                state: state::PLAYING,
                actions: actions::ALL,
                duration_us: 200_000_000,
                position_us: 0,
                rate: 1.0,
                art: Some(Image {
                    width: 64,
                    height: 64,
                    rgba: [0xe9, 0x54, 0x20, 0xff].repeat(64 * 64),
                }),
            };
            let b = m.encode();
            let st = c.call(
                &Request::Media {
                    player: 1,
                    len: b.len() as u32,
                },
                Some(&memfd(&b)?),
            )?;
            println!("MEDIA: {}", st.0);
            media = Some(m);
        }
        _ => bail!("unknown command {cmd}"),
    }

    let wait: u64 = std::env::var("INNKEEPER_WAIT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(30);
    let end =
        Instant::now() + Duration::from_secs(if cmd == "listen" { u64::MAX / 4 } else { wait });
    while Instant::now() < end {
        let Some((b, fds)) = c.recv(500)? else {
            continue;
        };
        if wire::decode_reply(&b).is_some() {
            continue;
        }
        if let Some((cmd, player, _)) = c.event(&b, fds)?
            && let Some(m) = media.as_mut()
        {
            m.state = match cmd {
                media_cmd::PLAY => state::PLAYING,
                media_cmd::PAUSE | media_cmd::STOP => state::PAUSED,
                media_cmd::PLAY_PAUSE if m.state == state::PLAYING => state::PAUSED,
                media_cmd::PLAY_PAUSE => state::PLAYING,
                _ => m.state,
            };
            println!(
                "now {}",
                if m.state == state::PLAYING {
                    "playing"
                } else {
                    "paused"
                }
            );
            let b = m.encode();
            c.send(
                |t| {
                    p::encode(
                        t,
                        &Request::Media {
                            player,
                            len: b.len() as u32,
                        },
                    )
                },
                Some(&memfd(&b)?),
            )?;
        }
    }
    Ok(())
}
