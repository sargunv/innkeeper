//! The serve loop. [`serve`] receives every notification of the session's
//! seccomp filter ([`crate::filter`]) and either answers it here or hands it
//! to the module that owns it. Binder is served here, which connects the
//! [`BinderDriver`] to real Android processes:
//!
//!  * An `openat` of a binder device path injects a memfd (`ADDFD`+`SEND`).
//!    The process maps it as its receive region, and the broker writes
//!    transactions there through its own shared mapping.
//!  * `ioctl(fd, BINDER_*, argp)` goes to [`BinderDriver::ioctl`], which
//!    reaches the caller's memory through its pinned `/proc/<tid>/mem` (see
//!    [`PinnedMem`]) and answers the notification itself. It answers at once,
//!    or, for a blocking read, once there is work. Waits stay interruptible so
//!    that ptrace and job control work. A signal ends the notification, and
//!    the kernel restarts the ioctl. The driver makes the ioctl idempotent so
//!    the restart is safe.
//!  * When a thread sets up polling (`IPCThreadState::setupPolling`), its
//!    process's binder fd is swapped for an eventfd (`ADDFD`+`SETFD`) whose
//!    level the broker drives (`binder_poll`). The mapping keeps the memfd
//!    alive after the swap.
//!
//! A monitor thread watches a pidfd per process to detect death. It also
//! checks periodically with `kcmp` that each process still holds its binder
//! fd, which it loses on close or exec. When a process dies or loses its fd,
//! the monitor releases its broker process, which fires death notifications.

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::binder::driver::{BinderDriver, Call, InstallError, ProcId, Userspace};
use crate::binder::uapi::*;
use crate::{Config, Guest, trace};

// Seccomp uapi for x86_64.

const SECCOMP_IOC_MAGIC: u8 = b'!';
const fn ioc(dir: u32, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((SECCOMP_IOC_MAGIC as u32) << 8) | (nr as u32) | ((size as u32) << 16)
}
fn notif_send_cmd(sz: usize) -> u32 {
    ioc(3, 1, sz)
}
fn notif_id_valid_cmd() -> u32 {
    ioc(1, 2, size_of::<u64>())
}
fn notif_addfd_cmd(sz: usize) -> u32 {
    ioc(1, 3, sz)
}

