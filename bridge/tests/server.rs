//! End-to-end tests of the bridge over a real socket, with the null output:
//! a well-behaved client streams through the ring, and hostile clients
//! (garbage, oversized packets, attached fds, resource exhaustion) are
//! answered or dropped without disturbing anyone else.

use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use linuxapk_bridge::audio::{Backend, Limits};
use linuxapk_bridge::proto::{
    self, OpenOutputReply, OutputConfig, Request, SampleFormat, libc_errno::*,
};
use linuxapk_bridge::server::{self, Config};
use proptest::prelude::*;
use rustix::event::{PollFd, PollFlags};
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, SocketAddrUnix, SocketFlags, SocketType,
};

static N: AtomicU32 = AtomicU32::new(0);

fn start(limits: Limits, allow_self: bool) -> PathBuf {
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "bridge-{}-{}.sock",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let l = server::listen(&path).unwrap();
    let me = rustix::process::getuid().as_raw();
    let cfg = Config {
        allow_uids: if allow_self {
            vec![me]
        } else {
            vec![me.wrapping_add(1)]
        },
        backend: Backend::Null,
        limits,
        max_clients: 4,
    };
    std::thread::spawn(move || server::serve(l, Arc::new(cfg)));
    path
}

struct Client {
    sock: OwnedFd,
    tag: u32,
}

impl Client {
    fn connect(path: &PathBuf) -> Client {
        let sock = rustix::net::socket_with(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        rustix::net::connect(&sock, &SocketAddrUnix::new(path).unwrap()).unwrap();
        Client { sock, tag: 0 }
    }

    fn send_raw(&self, b: &[u8], fds: &[OwnedFd]) -> rustix::io::Result<usize> {
        let borrowed: Vec<_> = fds.iter().map(|f| f.as_fd()).collect();
        let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(8))];
        let mut anc = SendAncillaryBuffer::new(&mut space);
        if !borrowed.is_empty() {
            assert!(anc.push(SendAncillaryMessage::ScmRights(&borrowed)));
        }
        rustix::net::sendmsg(
            &self.sock,
            &[std::io::IoSlice::new(b)],
            &mut anc,
            SendFlags::NOSIGNAL,
        )
    }

    /// Receives one packet: None on EOF or timeout.
    fn recv(&self) -> Option<(Vec<u8>, Vec<OwnedFd>)> {
        let mut pfd = [PollFd::new(&self.sock, PollFlags::IN)];
        let t = rustix::time::Timespec {
            tv_sec: 5,
            tv_nsec: 0,
        };
        if rustix::event::poll(&mut pfd, Some(&t)).unwrap() == 0 {
            return None;
        }
        let mut buf = vec![0u8; 1024];
        let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(8))];
        let mut anc = RecvAncillaryBuffer::new(&mut space);
        let m = rustix::net::recvmsg(
            &self.sock,
            &mut [IoSliceMut::new(&mut buf)],
            &mut anc,
            RecvFlags::CMSG_CLOEXEC,
        )
        .ok()?;
        let mut fds = Vec::new();
        for a in anc.drain() {
            if let RecvAncillaryMessage::ScmRights(f) = a {
                fds.extend(f);
            }
        }
        if m.bytes == 0 {
            return None;
        }
        buf.truncate(m.bytes);
        Some((buf, fds))
    }

    fn call(&mut self, req: &Request) -> Option<(i32, Vec<u8>, Vec<OwnedFd>)> {
        self.tag += 1;
        self.send_raw(&proto::encode(self.tag, req), &[]).ok()?;
        let (b, fds) = self.recv()?;
        let (tag, status, body) = proto::decode_reply(&b).unwrap();
        assert_eq!(tag, self.tag);
        Some((status, body.to_vec(), fds))
    }

    fn hello(&mut self) {
        let r = self
            .call(&Request::Hello {
                major: 1,
                minor: 0,
                service: 1,
                name: "test".into(),
            })
            .unwrap();
        assert_eq!(r.0, 0);
        assert_eq!(r.1, proto::hello_body());
    }
}

fn out_cfg(period: u32, periods: u32) -> Request {
    Request::OpenOutput(OutputConfig {
        rate: 48000,
        channels: 2,
        format: SampleFormat::S16Le,
        period_frames: period,
        periods,
        name: "test".into(),
    })
}

