//! Opens a window through a running bridge's display service, the way the
//! composer does (a udmabuf pool, BUFFER_ADD, FRAME, RELEASE), animates a test
//! pattern and prints the events (configure, input lines, close).
//!
//!   cargo run -p linuxapk-bridge --example window -- SOCKET [SECONDS]

use std::io::IoSliceMut;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, AsRawFd, OwnedFd};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use linuxapk_bridge::display::proto::{self as d, BufferSpec, Request, WindowSpec, fourcc, ty};
use linuxapk_bridge::ipc::proto::{Hello, service};
use linuxapk_bridge::ipc::wire;
use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvFlags, SendAncillaryBuffer, SendAncillaryMessage,
    SendFlags, SocketAddrUnix, SocketFlags, SocketType,
};

const W: u32 = 640;
const H: u32 = 400;

struct Conn {
    sock: OwnedFd,
    tag: u32,
    released: Vec<u32>,
    closed: bool,
}

impl Conn {
    fn handle_event(&mut self, ty: u16, body: &[u8]) {
        let u32_at = |i: usize| u32::from_le_bytes(body[i..i + 4].try_into().unwrap());
        match ty {
            ty::CONFIGURE => println!(
                "configure window {} {}x{} scale {:.2}",
                u32_at(0),
                u32_at(4),
                u32_at(8),
                u32_at(12) as f64 / 120.0
            ),
            ty::CLOSE => {
                println!("close window {}", u32_at(0));
                self.closed = true;
            }
            ty::RELEASE => self.released.push(u32_at(0)),
            ty::INPUT => {
                let n = u16::from_le_bytes([body[0], body[1]]) as usize;
                println!("input: {}", String::from_utf8_lossy(&body[2..2 + n]));
            }
            t => println!("event {t:#x}"),
        }
    }

    /// Reads one packet; events are handled, a reply is returned.
    fn read(&mut self, timeout: Option<Duration>) -> Result<Option<(u32, i32)>> {
        if let Some(t) = timeout {
            let mut p = [rustix::event::PollFd::new(
                &self.sock,
                rustix::event::PollFlags::IN,
            )];
            let ts = rustix::time::Timespec {
                tv_sec: t.as_secs() as i64,
                tv_nsec: t.subsec_nanos() as i64,
            };
            if rustix::event::poll(&mut p, Some(&ts))? == 0 {
                return Ok(None);
            }
        }
        let mut buf = vec![0u8; 1024];
        let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(4))];
        let mut anc = RecvAncillaryBuffer::new(&mut space);
        let m = rustix::net::recvmsg(
            &self.sock,
            &mut [IoSliceMut::new(&mut buf)],
            &mut anc,
            RecvFlags::CMSG_CLOEXEC,
        )?;
        if m.bytes == 0 {
            bail!("bridge closed the connection");
        }
        let h = wire::decode_header(&buf[..m.bytes]).map_err(|e| anyhow::anyhow!("{e:?}"))?;
        if h.ty == wire::REPLY {
            let (tag, status, _) = wire::decode_reply(&buf[..m.bytes]).context("reply")?;
            return Ok(Some((tag, status)));
        }
        self.handle_event(h.ty, &buf[wire::HDR..m.bytes]);
        Ok(None)
    }

    fn call(&mut self, req: &Request, fd: Option<&OwnedFd>) -> Result<i32> {
        self.tag += 1;
        let msg = d::encode(self.tag, req);
        let mut space = [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut anc = SendAncillaryBuffer::new(&mut space);
        let fds;
        if let Some(f) = fd {
            fds = [f.as_fd()];
            anc.push(SendAncillaryMessage::ScmRights(&fds));
        }
        rustix::net::sendmsg(
            &self.sock,
            &[std::io::IoSlice::new(&msg)],
            &mut anc,
            SendFlags::NOSIGNAL,
        )?;
        loop {
            if let Some((tag, status)) = self.read(None)?
                && tag == self.tag
            {
                return Ok(status);
            }
        }
    }
}