const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;
const SECCOMP_ADDFD_FLAG_SETFD: u32 = 1;
pub(crate) const SECCOMP_ADDFD_FLAG_SEND: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct SeccompData {
    pub(crate) nr: i32,
    arch: u32,
    instruction_pointer: u64,
    pub(crate) args: [u64; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct SeccompNotif {
    pub(crate) id: u64,
    pub(crate) pid: u32,
    flags: u32,
    pub(crate) data: SeccompData,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SeccompNotifResp {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub(crate) struct SeccompNotifAddfd {
    pub(crate) id: u64,
    pub(crate) flags: u32,
    pub(crate) srcfd: u32,
    pub(crate) newfd: u32,
    pub(crate) newfd_flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SeccompNotifSizes {
    seccomp_notif: u16,
    seccomp_notif_resp: u16,
    seccomp_data: u16,
}

const SECCOMP_GET_NOTIF_SIZES: u32 = 3;
const O_CLOEXEC: u32 = 0o2000000;
const O_NONBLOCK: u32 = 0o4000;
const KCMP_FILE: i32 = 0;

/// Size of each receive memfd. The kernel caps binder mappings at 4 MiB. The
/// memfd is sparse, and the broker hands out offsets only inside the part the
/// process mapped, which is 1 MiB minus 2 pages for libbinder.
const BINDER_MEMFD_SIZE: usize = 4 * 1024 * 1024;

/// The session's seccomp listener and the kernel's size of a response.
#[derive(Clone, Copy)]
pub(crate) struct Listener {
    fd: RawFd,
    resp_sz: usize,
}

impl Listener {
    fn new(fd: RawFd) -> std::io::Result<(Listener, usize)> {
        let mut sizes = SeccompNotifSizes::default();
        let r = unsafe { libc::syscall(libc::SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &mut sizes) };
        if r != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let notif_sz = (sizes.seccomp_notif as usize).max(size_of::<SeccompNotif>());
        let resp_sz = (sizes.seccomp_notif_resp as usize).max(size_of::<SeccompNotifResp>());
        Ok((Listener { fd, resp_sz }, notif_sz))
    }

    #[cfg(test)]
    pub(crate) const NONE: Listener = Listener { fd: -1, resp_sz: 0 };

    fn send(&self, id: u64, val: i64, errno: i32, flags: u32) -> bool {
        let resp = SeccompNotifResp {
            id,
            val,
            error: -errno.abs(),
            flags,
        };
        let mut buf = vec![0u8; self.resp_sz];
        unsafe {
            std::ptr::write_unaligned(buf.as_mut_ptr() as *mut SeccompNotifResp, resp);
            libc::ioctl(self.fd, notif_send_cmd(self.resp_sz) as _, buf.as_mut_ptr()) == 0
        }
    }

    /// Answers notification `id` with `val`, or fails it with `errno` if that
    /// is nonzero. Returns false if the notification is gone because a signal
    /// ended its wait or its task died.
    pub(crate) fn reply(&self, id: u64, val: i64, errno: i32) -> bool {
        self.send(id, val, errno, 0)
    }

    /// Lets the kernel run notification `id`'s syscall.
    pub(crate) fn cont(&self, id: u64) -> bool {
        self.send(id, 0, 0, SECCOMP_USER_NOTIF_FLAG_CONTINUE)
    }

    /// Whether notification `id` is still pending, with its task still in the
    /// syscall.
    pub(crate) fn valid(&self, id: u64) -> bool {
        unsafe { libc::ioctl(self.fd, notif_id_valid_cmd() as _, &id as *const u64) == 0 }
    }

    /// Installs a copy of fd `a.srcfd` in the caller and returns the new fd
    /// number, or -1.
    pub(crate) fn addfd(&self, a: &SeccompNotifAddfd) -> i32 {
        unsafe {
            libc::ioctl(
                self.fd,
                notif_addfd_cmd(size_of::<SeccompNotifAddfd>()) as _,
                a as *const _,
            )
        }
    }

    /// Completes `req`, an open, with a copy of `fd` that is close-on-exec if
    /// the open's flags ask for it. Returns the new fd number, or -1.
    pub(crate) fn open_with(&self, req: &SeccompNotif, fd: RawFd) -> i32 {
        self.addfd(&SeccompNotifAddfd {
            id: req.id,
            flags: SECCOMP_ADDFD_FLAG_SEND,
            srcfd: fd as u32,
            newfd: 0,
            newfd_flags: (req.data.args[2] as u32) & O_CLOEXEC,
        })
    }
}

/// Worker threads for notifications that can wait on the filesystem, so that
/// they don't block the receive thread, which serves binder.
pub(crate) struct Pool(mpsc::Sender<Box<dyn FnOnce() + Send>>);

impl Pool {
    fn new(threads: usize) -> Pool {
        let (tx, rx) = mpsc::channel::<Box<dyn FnOnce() + Send>>();
        let rx = Arc::new(Mutex::new(rx));
        for _ in 0..threads {
            let rx = Arc::clone(&rx);
            std::thread::spawn(move || {
                loop {
                    let Ok(job) = rx.lock().unwrap().recv() else {
                        return;
                    };
                    job();
                }
            });
        }
        Pool(tx)
    }

    pub(crate) fn run(&self, job: impl FnOnce() + Send + 'static) {
        self.0.send(Box::new(job)).expect("worker pool stopped");
    }
}

struct ProcState {
    pid: i32,
    /// The binder fd number in the target.
    fd: u32,
    /// The receive region, which the process maps and the broker writes
    /// through `map`. The process can resize it. If it shrinks it, the
    /// broker's next write there faults with SIGBUS and kills the broker,
    /// which takes binder down for the whole session. Desktop parity accepts
    /// that, since a desktop app can also kill the user's session.
    memfd: OwnedFd,
    memfd_ino: u64,
    /// The opener's address space, pinned while its notification was valid
    /// (see [`PinnedMem`]). All access to the process's memory goes through
    /// it.
    mem: PinnedMem,
    map: *mut u8,
    /// Target mapping (base, usable length), found in /proc/<pid>/maps.
    base: Option<(u64, usize)>,
    free: Vec<(usize, usize)>,
    live: HashMap<u64, (usize, usize)>,
    pidfd: Option<OwnedFd>,
    /// The eventfd the process holds instead of its binder fd once a thread
    /// polls. It is readable when a polling thread has work. The process
    /// shares the open file, including its flags. If it clears O_NONBLOCK and
    /// drains or fills the counter, the broker's next read or write of it
    /// blocks under the broker's lock, and binder stops for the whole session.
    /// Desktop parity accepts that too.
    poll: Option<OwnedFd>,
    poll_ready: bool,
}

// The raw mapping pointer is owned by the supervisor under its lock.
unsafe impl Send for ProcState {}

impl Drop for ProcState {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.map as *mut _, BINDER_MEMFD_SIZE) };
    }
}

impl ProcState {
    /// Whether the process still holds its binder fd, which it loses on close
    /// or across exec.
    fn still_open(&self) -> bool {
        let ours = self.poll.as_ref().unwrap_or(&self.memfd).as_raw_fd();
        kcmp_same_file(self.pid, ours, self.fd)
    }

    fn ensure_base(&mut self) -> Option<(u64, usize)> {
        if self.base.is_some() {
            return self.base;
        }
        let maps = std::fs::read_to_string(format!("/proc/{}/maps", self.pid)).ok()?;
        for line in maps.lines() {
            // start-end perms offset dev inode path
            let mut it = line.split_ascii_whitespace();
            let (Some(range), Some(_), Some(off), Some(_), Some(ino), Some(path)) = (
                it.next(),
                it.next(),
                it.next(),
                it.next(),
                it.next(),
                it.next(),
            ) else {
                continue;
            };
            if ino.parse::<u64>().ok() != Some(self.memfd_ino)
                || !path.starts_with("/memfd:innkeeper-binder-")
                || !off.trim_start_matches('0').is_empty()
            {
                continue;
            }
            let (s, e) = range.split_once('-')?;
            let s = u64::from_str_radix(s, 16).ok()?;
            let e = u64::from_str_radix(e, 16).ok()?;
            let len = ((e - s) as usize).min(BINDER_MEMFD_SIZE);
            self.base = Some((s, len));
            self.free = vec![(0, len)];
            break;
        }
        self.base
    }

    fn alloc(&mut self, size: usize) -> Option<usize> {
        let size = (size + 7) & !7;
        let mut best: Option<usize> = None;
        for (i, &(_, len)) in self.free.iter().enumerate() {
            if len >= size && best.map(|b| self.free[b].1 > len).unwrap_or(true) {
                best = Some(i);
            }
        }
        let i = best?;
        let (off, len) = self.free[i];
        if len == size {
            self.free.remove(i);
        } else {
            self.free[i] = (off + size, len - size);
        }
        Some(off)
    }

    fn dealloc(&mut self, off: usize, size: usize) {
        let size = (size + 7) & !7;
        let i = self.free.partition_point(|&(o, _)| o < off);
        self.free.insert(i, (off, size));
        if i + 1 < self.free.len() && self.free[i].0 + self.free[i].1 == self.free[i + 1].0 {
            self.free[i].1 += self.free[i + 1].1;
            self.free.remove(i + 1);
        }
        if i > 0 && self.free[i - 1].0 + self.free[i - 1].1 == self.free[i].0 {
            self.free[i - 1].1 += self.free[i].1;
            self.free.remove(i);
        }
    }
}

pub struct Supervisor {
    guest: Guest,
    broker: BinderDriver,
    us: SeccompUserspace,
    /// Virtual ids. Binder's sender_euid comes from them.
    vid: Arc<crate::creds::Vid>,
    /// (tgid, fd) -> broker process
    fd_index: HashMap<(i32, u32), ProcId>,
    tgids: HashMap<i32, i32>,
    next_memfd: u64,
    watch: mpsc::Sender<i32>,
}

/// Reads memory of task `pid` by number, without pinning. Use it only for
/// reads that precede a SECCOMP_IOCTL_NOTIF_ID_VALID check, or that decide
/// whether a call needs a closer look.
pub(crate) fn vm_read(pid: i32, addr: u64, len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
    }
    if addr == 0 {
        return None;
    }
    let mut buf = vec![0u8; len];
    let local = libc::iovec {
        iov_base: buf.as_mut_ptr() as *mut _,
        iov_len: len,
    };
    let remote = libc::iovec {
        iov_base: addr as *mut _,
        iov_len: len,
    };
    let n = unsafe { libc::process_vm_readv(pid, &local, 1, &remote, 1, 0) };
    if n == len as isize { Some(buf) } else { None }
}

/// A task's pinned address space. It is the task's `/proc/<tid>/mem`, which
/// holds the mm the task had when the file was opened, so after an exec it
/// reaches nothing. Callers open it and then check
/// SECCOMP_IOCTL_NOTIF_ID_VALID. An mm switch on exec, or tid reuse, happens
/// only after the caller's notification is gone, so a valid id afterwards
/// means the mm is the caller's. Writes meant for a caller therefore can't
/// land in another process or in the image it exec'd.
///
/// Accesses through it use FOLL_FORCE, so unlike `process_vm_*` they also
/// reach the caller's read-only and PROT_NONE private pages. A write there
/// breaks copy-on-write. That is acceptable under the desktop-parity threat
/// model (docs/security.md), because the memory is the caller's own and the
/// caller can change it anyway.
pub(crate) struct PinnedMem(OwnedFd);

/// The fd of a [`PinnedMem`], for passing around under a borrow.
#[derive(Clone, Copy)]
pub(crate) struct MemFd(RawFd);

impl PinnedMem {
    pub(crate) fn raw(&self) -> MemFd {
        MemFd(self.0.as_raw_fd())
    }
}

pub(crate) fn open_mem(tid: i32) -> Option<PinnedMem> {
    let path = std::ffi::CString::new(format!("/proc/{tid}/mem")).ok()?;
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    (fd >= 0).then(|| PinnedMem(unsafe { OwnedFd::from_raw_fd(fd) }))
}

/// Reads target memory through its pinned address space.
pub(crate) fn mem_read(m: MemFd, addr: u64, len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
    }
    let off = i64::try_from(addr).ok().filter(|&a| a != 0)?;
    let mut buf = vec![0u8; len];
    let n = unsafe { libc::pread(m.0, buf.as_mut_ptr() as *mut _, len, off) };
    (n == len as isize).then_some(buf)
}