fn wait_readable(fd: &OwnedFd, ms: i64) -> bool {
    let mut pfd = [PollFd::new(fd, PollFlags::IN)];
    let t = rustix::time::Timespec {
        tv_sec: ms / 1000,
        tv_nsec: (ms % 1000) * 1_000_000,
    };
    rustix::event::poll(&mut pfd, Some(&t)).unwrap() > 0
}

#[test]
fn stream_through_ring() {
    let path = start(Limits::default(), true);
    let mut c = Client::connect(&path);
    c.hello();
    let (status, body, fds) = c.call(&out_cfg(480, 4)).unwrap();
    assert_eq!(status, 0);
    let r = OpenOutputReply::decode(&body).unwrap();
    assert_eq!((r.capacity_frames, r.data_offset), (1920, 4096));
    assert_eq!(fds.len(), 3);
    let [memfd, data, space]: [OwnedFd; 3] = fds.try_into().unwrap();
    // SAFETY: test mapping of the ring we were given, of its advertised size.
    let map = unsafe {
        rustix::mm::mmap(
            std::ptr::null_mut(),
            r.map_size as usize,
            rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
            rustix::mm::MapFlags::SHARED,
            &memfd,
            0,
        )
        .unwrap()
    } as *mut u8;
    let hdr = |off: usize| unsafe { &*(map.add(off) as *const std::sync::atomic::AtomicU64) };
    assert_eq!(unsafe { *(map as *const u32) }, 0x5241_584c);
    // The client cannot shrink the ring under the bridge.
    assert!(rustix::fs::ftruncate(&memfd, 0).is_err());

    // Write 100 ms of frames, a period at a time, and watch them drain at
    // roughly real time.
    let t0 = Instant::now();
    let mut written = 0u64;
    while written < 4800 {
        let read = hdr(128).load(Ordering::Acquire);
        if written - read + 480 <= 1920 {
            written += 480;
            hdr(64).store(written, Ordering::Release);
            rustix::io::write(&data, &1u64.to_ne_bytes()).unwrap();
        } else {
            assert!(wait_readable(&space, 1000), "no space signal");
            let mut b = [0u8; 8];
            let _ = rustix::io::read(&space, &mut b);
        }
    }
    while hdr(128).load(Ordering::Acquire) < written {
        assert!(wait_readable(&space, 1000));
        let mut b = [0u8; 8];
        let _ = rustix::io::read(&space, &mut b);
    }
    let el = t0.elapsed();
    assert!(
        el >= Duration::from_millis(60) && el < Duration::from_millis(400),
        "{el:?}"
    );
    // Timing was published.
    assert!(hdr(208).load(Ordering::Acquire) > 0);

    assert_eq!(c.call(&Request::Standby { stream: r.stream }).unwrap().0, 0);
    assert_eq!(
        c.call(&Request::SetVolume {
            stream: r.stream,
            left: 0.5,
            right: 0.5
        })
        .unwrap()
        .0,
        0
    );
    assert_eq!(
        c.call(&Request::SetMaster {
            volume: 1.0,
            muted: true
        })
        .unwrap()
        .0,
        0
    );
    assert_eq!(c.call(&Request::Close { stream: r.stream }).unwrap().0, 0);
    assert_eq!(
        c.call(&Request::Close { stream: r.stream }).unwrap().0,
        -ENOENT
    );
    // The ring is marked dead once the stream is gone.
    assert_eq!(unsafe { *(map.add(224) as *const u32) }, 2);
}

