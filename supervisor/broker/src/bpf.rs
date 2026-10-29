//! Userspace eBPF objects and the rootless syscall emulation netd needs.
//! Objects live for the session. Pins name them in the supervisor's original
//! bpffs directory; maps share data and an inode, with independent opens for
//! OFD locking. Programs and attachments succeed without affecting traffic.

use crate::notify::{self, MemFd, SeccompNotif, SeccompNotifAddfd};
use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

type Result<T> = std::result::Result<T, i32>;
const MAX_OBJS: usize = 4096;
const MAP: usize = 0;
const PROG: usize = 1;
const BTF: usize = 2;
const LINK: usize = 3;
const KINDS: [&str; 4] = ["map", "prog", "btf", "link"];

// bpf_attr's x86_64 UAPI is a union: access by byte offset keeps each command's
// overlapping fields explicit, without reading uninitialized union members.
struct Attr([u8; 168]);
impl Attr {
    fn u32(&self, off: usize) -> u32 {
        u32::from_ne_bytes(self.0[off..off + 4].try_into().unwrap())
    }
    fn u64(&self, off: usize) -> u64 {
        u64::from_ne_bytes(self.0[off..off + 8].try_into().unwrap())
    }
}
fn put(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_ne_bytes());
}
fn errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}
fn fd_result(fd: i32) -> Result<OwnedFd> {
    if fd < 0 {
        Err(errno())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}
fn rd(mem: MemFd, addr: u64, len: usize) -> Result<Vec<u8>> {
    notify::mem_read(mem, addr, len).ok_or(libc::EFAULT)
}
fn wr(mem: MemFd, addr: u64, buf: &[u8]) -> Result<()> {
    if notify::mem_write(mem, addr, buf) {
        Ok(())
    } else {
        Err(libc::EFAULT)
    }
}
fn string(mem: MemFd, addr: u64) -> Result<String> {
    let bytes = notify::mem_read_path(mem, addr).ok_or(libc::EFAULT)?;
    String::from_utf8(bytes).map_err(|_| libc::EINVAL)
}
fn is_array(t: u32) -> bool {
    matches!(t, 2 | 3 | 4 | 6 | 8 | 12 | 14 | 15 | 16 | 17)
}
fn is_percpu(t: u32) -> bool {
    matches!(t, 5 | 6 | 10 | 21)
}

struct Object {
    kind: usize,
    id: u32,
    fd: OwnedFd,
    ty: u32,
    key_size: usize,
    value_size: usize,
    max: usize,
    flags: u32,
    stride: usize,
    name: [u8; 16],
    keys: Vec<u8>,
    vals: Vec<u8>,
    used: Vec<bool>,
    count: usize,
    bytes: usize,
    insns: u32,
}
impl Object {
    fn find(&self, key: &[u8], lpm: bool) -> Option<usize> {
        let mut best = None;
        let mut best_len = 0;
        for i in 0..self.max {
            if !self.used[i] {
                continue;
            }
            let k = &self.keys[i * self.key_size..(i + 1) * self.key_size];
            if !lpm {
                if k == key {
                    return Some(i);
                }
                continue;
            }
            let plen = u32::from_ne_bytes(k[..4].try_into().unwrap());
            let qlen = u32::from_ne_bytes(key[..4].try_into().unwrap());
            if plen > qlen || (best.is_some() && plen < best_len) {
                continue;
            }
            let full = (plen / 8) as usize;
            let rem = plen % 8;
            if k[4..4 + full] != key[4..4 + full] {
                continue;
            }
            if rem != 0 && (k[4 + full] ^ key[4 + full]) & (0xff << (8 - rem)) != 0 {
                continue;
            }
            best = Some(i);
            best_len = plen;
        }
        best
    }
    fn index(&self, key: &[u8], lookup: bool) -> Option<usize> {
        if is_array(self.ty) {
            let i = u32::from_ne_bytes(key.get(..4)?.try_into().ok()?) as usize;
            (i < self.max).then_some(i)
        } else {
            self.find(key, lookup && self.ty == 11)
        }
    }
    fn update(&mut self, key: &[u8], val: &[u8], flags: u64) -> Result<()> {
        if is_array(self.ty) {
            if flags == 1 {
                return Err(libc::EEXIST);
            }
            let i = self.index(key, false).ok_or(libc::E2BIG)?;
            self.vals[i * self.stride..(i + 1) * self.stride].copy_from_slice(val);
            return Ok(());
        }
        if self.ty == 11
            && u32::from_ne_bytes(key[..4].try_into().unwrap()) as usize > (self.key_size - 4) * 8
        {
            return Err(libc::EINVAL);
        }
        if let Some(i) = self.find(key, false) {
            if flags & 3 == 1 {
                return Err(libc::EEXIST);
            }
            self.vals[i * self.stride..(i + 1) * self.stride].copy_from_slice(val);
            return Ok(());
        }
        if flags & 3 == 2 {
            return Err(libc::ENOENT);
        }
        if self.count == self.max {
            if !matches!(self.ty, 9 | 10) {
                return Err(libc::E2BIG);
            }
            if let Some(i) = self.used.iter().position(|&b| b) {
                self.used[i] = false;
                self.count -= 1;
            }
        }
        let i = self.used.iter().position(|&b| !b).ok_or(libc::E2BIG)?;
        self.used[i] = true;
        self.count += 1;
        self.keys[i * self.key_size..(i + 1) * self.key_size].copy_from_slice(key);
        self.vals[i * self.stride..(i + 1) * self.stride].copy_from_slice(val);
        Ok(())
    }
    fn delete(&mut self, key: &[u8]) -> Result<()> {
        if is_array(self.ty) {
            return Err(libc::EINVAL);
        }
        let i = self.find(key, false).ok_or(libc::ENOENT)?;
        self.used[i] = false;
        self.count -= 1;
        Ok(())
    }
    fn next(&self, key: Option<&[u8]>) -> Result<Vec<u8>> {
        if is_array(self.ty) {
            let prev = key
                .and_then(|k| k.get(..4))
                .map(|k| u32::from_ne_bytes(k.try_into().unwrap()) as usize);
            let i = match prev {
                Some(i) if i < self.max => i + 1,
                _ => 0,
            };
            if i >= self.max {
                return Err(libc::ENOENT);
            }
            let mut next = vec![0; self.key_size];
            let bytes = (i as u32).to_ne_bytes();
            let n = next.len().min(4);
            next[..n].copy_from_slice(&bytes[..n]);
            return Ok(next);
        }
        let start = key.and_then(|k| self.find(k, false)).map_or(0, |i| i + 1);
        let i = (start..self.max)
            .find(|&i| self.used[i])
            .ok_or(libc::ENOENT)?;
        Ok(self.keys[i * self.key_size..(i + 1) * self.key_size].to_vec())
    }
}

enum Reply {
    Value(i64),
    Continue,
    Injected,
}

pub(crate) struct Emulation {
    listener: RawFd,
    resp_size: usize,
    pins: Option<OwnedFd>,
    objects: [Vec<Object>; 4],
    ncpus: usize,
    bytes: usize,
    attached: [u32; 128],
}
impl Emulation {
    pub(crate) fn new(listener: RawFd, resp_size: usize, pins: Option<OwnedFd>) -> Self {
        Self {
            listener,
            resp_size,
            pins,
            objects: std::array::from_fn(|_| Vec::new()),
            ncpus: unsafe { libc::sysconf(libc::_SC_NPROCESSORS_CONF) }.max(1) as usize,
            bytes: 0,
            attached: [0; 128],
        }
    }
    pub(crate) fn serve(&mut self, req: &SeccompNotif) {
        let r = self.handle(req);
        let (val, error, cont) = match r {
            Ok(Reply::Injected) => return,
            Ok(Reply::Continue) => (0, 0, true),
            Ok(Reply::Value(v)) => (v, 0, false),
            Err(e) => (0, -e, false),
        };
        notify::send_resp(self.listener, self.resp_size, req.id, val, error, cont);
    }
    fn new_obj(&mut self, kind: usize) -> Result<usize> {
        if self.objects.iter().map(Vec::len).sum::<usize>() >= MAX_OBJS {
            return Err(libc::ENOMEM);
        }
        let id = self.objects[kind].len() as u32 + 1;
        let name = CString::new(format!("lxbpf-{}-{id}", KINDS[kind])).unwrap();
        let fd = fd_result(unsafe { libc::memfd_create(name.as_ptr(), libc::MFD_CLOEXEC) })?;
        self.objects[kind].push(Object {
            kind,
            id,
            fd,
            ty: 0,
            key_size: 0,
            value_size: 0,
            max: 0,
            flags: 0,
            stride: 0,
            name: [0; 16],
            keys: Vec::new(),
            vals: Vec::new(),
            used: Vec::new(),
            count: 0,
            bytes: 0,
            insns: 0,
        });
        Ok(id as usize - 1)
    }
    fn alloc(&mut self, i: usize) -> Result<()> {
        let m = &mut self.objects[MAP][i];
        if m.key_size > 512 {
            return Err(libc::E2BIG);
        }
        if m.ty == 11 && !(5..=260).contains(&m.key_size) {
            return Err(libc::EINVAL);
        }
        if matches!(m.ty, 27 | 31) {
            let size = 2 * unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as usize + 2 * m.max;
            self.charge(i, size)?;
            if unsafe { libc::ftruncate(self.objects[MAP][i].fd.as_raw_fd(), size as _) } != 0 {
                return Err(errno());
            }
            return Ok(());
        }
        if matches!(m.ty, 24 | 28 | 29 | 32) && m.max == 0 {
            m.max = 256;
        }
        if m.max == 0 || m.max > 1 << 24 {
            return Err(libc::EINVAL);
        }
        m.stride = if is_percpu(m.ty) {
            ((m.value_size + 7) & !7) * self.ncpus
        } else {
            m.value_size
        };
        if m.stride > 1 << 24 {
            return Err(libc::E2BIG);
        }
        let size = m.max * m.stride.max(1)
            + if is_array(m.ty) {
                0
            } else {
                m.max * (m.key_size.max(1) + 1)
            };
        self.charge(i, size)?;
        let m = &mut self.objects[MAP][i];
        m.vals = vec![0; m.max * m.stride.max(1)];
        if !is_array(m.ty) {
            m.keys = vec![0; m.max * m.key_size.max(1)];
            m.used = vec![false; m.max];
        }
        Ok(())
    }
    fn charge(&mut self, i: usize, size: usize) -> Result<()> {
        if size > (128 << 20) + (2 << 20) * self.ncpus - self.bytes {
            return Err(libc::ENOMEM);
        }
        self.bytes += size;
        self.objects[MAP][i].bytes += size;
        Ok(())
    }
    fn parse(&self, name: &str) -> Option<(usize, usize)> {
        let tail = name.strip_prefix("lxbpf-")?;
        let (kind, id) = tail.split_once('-')?;
        let kind = KINDS.iter().position(|&k| k == kind)?;
        let id = id
            .split(|c: char| !c.is_ascii_digit())
            .next()?
            .parse::<usize>()
            .ok()?;
        (id > 0 && id <= self.objects[kind].len()).then_some((kind, id.wrapping_sub(1)))
    }
    fn lookup(&self, pid: u32, fd: u32, want: Option<usize>) -> Result<(usize, usize)> {
        let link = std::fs::read_link(format!("/proc/{pid}/fd/{fd}")).map_err(|_| libc::EBADF)?;
        let (k, i) = self
            .parse(
                link.to_str()
                    .and_then(|s| s.strip_prefix("/memfd:"))
                    .ok_or(libc::EBADF)?,
            )
            .ok_or(libc::EBADF)?;
        if want.is_some_and(|w| w != k) {
            return Err(libc::EBADF);
        }
        Ok((k, i))
    }
    fn inject(&self, req: &SeccompNotif, kind: usize, i: usize) -> Result<Reply> {
        // dup/ADDFD alone would share the open file description, so OFD locks
        // would never conflict. A fresh memfd would instead change the inode.
        let path = CString::new(format!(
            "/proc/self/fd/{}",
            self.objects[kind][i].fd.as_raw_fd()
        ))
        .unwrap();
        let fd = fd_result(unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) })?;
        self.inject_fd(req, fd.as_raw_fd(), libc::O_CLOEXEC as u32)
    }
    fn inject_fd(&self, req: &SeccompNotif, fd: RawFd, flags: u32) -> Result<Reply> {
        let a = SeccompNotifAddfd {
            id: req.id,
            flags: notify::SECCOMP_ADDFD_FLAG_SEND,
            srcfd: fd as u32,
            newfd: 0,
            newfd_flags: flags,
        };
        let r = unsafe {
            libc::ioctl(
                self.listener,
                notify::notif_addfd_cmd(size_of::<SeccompNotifAddfd>()) as _,
                &a,
            )
        };
        if r < 0 {
            Err(errno())
        } else {
            Ok(Reply::Injected)
        }
    }
    fn beneath(&self, rel: &str, flags: i32, mode: u32) -> Result<OwnedFd> {
        let pins = self.pins.as_ref().ok_or(libc::ENOENT)?;
        let path =
            CString::new(if rel.is_empty() { "." } else { rel }).map_err(|_| libc::EINVAL)?;
        // open_how: BENEATH | NO_XDEV | NO_SYMLINKS | NO_MAGICLINKS.
        let how = [(flags | libc::O_CLOEXEC) as u64, mode as u64, 0x0f];
        fd_result(unsafe {
            libc::syscall(
                libc::SYS_openat2,
                pins.as_raw_fd(),
                path.as_ptr(),
                &how,
                size_of_val(&how),
            )
        } as i32)
    }
    fn pin(&self, mem: MemFd, addr: u64, flags: i32, mode: u32) -> Result<OwnedFd> {
        let path = string(mem, addr)?;
        let rel = path.strip_prefix("/sys/fs/bpf/").ok_or(libc::EINVAL)?;
        let flags = flags | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY;
        let fd = match self.beneath(rel, flags, mode) {
            Err(libc::EACCES) if flags & libc::O_CREAT == 0 => {
                // netbpfload uses modes like 0060. Temporarily grant the host
                // owner read access; Flatpak has no CAP_DAC_OVERRIDE.
                let pfd = self.beneath(rel, libc::O_PATH | libc::O_NOFOLLOW, 0)?;
                let mut st: libc::stat = unsafe { std::mem::zeroed() };
                if unsafe { libc::fstat(pfd.as_raw_fd(), &mut st) } != 0
                    || st.st_mode & libc::S_IFMT != libc::S_IFREG
                {
                    return Err(libc::EACCES);
                }
                let p = CString::new(format!("/proc/self/fd/{}", pfd.as_raw_fd())).unwrap();
                if unsafe { libc::chmod(p.as_ptr(), (st.st_mode & 0o7777) | 0o400) } != 0 {
                    return Err(errno());
                }
                let result = fd_result(unsafe {
                    libc::open(
                        p.as_ptr(),
                        libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NONBLOCK | libc::O_NOCTTY,
                    )
                });
                unsafe {
                    libc::chmod(p.as_ptr(), st.st_mode & 0o7777);
                }
                result?
            }
            r => r?,
        };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstat(fd.as_raw_fd(), &mut st) } != 0
            || st.st_mode & libc::S_IFMT != libc::S_IFREG
        {
            return Err(libc::EINVAL);
        }
        Ok(fd)
    }
    fn elem(&mut self, req: &SeccompNotif, mem: MemFd, cmd: u32, a: &Attr) -> Result<Reply> {
        let (_, i) = self.lookup(req.pid, a.u32(0), Some(MAP))?;
        let m = &mut self.objects[MAP][i];
        if m.vals.is_empty() {
            return Err(libc::EINVAL);
        }
        let key = if a.u64(8) != 0 {
            Some(rd(mem, a.u64(8), m.key_size)?)
        } else {
            None
        };
        if cmd != 4 && key.is_none() {
            return Err(libc::EINVAL);
        }
        match cmd {
            1 | 21 => {
                let k = key.as_deref().unwrap();
                let i = m.index(k, true).ok_or(libc::ENOENT)?;
                wr(mem, a.u64(16), &m.vals[i * m.stride..(i + 1) * m.stride])?;
                if cmd == 21 && !is_array(m.ty) {
                    m.delete(k)?;
                }
            }
            2 => m.update(
                key.as_deref().unwrap(),
                &rd(mem, a.u64(16), m.stride)?,
                a.u64(24),
            )?,
            3 => m.delete(key.as_deref().unwrap())?,
            4 => wr(mem, a.u64(16), &m.next(key.as_deref())?)?,
            _ => unreachable!(),
        }
        Ok(Reply::Value(0))
    }
    fn bpf(&mut self, req: &SeccompNotif, mem: MemFd) -> Result<Reply> {
        let cmd = req.data.args[0] as u32;
        let addr = req.data.args[1];
        let mut a = Attr([0; 168]);
        let n = (req.data.args[2] as u32 as usize).min(a.0.len());
        a.0[..n].copy_from_slice(&rd(mem, addr, n)?);
        match cmd {
            0 | 5 | 18 | 28 | 17 => {
                let kind = match cmd {
                    0 => MAP,
                    5 => PROG,
                    18 => BTF,
                    _ => LINK,
                };
                let i = self.new_obj(kind)?;
                let o = &mut self.objects[kind][i];
                if kind == MAP {
                    o.ty = a.u32(0);
                    o.key_size = a.u32(4) as usize;
                    o.value_size = a.u32(8) as usize;
                    o.max = a.u32(12) as usize;
                    o.flags = a.u32(16);
                    if matches!(o.ty, 14 | 25) {
                        o.flags |= 1 << 7;
                    } // RDONLY_PROG
                    o.name.copy_from_slice(&a.0[28..44]);
                } else if kind == PROG {
                    o.ty = a.u32(0);
                    o.insns = a.u32(4);
                    o.name.copy_from_slice(&a.0[48..64]);
                }
                let result = if kind == MAP { self.alloc(i) } else { Ok(()) }
                    .and_then(|()| self.inject(req, kind, i));
                if result.is_err() {
                    let o = self.objects[kind].pop().unwrap();
                    self.bytes -= o.bytes;
                }
                result
            }
            1..=4 | 21 => self.elem(req, mem, cmd, &a),
            6 => {
                let (k, i) = self.lookup(req.pid, a.u32(8), None)?;
                let fd = self.pin(
                    mem,
                    a.u64(0),
                    libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                    0o600,
                )?;
                let o = &self.objects[k][i];
                let text = format!("lxbpf-{}-{}\n", KINDS[o.kind], o.id);
                if unsafe { libc::write(fd.as_raw_fd(), text.as_ptr() as _, text.len()) }
                    != text.len() as isize
                {
                    return Err(errno());
                }
                Ok(Reply::Value(0))
            }
            7 => {
                let fd = self.pin(mem, a.u64(0), libc::O_RDONLY, 0)?;
                let mut buf = [0; 64];
                let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr() as _, buf.len()) };
                if n <= 0 {
                    return Err(libc::EINVAL);
                }
                let text = std::str::from_utf8(&buf[..n as usize]).map_err(|_| libc::EINVAL)?;
                let (k, i) = self.parse(text).ok_or(libc::ENOENT)?;
                self.inject(req, k, i)
            }
            15 => {
                let (k, i) = self.lookup(req.pid, a.u32(0), None)?;
                let o = &self.objects[k][i];
                // Linux x86_64 bpf_{map,prog,btf,link}_info UAPI sizes.
                let mut info = vec![0; [104, 232, 32, 64][k]];
                if k == BTF {
                    put(&mut info, 12, o.id);
                } else {
                    put(&mut info, 4, o.id);
                }
                if k == MAP {
                    for (off, v) in [
                        (0, o.ty),
                        (8, o.key_size as u32),
                        (12, o.value_size as u32),
                        (16, o.max as u32),
                        (20, o.flags),
                    ] {
                        put(&mut info, off, v);
                    }
                    info[24..40].copy_from_slice(&o.name);
                } else if k == PROG {
                    put(&mut info, 0, o.ty);
                    put(&mut info, 16, o.insns.wrapping_mul(8));
                    put(&mut info, 20, o.insns.wrapping_mul(8));
                    info[64..80].copy_from_slice(&o.name);
                }
                let n = (a.u32(4) as usize).min(info.len());
                wr(mem, a.u64(8), &info[..n])?;
                wr(mem, addr + 4, &(n as u32).to_ne_bytes())?;
                Ok(Reply::Value(0))
            }
            11 | 12 | 23 => {
                let k = match cmd {
                    11 => PROG,
                    12 => MAP,
                    _ => BTF,
                };
                let next = a.u32(0).wrapping_add(1);
                if next as usize > self.objects[k].len() {
                    return Err(libc::ENOENT);
                }
                wr(mem, addr + 4, &next.to_ne_bytes())?;
                Ok(Reply::Value(0))
            }
            13 | 14 | 19 => {
                let k = match cmd {
                    13 => PROG,
                    14 => MAP,
                    _ => BTF,
                };
                let id = a.u32(0) as usize;
                if id == 0 || id > self.objects[k].len() {
                    return Err(libc::ENOENT);
                }
                self.inject(req, k, id - 1)
            }
            8 | 9 => {
                let t = a.u32(8) as usize;
                if t < self.attached.len() {
                    self.attached[t] = if cmd == 9 {
                        0
                    } else {
                        let (k, i) = self.lookup(req.pid, a.u32(4), Some(PROG))?;
                        self.objects[k][i].id
                    };
                }
                Ok(Reply::Value(0))
            }
            16 => {
                let id = self.attached.get(a.u32(4) as usize).copied().unwrap_or(0);
                let count = u32::from(id != 0);
                if count != 0 && a.u32(24) >= 1 && a.u64(16) != 0 {
                    wr(mem, a.u64(16), &id.to_ne_bytes())?;
                }
                wr(mem, addr + 24, &count.to_ne_bytes())?;
                Ok(Reply::Value(0))
            }
            10 | 22 | 29 | 34 | 35 => Ok(Reply::Value(0)),
            36 => Err(libc::EOPNOTSUPP),
            _ => {
                eprintln!("bpfemu: unsupported bpf cmd {cmd} from pid {}", req.pid);
                Err(libc::EINVAL)
            }
        }
    }
    #[allow(non_upper_case_globals)] // libc syscall constants retain their UAPI names.
    fn handle(&mut self, req: &SeccompNotif) -> Result<Reply> {
        use libc::*;
        let mem = notify::open_mem(req.pid as i32).ok_or(EFAULT)?;
        if !notify::notif_id_valid(self.listener, req.id) {
            return Err(ESRCH);
        }
        let m = mem.raw();
        let a = req.data.args;
        match req.data.nr as i64 {
            SYS_bpf => self.bpf(req, m),
            SYS_sched_setscheduler | SYS_setsockopt | SYS_capset | SYS_prctl => Ok(Reply::Value(0)),
            SYS_prlimit64 if a[2] == 0 => Ok(Reply::Continue),
            SYS_prlimit64 | SYS_setrlimit => Ok(Reply::Value(0)),
            SYS_getsockopt => {
                let len = rd(m, a[4], 4)?;
                if u32::from_ne_bytes(len.try_into().unwrap()) < 8 {
                    return Err(EINVAL);
                }
                wr(m, a[3], &0u64.to_ne_bytes())?;
                wr(m, a[4], &8u32.to_ne_bytes())?;
                Ok(Reply::Value(0))
            }
            SYS_timerfd_create => {
                let clock = if a[0] == CLOCK_REALTIME_ALARM as u64 {
                    CLOCK_REALTIME
                } else {
                    CLOCK_BOOTTIME
                };
                let flags = a[1] as i32;
                let fd = fd_result(unsafe { timerfd_create(clock, flags | TFD_CLOEXEC) })?;
                self.inject_fd(req, fd.as_raw_fd(), (flags & TFD_CLOEXEC) as u32)
            }
            SYS_statfs => {
                let Ok(path) = string(m, a[0]) else {
                    return Ok(Reply::Continue);
                };
                let rel = if path == "/sys/fs/bpf" {
                    ""
                } else if let Some(rel) = path.strip_prefix("/sys/fs/bpf/") {
                    rel
                } else {
                    return Ok(Reply::Continue);
                };
                let fd = self.beneath(rel, O_PATH, 0)?;
                let mut st: statfs = unsafe { std::mem::zeroed() };
                if unsafe { fstatfs(fd.as_raw_fd(), &mut st) } != 0 {
                    return Err(errno());
                }
                st.f_type = 0xcafe4a11;
                // statfs is the native x86_64 syscall layout in both libcs.
                let bytes = unsafe {
                    std::slice::from_raw_parts(
                        (&st as *const statfs).cast::<u8>(),
                        size_of::<statfs>(),
                    )
                };
                wr(m, a[1], bytes)?;
                Ok(Reply::Value(0))
            }
            SYS_getxattr | SYS_lgetxattr => {
                if string(m, a[1]).as_deref() != Ok("security.selinux") {
                    return Ok(Reply::Continue);
                }
                let Ok(path) = string(m, a[0]) else {
                    return Ok(Reply::Continue);
                };
                let ctx = match path.as_str() {
                    "/apex/com.android.tethering/bin/for-system" => {
                        "u:object_r:system_file:s0".to_string()
                    }
                    "/apex/com.android.tethering/bin/for-system/clatd" => {
                        "u:object_r:clatd_exec:s0".to_string()
                    }
                    "/sys/fs/bpf" => "u:object_r:fs_bpf:s0".to_string(),
                    p => {
                        let Some(d) = p
                            .strip_prefix("/sys/fs/bpf/")
                            .and_then(|s| s.split('/').next())
                            .filter(|d| !d.is_empty() && d.len() < 64)
                        else {
                            return Ok(Reply::Continue);
                        };
                        format!("u:object_r:fs_bpf_{d}:s0")
                    }
                };
                let bytes = CString::new(ctx).unwrap().into_bytes_with_nul();
                if a[3] != 0 {
                    if a[3] < bytes.len() as u64 {
                        return Err(ERANGE);
                    }
                    wr(m, a[2], &bytes)?;
                }
                Ok(Reply::Value(bytes.len() as i64))
            }
            SYS_capget => {
                let hdr = rd(m, a[0], 8)?;
                if u32::from_ne_bytes(hdr[..4].try_into().unwrap()) != 0x20080522 || a[1] == 0 {
                    return Ok(Reply::Continue);
                }
                let mut data = vec![0; 24];
                for (off, v) in [(0, u32::MAX), (4, u32::MAX), (12, 0x1ff), (16, 0x1ff)] {
                    put(&mut data, off, v);
                }
                wr(m, a[1], &data)?;
                Ok(Reply::Value(0))
            }
            _ => Ok(Reply::Continue),
        }
    }
}