/// Writes target memory through its pinned address space.
pub(crate) fn mem_write(m: MemFd, addr: u64, data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    let Some(off) = i64::try_from(addr).ok().filter(|&a| a != 0) else {
        return false;
    };
    let n = unsafe { libc::pwrite(m.0, data.as_ptr() as *const _, data.len(), off) };
    n == data.len() as isize
}

/// Reads a NUL-terminated path. It doesn't read into the next page until it
/// knows the path continues there, since that page need not be mapped.
pub(crate) fn read_path(pid: i32, addr: u64) -> Option<Vec<u8>> {
    read_path_with(addr, |a, n| vm_read(pid, a, n))
}

/// `read_path` through a pinned `/proc/<pid>/mem` fd.
pub(crate) fn mem_read_path(mem: MemFd, addr: u64) -> Option<Vec<u8>> {
    read_path_with(addr, |a, n| mem_read(mem, a, n))
}

fn read_path_with(addr: u64, read: impl Fn(u64, usize) -> Option<Vec<u8>>) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut a = addr;
    while out.len() < 4096 {
        let to_page_end = 4096 - (a as usize & 4095);
        let chunk = to_page_end.min(64);
        let buf = read(a, chunk)?;
        if let Some(p) = buf.iter().position(|&b| b == 0) {
            out.extend_from_slice(&buf[..p]);
            return Some(out);
        }
        out.extend_from_slice(&buf);
        a += chunk as u64;
    }
    None
}