#[test]
fn protocol_errors() {
    let path = start(Limits::default(), true);
    // Requests before HELLO are refused and the connection dropped.
    let mut c = Client::connect(&path);
    assert_eq!(c.call(&Request::Close { stream: 1 }).unwrap().0, -EPROTO);
    assert!(c.recv().is_none());
    // Wrong major, unknown service.
    let mut c = Client::connect(&path);
    assert_eq!(
        c.call(&Request::Hello {
            major: 2,
            minor: 0,
            service: 1,
            name: String::new()
        })
        .unwrap()
        .0,
        -EPROTO
    );
    let mut c = Client::connect(&path);
    assert_eq!(
        c.call(&Request::Hello {
            major: 1,
            minor: 0,
            service: 99,
            name: String::new()
        })
        .unwrap()
        .0,
        -ENOSYS
    );

    let mut c = Client::connect(&path);
    c.hello();
    // Second HELLO, bad values, unknown type, unknown stream: answered.
    assert_eq!(
        c.call(&Request::Hello {
            major: 1,
            minor: 0,
            service: 1,
            name: String::new()
        })
        .unwrap()
        .0,
        -EPROTO
    );
    assert_eq!(c.call(&out_cfg(1, 4)).unwrap().0, -EINVAL);
    assert_eq!(c.call(&Request::Standby { stream: 42 }).unwrap().0, -ENOENT);
    let mut unknown = proto::encode(77, &Request::Close { stream: 0 });
    unknown[0] = 0x55;
    c.send_raw(&unknown, &[]).unwrap();
    let (b, _) = c.recv().unwrap();
    assert_eq!(proto::decode_reply(&b).unwrap().1, -ENOSYS);
    // Attached fds are closed and the request refused.
    let fd = rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC).unwrap();
    c.send_raw(&proto::encode(78, &Request::Standby { stream: 1 }), &[fd])
        .unwrap();
    let (b, fds) = c.recv().unwrap();
    assert!(fds.is_empty());
    assert_eq!(proto::decode_reply(&b).unwrap().1, -EPROTO);
    // Oversized packet: dropped.
    c.send_raw(&[0u8; 2000], &[]).unwrap();
    assert!(c.recv().is_none());

    // The server still serves others.
    let mut d = Client::connect(&path);
    d.hello();
}

#[test]
fn resource_caps() {
    let limits = Limits {
        streams_per_client: 2,
        ring_bytes_per_client: 1 << 20,
        streams_total: 3,
    };
    let path = start(limits, true);
    let mut a = Client::connect(&path);
    a.hello();
    assert_eq!(a.call(&out_cfg(480, 2)).unwrap().0, 0);
    assert_eq!(a.call(&out_cfg(480, 2)).unwrap().0, 0);
    assert_eq!(a.call(&out_cfg(480, 2)).unwrap().0, -EBUSY); // per client
    let mut b = Client::connect(&path);
    b.hello();
    // 16384 * 16 frames * 4 bytes = 1 MiB + header: over the byte budget.
    assert_eq!(b.call(&out_cfg(16384, 16)).unwrap().0, -EBUSY);
    assert_eq!(b.call(&out_cfg(480, 2)).unwrap().0, 0);
    assert_eq!(b.call(&out_cfg(480, 2)).unwrap().0, -EBUSY); // global
    // Closing a's connection frees its streams.
    drop(a);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(b.call(&out_cfg(480, 2)).unwrap().0, 0);
    // Client cap: 4 connections at most.
    let extra: Vec<Client> = (0..4).map(|_| Client::connect(&path)).collect();
    std::thread::sleep(Duration::from_millis(200));
    let mut last = Client::connect(&path);
    assert!(
        last.call(&Request::Hello {
            major: 1,
            minor: 0,
            service: 1,
            name: String::new()
        })
        .is_none()
    );
    drop(extra);
}

#[test]
fn refuses_unlisted_uid() {
    let path = start(Limits::default(), false);
    let mut c = Client::connect(&path);
    assert!(
        c.call(&Request::Hello {
            major: 1,
            minor: 0,
            service: 1,
            name: String::new()
        })
        .is_none()
    );
}

#[test]
fn listen_refuses_to_replace_files() {
    let p = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("not-a-socket-{}", std::process::id()));
    std::fs::write(&p, b"keep me").unwrap();
    assert!(server::listen(&p).is_err());
    assert_eq!(std::fs::read(&p).unwrap(), b"keep me");
    std::fs::remove_file(&p).unwrap();
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    // Random packets on a live, authenticated connection: every one is
    // answered or ends the connection, and the server keeps serving.
    #[test]
    fn fuzz_live_connection(pkts in proptest::collection::vec(proptest::collection::vec(any::<u8>(), 0..80), 1..12)) {
        static PATH: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        let path = PATH.get_or_init(|| start(Limits::default(), true));
        let mut c = Client::connect(path);
        c.hello();
        for mut p in pkts {
            // Keep the header plausible half the time so bodies get exercised.
            if p.len() >= 8 && p[0] & 1 == 0 {
                p[1] = 1;
                p[2] = 0;
                p[3] = 0;
            }
            if p.is_empty() || c.send_raw(&p, &[]).is_err() || c.recv().is_none() {
                break;
            }
        }
        let mut d = Client::connect(path);
        d.hello();
    }
}
