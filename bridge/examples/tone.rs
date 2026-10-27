//! Plays a sine through a running bridge, the way the Android client does
//! (ring + eventfds), and reports the bridge's timing estimates.
//!
//!   cargo run -p linuxapk-bridge --example tone -- SOCKET [SECONDS] [HZ]

use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use linuxapk_bridge::audio::proto::{
    self as audio, OpenOutputReply, OutputConfig, Request, SampleFormat,
};
use linuxapk_bridge::proto::{Hello, service};
use linuxapk_bridge::wire;
use rustix::event::{PollFd, PollFlags};
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendFlags, SocketAddrUnix,
    SocketFlags, SocketType,
};

fn call_raw(sock: &OwnedFd, msg: &[u8]) -> Result<(i32, Vec<u8>, Vec<OwnedFd>)> {
    rustix::net::send(sock, msg, SendFlags::NOSIGNAL)?;
    let mut buf = vec![0u8; 1024];
    let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(4))];
    let mut anc = RecvAncillaryBuffer::new(&mut space);
    let m = rustix::net::recvmsg(
        sock,
        &mut [IoSliceMut::new(&mut buf)],
        &mut anc,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    let mut fds = Vec::new();
    for a in anc.drain() {
        if let RecvAncillaryMessage::ScmRights(f) = a {
            fds.extend(f);
        }
    }
    let (_, status, body) = wire::decode_reply(&buf[..m.bytes]).context("bad reply")?;
    Ok((status, body.to_vec(), fds))
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).context("usage: tone SOCKET [SECONDS] [HZ]")?;
    let secs: f64 = args.get(2).map_or(Ok(3.0), |s| s.parse())?;
    let hz: f64 = args.get(3).map_or(Ok(440.0), |s| s.parse())?;
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
        service: service::AUDIO,
        name: "tone example".into(),
    };
    let (st, _, _) = call_raw(&sock, &hello.encode(1))?;
    if st != 0 {
        bail!("hello: {st}");
    }
    let (rate, period, periods) = (48000u32, 240u32, 4u32);
    let (st, body, fds) = call_raw(
        &sock,
        &audio::encode(
            2,
            &Request::OpenOutput(OutputConfig {
                rate,
                channels: 2,
                format: SampleFormat::S16Le,
                period_frames: period,
                periods,
                name: format!("{hz} Hz tone"),
            }),
        ),
    )?;
    if st != 0 {
        bail!("open: {st}");
    }
    let r = OpenOutputReply::decode(&body).context("open reply")?;
    let [memfd, data, space]: [OwnedFd; 3] = fds
        .try_into()
        .map_err(|_| anyhow::anyhow!("expected 3 fds"))?;
    // SAFETY: mapping the ring we were given, of its advertised size.
    let map = unsafe {
        rustix::mm::mmap(
            std::ptr::null_mut(),
            r.map_size as usize,
            rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
            rustix::mm::MapFlags::SHARED,
            &memfd,
            0,
        )?
    } as *mut u8;
    // SAFETY: header fields are 8-aligned inside the first page.
    let hdr = |off: usize| unsafe { &*(map.add(off) as *const AtomicU64) };
    let cap = r.capacity_frames as u64;
    let total = (secs * rate as f64) as u64;
    let mut w = 0u64;
    let mut lat_samples = Vec::new();
    let mut last_report = 0u64;
    while w < total {
        let rd = hdr(128).load(Ordering::Acquire);
        let free = cap - (w - rd);
        if free >= period as u64 {
            for i in 0..period as u64 {
                let t = (w + i) as f64 / rate as f64;
                let v = ((t * hz * std::f64::consts::TAU).sin() * 0.5 * i16::MAX as f64) as i16;
                let idx = ((w + i) % cap) as usize;
                // SAFETY: idx < capacity; 4-byte frames in the data area.
                unsafe {
                    let p = map.add(r.data_offset as usize + idx * 4) as *mut i16;
                    *p = v;
                    *p.add(1) = v;
                }
            }
            w += period as u64;
            hdr(64).store(w, Ordering::Release);
            rustix::io::write(&data, &1u64.to_ne_bytes())?;
        } else {
            let mut pfd = [PollFd::new(&space, PollFlags::IN)];
            let t = rustix::time::Timespec {
                tv_sec: 1,
                tv_nsec: 0,
            };
            if rustix::event::poll(&mut pfd, Some(&t))? == 0 {
                bail!("bridge stopped consuming");
            }
            let mut b = [0u8; 8];
            let _ = rustix::io::read(&space, &mut b);
        }
        // Output latency as an app would compute it: frames written minus
        // frames presented, plus the time since that estimate.
        let played = hdr(200).load(Ordering::Acquire);
        let ts = hdr(208).load(Ordering::Acquire);
        if ts != 0 && w - last_report >= rate as u64 / 10 {
            last_report = w;
            let now = linuxapk_bridge::audio::stream::monotonic_ns();
            let presented_now = played as f64 + (now.saturating_sub(ts)) as f64 * rate as f64 / 1e9;
            let ms = (w as f64 - presented_now) * 1000.0 / rate as f64;
            lat_samples.push(ms);
        }
    }
    lat_samples.sort_by(f64::total_cmp);
    if !lat_samples.is_empty() {
        let n = lat_samples.len();
        println!(
            "output latency (written - presented): median {:.1} ms, p10 {:.1}, p90 {:.1} ({} samples); ring {} ms, bridge nominal {} ms",
            lat_samples[n / 2],
            lat_samples[n / 10],
            lat_samples[n * 9 / 10],
            n,
            cap * 1000 / rate as u64,
            r.latency_us / 1000
        );
    }
    let (st, _, _) = call_raw(
        &sock,
        &audio::encode(3, &Request::Close { stream: r.stream }),
    )?;
    println!(
        "played {:.1} s at {hz} Hz (close: {st})",
        w as f64 / rate as f64
    );
    Ok(())
}