pub(crate) fn status_field(pid: i32, key: &str) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
}

fn pidfd_open(pid: i32) -> Option<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

fn kcmp_same_file(pid: i32, ours: RawFd, theirs: u32) -> bool {
    let me = unsafe { libc::getpid() };
    let r = unsafe {
        libc::syscall(
            libc::SYS_kcmp,
            me,
            pid,
            KCMP_FILE,
            ours as u64,
            theirs as u64,
        )
    };
    r == 0
}

/// A receive-region memfd of BINDER_MEMFD_SIZE.
fn recv_memfd(name: &str) -> Option<OwnedFd> {
    let cname = std::ffi::CString::new(name).ok()?;
    let fd = unsafe { libc::memfd_create(cname.as_ptr(), libc::MFD_CLOEXEC) };
    if fd < 0 {
        return None;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    (unsafe { libc::ftruncate(fd.as_raw_fd(), BINDER_MEMFD_SIZE as i64) } == 0).then_some(fd)
}

/// The Android processes the driver serves, reached through their pinned
/// memory, their receive memfds and the seccomp listener.
struct SeccompUserspace {
    listener: Listener,
    procs: HashMap<ProcId, ProcState>,
    captured: HashMap<u64, OwnedFd>,
    next_token: u64,
    timer: Arc<Timer>,
    /// OpenHarmony's access tokens (empty for Android).
    tokens: crate::ohos::Tokens,
}

/// The driver's clock and the time it asked to be woken at. `monitor` calls
/// `BinderDriver::expire` at that time.
struct Timer {
    start: std::time::Instant,
    /// Milliseconds since `start`, or u64::MAX when unarmed.
    at: AtomicU64,
}

impl Timer {
    fn now_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

impl Userspace for SeccompUserspace {
    fn access_token(&mut self, proc: ProcId, _tid: u32) -> (u64, u64) {
        self.procs
            .get(&proc)
            .map_or((0, 0), |p| self.tokens.get(p.pid))
    }
    fn read(&mut self, proc: ProcId, addr: u64, len: usize) -> Option<Vec<u8>> {
        mem_read(self.procs.get(&proc)?.mem.raw(), addr, len)
    }
    fn write(&mut self, proc: ProcId, addr: u64, data: &[u8]) -> bool {
        self.procs
            .get(&proc)
            .is_some_and(|p| mem_write(p.mem.raw(), addr, data))
    }
    fn recv_size(&mut self, proc: ProcId) -> usize {
        self.procs
            .get_mut(&proc)
            .and_then(|p| p.ensure_base())
            .map(|(_, l)| l)
            .unwrap_or(0)
    }
    fn alloc_recv(&mut self, proc: ProcId, size: usize, _is_async: bool) -> Option<u64> {
        let p = self.procs.get_mut(&proc)?;
        let (base, _) = p.ensure_base()?;
        let off = p.alloc(size)?;
        let addr = base + off as u64;
        p.live.insert(addr, (off, size));
        Some(addr)
    }
    fn write_recv(&mut self, proc: ProcId, addr: u64, data: &[u8]) {
        if let Some(p) = self.procs.get_mut(&proc)
            && let Some((base, len)) = p.base
            && addr >= base
        {
            let off = (addr - base) as usize;
            if off.checked_add(data.len()).is_some_and(|e| e <= len) {
                unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), p.map.add(off), data.len()) };
            }
        }
    }
    fn free_recv(&mut self, proc: ProcId, addr: u64) {
        if let Some(p) = self.procs.get_mut(&proc)
            && let Some((off, len)) = p.live.remove(&addr)
        {
            p.dealloc(off, len);
        }
    }
    fn capture_fd(&mut self, from: ProcId, from_fd: u32) -> Option<u64> {
        let pidfd = self.procs.get(&from)?.pidfd.as_ref()?;
        let got =
            unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), from_fd as i32, 0) };
        if got < 0 {
            return None;
        }
        self.next_token += 1;
        self.captured.insert(self.next_token, unsafe {
            OwnedFd::from_raw_fd(got as RawFd)
        });
        Some(self.next_token)
    }
    fn install_fd(&mut self, _to: ProcId, call: Call, token: u64) -> Result<u32, InstallError> {
        let fd = self.captured.get(&token).ok_or(InstallError::Failed)?;
        let r = self.listener.addfd(&SeccompNotifAddfd {
            id: call.id,
            flags: 0,
            srcfd: fd.as_raw_fd() as u32,
            newfd: 0,
            newfd_flags: O_CLOEXEC,
        });
        match r {
            r if r >= 0 => Ok(r as u32),
            _ if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) => {
                Err(InstallError::Interrupted)
            }
            _ => Err(InstallError::Failed),
        }
    }
    fn drop_fd(&mut self, token: u64) {
        self.captured.remove(&token);
    }
    fn answer(&mut self, call: Call, errno: i32) -> bool {
        self.listener.reply(call.id, 0, errno)
    }
    fn pending(&mut self, call: Call) -> bool {
        self.listener.valid(call.id)
    }
    fn make_pollable(&mut self, proc: ProcId, call: Call) -> bool {
        let Some(p) = self.procs.get(&proc) else {
            return false;
        };
        // Swap once per process. Swapping again would replace the eventfd
        // under the other polling threads' epoll registrations.
        if p.poll.is_some() {
            return true;
        }
        let efd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if efd < 0 {
            return false;
        }
        let efd = unsafe { OwnedFd::from_raw_fd(efd) };
        let r = self.listener.addfd(&SeccompNotifAddfd {
            id: call.id,
            flags: SECCOMP_ADDFD_FLAG_SETFD,
            srcfd: efd.as_raw_fd() as u32,
            newfd: p.fd,
            newfd_flags: O_CLOEXEC,
        });
        if r < 0 {
            trace!("poll swap failed proc={proc}");
            return false;
        }
        trace!("poll swap proc={proc} fd={}", p.fd);
        let p = self.procs.get_mut(&proc).unwrap();
        p.poll = Some(efd);
        p.poll_ready = false;
        true
    }
    fn set_poll_ready(&mut self, proc: ProcId, ready: bool) {
        let Some(p) = self.procs.get_mut(&proc) else {
            return;
        };
        let Some(efd) = p.poll.as_ref() else {
            return;
        };
        if ready == p.poll_ready {
            return;
        }
        let fd = efd.as_raw_fd();
        if ready {
            let one: u64 = 1;
            unsafe { libc::write(fd, &one as *const u64 as *const _, 8) };
        } else {
            let mut buf = [0u8; 8];
            unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, 8) };
        }
        p.poll_ready = ready;
    }

    fn now_ms(&mut self) -> u64 {
        self.timer.now_ms()
    }

    fn arm_timer(&mut self, at_ms: u64) {
        self.timer.at.fetch_min(at_ms, Ordering::Relaxed);
    }
}