fn udmabuf(size: usize) -> Result<(OwnedFd, *mut u8)> {
    let memfd = rustix::fs::memfd_create("window-example", rustix::fs::MemfdFlags::ALLOW_SEALING)?;
    rustix::fs::ftruncate(&memfd, size as u64)?;
    rustix::fs::fcntl_add_seals(&memfd, rustix::fs::SealFlags::SHRINK)?;
    let map = unsafe {
        rustix::mm::mmap(
            std::ptr::null_mut(),
            size,
            rustix::mm::ProtFlags::READ | rustix::mm::ProtFlags::WRITE,
            rustix::mm::MapFlags::SHARED,
            &memfd,
            0,
        )?
    } as *mut u8;
    let dev = rustix::fs::open(
        "/dev/udmabuf",
        rustix::fs::OFlags::RDWR,
        rustix::fs::Mode::empty(),
    )?;
    let mut c = Create {
        memfd: memfd.as_raw_fd() as u32,
        flags: 1, // UDMABUF_FLAGS_CLOEXEC
        offset: 0,
        size: size as u64,
    };
    let fd = unsafe { rustix::ioctl::ioctl(&dev, UdmabufCreate(&mut c))? };
    Ok((fd, map))
}

/// struct udmabuf_create { u32 memfd; u32 flags; u64 offset; u64 size; }
#[repr(C)]
struct Create {
    memfd: u32,
    flags: u32,
    offset: u64,
    size: u64,
}

/// UDMABUF_CREATE (_IOW('u', 0x42, struct udmabuf_create)); returns the dma-buf.
struct UdmabufCreate<'a>(&'a mut Create);

unsafe impl rustix::ioctl::Ioctl for UdmabufCreate<'_> {
    type Output = OwnedFd;
    const IS_MUTATING: bool = false;
    fn opcode(&self) -> rustix::ioctl::Opcode {
        rustix::ioctl::opcode::write::<Create>(b'u', 0x42)
    }
    fn as_ptr(&mut self) -> *mut std::ffi::c_void {
        (self.0 as *mut Create).cast()
    }
    unsafe fn output_from_ptr(
        out: rustix::ioctl::IoctlOutput,
        _: *mut std::ffi::c_void,
    ) -> rustix::io::Result<OwnedFd> {
        use std::os::fd::FromRawFd;
        Ok(unsafe { OwnedFd::from_raw_fd(out) })
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let path = args.get(1).context("usage: window SOCKET [SECONDS]")?;
    let secs: f64 = args.get(2).map_or(Ok(10.0), |s| s.parse())?;
    let sock = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )?;
    rustix::net::connect(&sock, &SocketAddrUnix::new(path.as_str())?)?;
    let mut c = Conn {
        sock,
        tag: 0,
        released: vec![],
        closed: false,
    };
    let hello = Hello {
        major: 1,
        minor: 0,
        service: service::DISPLAY,
        name: "window example".into(),
    };
    rustix::net::send(&c.sock, &hello.encode(1), SendFlags::NOSIGNAL)?;
    c.tag = 1;
    loop {
        if let Some((_, st)) = c.read(None)? {
            if st != 0 {
                bail!("HELLO: {st}");
            }
            break;
        }
    }
    let st = c.call(
        &Request::WindowCreate(WindowSpec {
            window: 42,
            width: W,
            height: H,
            min_width: 0,
            min_height: 0,
            app_id: "dev.linuxapk.example".into(),
            title: "Bridge window example".into(),
        }),
        None,
    )?;
    println!("WINDOW_CREATE: {st}");
    let stride = W * 4;
    let size = (stride * H) as usize;
    let mut bufs = vec![];
    for i in 0..3u32 {
        let (fd, map) = udmabuf(size)?;
        let st = c.call(
            &Request::BufferAdd(BufferSpec {
                buffer: i,
                window: 42,
                width: W,
                height: H,
                fourcc: fourcc::XBGR8888,
                stride,
                offset: 0,
                modifier: 0,
            }),
            Some(&fd),
        )?;
        println!("BUFFER_ADD {i}: {st}");
        bufs.push((map, true));
    }
    let start = Instant::now();
    let mut n = 0u32;
    while start.elapsed().as_secs_f64() < secs && !c.closed {
        for r in std::mem::take(&mut c.released) {
            bufs[r as usize].1 = true;
        }
        if let Some(i) = bufs.iter().position(|b| b.1) {
            let px = unsafe { std::slice::from_raw_parts_mut(bufs[i].0, size) };
            for y in 0..H {
                for x in 0..W {
                    let o = ((y * W + x) * 4) as usize;
                    px[o] = (x * 255 / W) as u8;
                    px[o + 1] = (y * 255 / H) as u8;
                    px[o + 2] = (n * 4) as u8;
                    px[o + 3] = 255;
                }
            }
            bufs[i].1 = false;
            c.call(
                &Request::Frame {
                    window: 42,
                    buffer: i as u32,
                    damage: [0; 4],
                },
                None,
            )?;
            n += 1;
        }
        let _ = c.read(Some(Duration::from_millis(16)))?;
    }
    println!("{n} frames");
    Ok(())
}