/// Receives and serves every notification on the session's listener.
/// `pins` is the bpffs directory opened before the guest can mount over it.
pub fn serve(fd: RawFd, pins: Option<OwnedFd>, cfg: Config) {
    let (listener, notif_sz) = match Listener::new(fd) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("broker: SECCOMP_GET_NOTIF_SIZES: {e}");
            return;
        }
    };
    let guest = cfg.guest;
    let mut emulation = crate::bpf::Emulation::new(listener, pins);
    let (tx, rx) = mpsc::channel();
    let timer = Arc::new(Timer {
        start: std::time::Instant::now(),
        at: AtomicU64::new(u64::MAX),
    });
    let pool = Arc::new(Pool::new(4));
    let input = crate::input::Input::new(listener);
    let fs = Arc::new(crate::fs::Fs::new(
        listener,
        &cfg,
        input.as_ref().map(|i| i.nodes()),
    ));
    // Virtual ids for every process (supervisor/README.md).
    let vid = crate::creds::Vid::new(listener, Arc::clone(&pool), Arc::clone(&fs), cfg.init);
    let sup = Arc::new(Mutex::new(Supervisor {
        guest,
        broker: BinderDriver::with_guest(guest),
        us: SeccompUserspace {
            listener,
            procs: HashMap::new(),
            captured: HashMap::new(),
            next_token: 0,
            timer: Arc::clone(&timer),
            tokens: Default::default(),
        },
        vid: Arc::clone(&vid),
        fd_index: HashMap::new(),
        tgids: HashMap::new(),
        next_memfd: 1,
        watch: tx,
    }));
    {
        let sup = Arc::clone(&sup);
        std::thread::spawn(move || monitor(sup, rx, timer));
    }
    let to_fs = |req: SeccompNotif| {
        let fs = Arc::clone(&fs);
        pool.run(move || fs.serve(&req));
    };
    let recv_cmd = ioc(3, 0, notif_sz);
    let mut notif = vec![0u8; notif_sz];
    loop {
        notif.fill(0);
        if unsafe { libc::ioctl(fd, recv_cmd as _, notif.as_mut_ptr()) } != 0 {
            let e = std::io::Error::last_os_error();
            if matches!(e.raw_os_error(), Some(libc::EINTR) | Some(libc::ENOENT)) {
                continue;
            }
            eprintln!("broker: NOTIF_RECV: {e}");
            return;
        }
        let mut req: SeccompNotif = unsafe { std::ptr::read_unaligned(notif.as_ptr() as *const _) };
        as_openat(&mut req);
        let nr = req.data.nr as i64;
        if let Some(input) = &input
            && input.serve(&req)
        {
            continue;
        }
        if nr == libc::SYS_inotify_add_watch {
            listener.cont(req.id);
            continue;
        }
        if crate::creds::is_cred_syscall(nr, &req.data.args) {
            vid.serve(&req);
            continue;
        }
        if crate::fs::is_fs_syscall(nr) {
            to_fs(req);
            continue;
        }
        if !is_binder_syscall(guest, nr, &req.data.args) {
            emulation.serve(&req);
            continue;
        }
        // /proc/<pid>/status with the virtual ids.
        if nr == libc::SYS_openat && vid.proc_status(&req) {
            continue;
        }
        // Every openat in the session lands here. Let the ones that can't be
        // a binder device through without taking the lock.
        if nr == libc::SYS_openat && !binder_path(req.pid as i32, req.data.args[1]) {
            to_fs(req);
            continue;
        }
        sup.lock().unwrap().dispatch(req);
    }
}

/// OpenHarmony's musl opens files with `open(2)`, which the filter traps only
/// for that guest. This serves it as the equivalent `openat(AT_FDCWD, ...)`.
/// The answer goes by notification id, so a continued request still runs the
/// caller's original open.
fn as_openat(req: &mut SeccompNotif) {
    if req.data.nr as i64 == libc::SYS_open {
        let a = req.data.args;
        req.data.nr = libc::SYS_openat as i32;
        req.data.args = [libc::AT_FDCWD as u64, a[0], a[1], a[2], 0, 0];
    }
}

/// Whether this notification goes to [`Supervisor::dispatch`] instead of the
/// BPF emulation. That covers every `openat`, binder `ioctl`s and
/// OpenHarmony's device `ioctl`s.
fn is_binder_syscall(guest: Guest, nr: i64, args: &[u64; 6]) -> bool {
    nr == libc::SYS_openat
        || (nr == libc::SYS_ioctl
            && (is_binder_ioctl(args[1] as u32)
                || (guest == Guest::Ohos
                    && (crate::ohos::is_token_ioctl(args[1] as u32)
                        || crate::ashmem::is_ashmem_ioctl(args[1] as u32)))))
}

/// Whether the path names a binder device. Only binder devices take the binder
/// lock. Other device opens still need Flatpak policy, in particular the
/// selected DRM node.
fn binder_path(pid: i32, addr: u64) -> bool {
    let n = (4096 - (addr as usize & 4095)).min(5);
    match vm_read(pid, addr, n) {
        Some(b) if b"/dev/".starts_with(&b) || b == b"/dev/" => matches!(
            read_path(pid, addr).as_deref(),
            Some(b"/dev/binder" | b"/dev/hwbinder" | b"/dev/vndbinder")
        ),
        _ => false,
    }
}

/// Watches pidfds for process death and sweeps with kcmp for binder fds that
/// were closed or lost across exec, and releases those broker processes. It
/// also fires the driver's timer for `BINDER_FREEZE` timeouts.
fn monitor(sup: Arc<Mutex<Supervisor>>, rx: mpsc::Receiver<i32>, timer: Arc<Timer>) {
    let mut watched: HashMap<i32, OwnedFd> = HashMap::new();
    let mut last_sweep = std::time::Instant::now();
    loop {
        while let Ok(pid) = rx.try_recv() {
            if let std::collections::hash_map::Entry::Vacant(e) = watched.entry(pid)
                && let Some(fd) = pidfd_open(pid)
            {
                e.insert(fd);
            }
        }
        let mut pfds: Vec<libc::pollfd> = watched
            .values()
            .map(|fd| libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let pids: Vec<i32> = watched.keys().copied().collect();
        let wait = timer
            .at
            .load(Ordering::Relaxed)
            .saturating_sub(timer.now_ms());
        unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, wait.min(100) as i32) };
        let dead: Vec<i32> = pfds
            .iter()
            .zip(&pids)
            .filter(|(p, _)| p.revents != 0)
            .map(|(_, &pid)| pid)
            .collect();
        let sweep = last_sweep.elapsed() >= std::time::Duration::from_millis(500);
        let due = timer.at.load(Ordering::Relaxed) <= timer.now_ms();
        if dead.is_empty() && !sweep && !due {
            continue;
        }
        let mut s = sup.lock().unwrap();
        if due {
            // Disarm first, because expire re-arms for the deadlines still
            // ahead.
            timer.at.store(u64::MAX, Ordering::Relaxed);
            let s = &mut *s;
            s.broker.expire(&mut s.us);
        }
        for pid in &dead {
            watched.remove(pid);
            s.release_pid(*pid);
        }
        if sweep {
            last_sweep = std::time::Instant::now();
            s.release_closed(None);
            let live: HashSet<i32> = s.us.procs.values().map(|p| p.pid).collect();
            watched.retain(|pid, _| live.contains(pid));
        }
    }
}

impl Supervisor {
    /// Answers a notification that isn't a binder ioctl. `errno` is positive.
    fn respond(&self, id: u64, errno: i32, cont: bool) {
        if cont {
            self.us.listener.cont(id);
        } else {
            self.us.listener.reply(id, 0, errno);
        }
    }

    fn tgid(&mut self, tid: i32) -> Option<i32> {
        if let Some(&t) = self.tgids.get(&tid) {
            if t == tid || std::path::Path::new(&format!("/proc/{t}/task/{tid}")).exists() {
                return Some(t);
            }
            self.tgids.remove(&tid);
        }
        let t = status_field(tid, "Tgid:")?.parse().ok()?;
        if self.tgids.len() > 65536 {
            self.tgids.clear();
        }
        self.tgids.insert(tid, t);
        Some(t)
    }

    fn dispatch(&mut self, req: SeccompNotif) {
        match req.data.nr as i64 {
            libc::SYS_openat => self.handle_openat(req),
            libc::SYS_ioctl => self.handle_ioctl(req),
            _ => self.respond(req.id, 0, true),
        }
    }

    fn handle_openat(&mut self, req: SeccompNotif) {
        let tid = req.pid as i32;
        let path = read_path(tid, req.data.args[1]);
        let ctx = match path.as_deref() {
            Some(b"/dev/binder") => 0u8,
            Some(b"/dev/hwbinder") => 1,
            Some(b"/dev/vndbinder") => 2,
            _ => {
                self.respond(req.id, 0, true);
                return;
            }
        };
        let Some(pid) = self.tgid(tid) else {
            self.respond(req.id, 0, true);
            return;
        };
        // Pin the address space, then check that tid is still the task that
        // asked, so the path read above was also its path.
        let Some(mem) = open_mem(tid) else {
            self.respond(req.id, libc::EPERM, false);
            return;
        };
        if !self.us.listener.valid(req.id) {
            return;
        }
        // Drop any earlier binder fds of this process that exec closed.
        self.release_closed(Some(pid));
        let flags = req.data.args[2] as u32;
        let name = format!("innkeeper-binder-{}", self.next_memfd);
        self.next_memfd += 1;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let Some(memfd) =
            recv_memfd(&name).filter(|m| unsafe { libc::fstat(m.as_raw_fd(), &mut st) } == 0)
        else {
            self.respond(req.id, libc::ENOMEM, false);
            return;
        };
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                BINDER_MEMFD_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                memfd.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            self.respond(req.id, libc::ENOMEM, false);
            return;
        }
        let mut ps = ProcState {
            pid,
            fd: 0,
            memfd,
            memfd_ino: st.st_ino,
            mem,
            map: map as *mut u8,
            base: None,
            free: Vec::new(),
            live: HashMap::new(),
            pidfd: pidfd_open(pid),
            poll: None,
            poll_ready: false,
        };
        // ADDFD+SEND injects the memfd and completes the openat with its number.
        let newfd = self.us.listener.addfd(&SeccompNotifAddfd {
            id: req.id,
            flags: SECCOMP_ADDFD_FLAG_SEND,
            srcfd: ps.memfd.as_raw_fd() as u32,
            newfd: 0,
            newfd_flags: flags & O_CLOEXEC,
        });
        if newfd < 0 {
            return; // target gone
        }
        // Record the opener's virtual euid, as the kernel records proc->cred
        // at open.
        let euid = self.vid.creds(pid).map_or(0, |c| c.euid);
        let procid = self.broker.open(pid, euid, ctx, flags & O_NONBLOCK != 0);
        if self.guest == Guest::Ohos {
            self.us.tokens.resolve(pid, &crate::ohos::proc_stat);
        }
        ps.fd = newfd as u32;
        self.us.procs.insert(procid, ps);
        self.fd_index.insert((pid, newfd as u32), procid);
        let _ = self.watch.send(pid);
        trace!("open pid={pid} euid={euid} ctx={ctx} fd={newfd} proc={procid}");
    }

    fn handle_ioctl(&mut self, req: SeccompNotif) {
        let fd = req.data.args[0] as u32;
        let cmd = req.data.args[1] as u32;
        if self.guest == Guest::Ohos && crate::ohos::is_token_ioctl(cmd) {
            return self.token_ioctl(req);
        }
        if self.guest == Guest::Ohos && crate::ashmem::is_ashmem_ioctl(cmd) {
            return self.ashmem_ioctl(req);
        }
        let procid = match self
            .tgid(req.pid as i32)
            .and_then(|pid| self.fd_index.get(&(pid, fd)))
        {
            Some(&p) if self.broker.proc_exists(p) => p,
            _ => {
                self.respond(req.id, 0, true);
                return;
            }
        };
        let call = Call {
            id: req.id,
            arg: req.data.args[2],
        };
        self.broker.ioctl(procid, req.pid, cmd, call, &mut self.us);
    }

    /// Serves an OpenHarmony access-token ioctl on [`crate::ohos::NODE`].
    /// Other type-`'A'` ioctls, such as ALSA's, go through.
    fn token_ioctl(&mut self, req: SeccompNotif) {
        let tid = req.pid as i32;
        let [fd, cmd, arg, ..] = req.data.args;
        let node = std::fs::read_link(format!("/proc/{tid}/fd/{fd}"))
            .is_ok_and(|p| p.as_os_str() == crate::ohos::NODE);
        let (Some(pid), true) = (self.tgid(tid), node) else {
            return self.respond(req.id, 0, true);
        };
        let Some(mem) = open_mem(tid) else {
            return self.respond(req.id, libc::EPERM, false);
        };
        struct Mem(MemFd, u64);
        impl crate::ohos::Arg for Mem {
            fn read(&mut self, len: usize) -> Option<Vec<u8>> {
                mem_read(self.0, self.1, len)
            }
            fn write(&mut self, data: &[u8]) -> bool {
                mem_write(self.0, self.1, data)
            }
        }
        let r = self.us.tokens.ioctl(
            pid,
            cmd as u32,
            &crate::ohos::proc_stat,
            &mut Mem(mem.raw(), arg),
        );
        trace!("token pid={pid} cmd={cmd:#x} {r:?}");
        let (val, errno) = r.map_or_else(|e| (0, e), |v| (v, 0));
        self.us.listener.reply(req.id, val, errno);
    }

    /// Serves an ashmem ioctl on one of the broker's regions
    /// ([`crate::ashmem`]). Other type-0x77 ioctls go through.
    fn ashmem_ioctl(&mut self, req: SeccompNotif) {
        let tid = req.pid as i32;
        let [fd, cmd, arg, ..] = req.data.args;
        let link = format!("/proc/{tid}/fd/{fd}");
        if !std::fs::read_link(&link).is_ok_and(|p| {
            crate::ashmem::is_region(std::os::unix::ffi::OsStrExt::as_bytes(p.as_os_str()))
        }) {
            return self.respond(req.id, 0, true);
        }
        let (Ok(region), Some(mem)) = (
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&link),
            open_mem(tid),
        ) else {
            return self.respond(req.id, libc::EBADF, false);
        };
        let r = crate::ashmem::ioctl(&region, cmd as u32, arg, &mut |b| {
            mem_write(mem.raw(), arg, b)
        });
        trace!("ashmem tid={tid} cmd={cmd:#x} {r:?}");
        let (val, errno) = r.map_or_else(|e| (0, e), |v| (v, 0));
        self.us.listener.reply(req.id, val, errno);
    }

    fn release_proc(&mut self, procid: ProcId) {
        self.broker.release_proc(procid, &mut self.us);
        if let Some(p) = self.us.procs.remove(&procid) {
            self.fd_index.remove(&(p.pid, p.fd));
        }
        trace!("release proc={procid}");
    }

    fn release_pid(&mut self, pid: i32) {
        let ids: Vec<ProcId> = self
            .us
            .procs
            .iter()
            .filter(|(_, p)| p.pid == pid)
            .map(|(&k, _)| k)
            .collect();
        for id in ids {
            self.release_proc(id);
        }
        self.tgids.retain(|_, t| *t != pid);
    }

    /// Releases the broker processes of `pid`, or of every process if `None`,
    /// that no longer hold their binder fd.
    fn release_closed(&mut self, pid: Option<i32>) {
        let gone: Vec<ProcId> = self
            .us
            .procs
            .iter()
            .filter(|(_, p)| pid.is_none_or(|pid| p.pid == pid) && !p.still_open())
            .map(|(&k, _)| k)
            .collect();
        for id in gone {
            self.release_proc(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The start of `pid`'s first mapping, an address that is mapped.
    fn first_mapping(pid: i32) -> u64 {
        let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
        let start = maps.split('-').next().unwrap();
        u64::from_str_radix(start, 16).unwrap()
    }

    /// A process's pinned mem fd doesn't reach the image it exec'd into,
    /// though its numeric pid does. The broker's writes after a failed
    /// response, or racing an exec, must not land there. The binder path and
    /// the credential syscalls' workers use the pinned fd to read paths and
    /// write lstat results.
    #[test]
    fn pinned_mem_does_not_follow_exec() {
        use std::io::Write;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read x; exec sleep 10"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let mem = open_mem(pid).unwrap();
        let before = first_mapping(pid);
        assert!(mem_read(mem.raw(), before, 8).is_some());
        // The ELF header there has a NUL early on.
        assert!(mem_read_path(mem.raw(), before).is_some());
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        let exe = |p: i32| std::fs::read_link(format!("/proc/{p}/exe")).unwrap_or_default();
        for _ in 0..500 {
            if exe(pid).ends_with("sleep") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(exe(pid).ends_with("sleep"));
        let after = first_mapping(pid);
        assert!(vm_read(pid, after, 8).is_some());
        assert!(read_path(pid, after).is_some());
        assert!(mem_read(mem.raw(), after, 8).is_none());
        assert!(mem_read_path(mem.raw(), after).is_none());
        assert!(!mem_write(mem.raw(), after, b"x"));
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
