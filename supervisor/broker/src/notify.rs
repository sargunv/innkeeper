//! Seccomp user-notification binding: serves the [`BinderDriver`] to real
//! Android processes in a Linux.apk session.
//!
//! The session's `pivot-exec` installs one seccomp filter on Android init
//! (inherited by every process) that turns `bpf()`, `openat()` and binder
//! `ioctl()`s into user notifications, all received by [`serve`] in this
//! helper process outside the Android namespaces. It forwards the bpf ones to
//! the C supervisor (`bpfemu.c`) over a SOCK_SEQPACKET socket.
//!
//!  * `openat` of a binder device path -> inject a memfd (`ADDFD`+`SEND`); the
//!    process mmaps it as its receive region, and the broker writes
//!    transactions there through its own shared mapping.
//!  * `ioctl(fd, BINDER_*, argp)` -> [`BinderDriver::ioctl`], which reaches
//!    the caller's memory through its pinned `/proc/<tid>/mem` (see
//!    [`PinnedMem`]) and answers the notification itself: at once, or for a
//!    blocking read once there is work. Waits stay interruptible (ptrace and
//!    job control must work): a signal ends the notification and the kernel
//!    restarts the ioctl, which the driver makes idempotent.
//!  * a thread that sets up polling (`IPCThreadState::setupPolling`) gets its
//!    process's binder fd swapped for an epoll fd (`ADDFD`+`SETFD`; the mapping
//!    keeps the memfd alive) watching a broker-only eventfd whose level the
//!    broker drives (`binder_poll`).
//!
//! A monitor thread watches a pidfd per process (death) and periodically
//! checks with `kcmp` that each process still holds its binder fd (close,
//! exec), releasing the broker process otherwise, which fires death
//! notifications.

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::binder::driver::{BinderDriver, Call, InstallError, ProcId, Userspace};
use crate::binder::uapi::*;
use crate::{DEBUG, log};

macro_rules! dlog {
    ($($a:tt)*) => {
        if DEBUG.load(Ordering::Relaxed) {
            log(format_args!("broker: {}", format_args!($($a)*)));
        }
    };
}

// ---- seccomp uapi (x86_64) --------------------------------------------

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
pub(crate) fn notif_addfd_cmd(sz: usize) -> u32 {
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

/// Size of each receive memfd. The kernel caps binder mappings at 4 MiB; the
/// memfd is sparse, and the broker only hands out offsets inside the part the
/// process actually mapped (libbinder: 1 MiB - 2 pages).
const BINDER_MEMFD_SIZE: usize = 4 * 1024 * 1024;

// ---- per-process state -------------------------------------------------

struct ProcState {
    pid: i32,
    /// The binder fd number in the target.
    fd: u32,
    memfd: OwnedFd,
    memfd_ino: u64,
    /// The opener's address space, pinned while its notification was valid
    /// (see [`PinnedMem`]). All of the process's memory goes through it.
    mem: PinnedMem,
    map: *mut u8,
    /// Target mapping (base, usable length), found in /proc/<pid>/maps.
    base: Option<(u64, usize)>,
    free: Vec<(usize, usize)>,
    live: HashMap<u64, (usize, usize)>,
    pidfd: Option<OwnedFd>,
    /// Present once a thread polls: the eventfd whose level the broker
    /// drives, and the epoll fd watching it, which the target holds instead
    /// of its binder fd. The target can't reach the eventfd itself, so it
    /// can't drain it, saturate it or clear its O_NONBLOCK under the broker.
    poll: Option<(OwnedFd, OwnedFd)>,
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
    /// Does the process still hold its binder fd (not closed, nor lost
    /// across exec)?
    fn still_open(&self) -> bool {
        let ours = self
            .poll
            .as_ref()
            .map_or(&self.memfd, |(_, ep)| ep)
            .as_raw_fd();
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
                || !path.starts_with("/memfd:lxbinder-")
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
        // merge with next, then previous
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
    broker: BinderDriver,
    us: SeccompUserspace,
    /// Single-uid mode's virtual ids (binder's sender_euid comes from them).
    vid: Option<Arc<crate::creds::Vid>>,
    /// (tgid, fd) -> broker process
    fd_index: HashMap<(i32, u32), ProcId>,
    tgids: HashMap<i32, i32>,
    next_memfd: u64,
    watch: mpsc::Sender<i32>,
}

// ---- libc helpers ------------------------------------------------------

/// Read memory of task `pid` by number, unpinned. Only for reads that
/// precede a SECCOMP_IOCTL_NOTIF_ID_VALID check, or that only decide whether a
/// call needs a closer look.
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

/// A task's address space, pinned: its `/proc/<tid>/mem`, which holds the mm
/// the task had when it was opened, so after an exec it reaches nothing.
/// Callers open it and then check SECCOMP_IOCTL_NOTIF_ID_VALID: an mm switch
/// (exec) or tid reuse only happens after the caller's notification is gone,
/// so a valid id afterwards means the mm is the caller's. Writes meant for a
/// caller therefore never land in another process, or in the image it exec'd.
///
/// Accesses through it use FOLL_FORCE, so unlike `process_vm_*` they also
/// reach the caller's read-only and PROT_NONE private pages (a write there
/// breaks copy-on-write). That is fine under the desktop-parity threat model
/// (docs/STRATEGY.md): the memory is the caller's own, which it can change
/// itself.
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

/// Read target memory through its pinned address space.
pub(crate) fn mem_read(m: MemFd, addr: u64, len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
    }
    let off = i64::try_from(addr).ok().filter(|&a| a != 0)?;
    let mut buf = vec![0u8; len];
    let n = unsafe { libc::pread(m.0, buf.as_mut_ptr() as *mut _, len, off) };
    (n == len as isize).then_some(buf)
}

/// Write target memory through its pinned address space.
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

/// Read a NUL-terminated path, never crossing into a page that need not be
/// mapped before it is known to be needed.
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

/// A receive-region memfd of BINDER_MEMFD_SIZE, sealed against resizing: the
/// process gets it writable, and a shrink would make the broker's writes
/// through its own mapping fault (SIGBUS).
fn recv_memfd(name: &str) -> Option<OwnedFd> {
    let cname = std::ffi::CString::new(name).ok()?;
    let fd =
        unsafe { libc::memfd_create(cname.as_ptr(), libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING) };
    if fd < 0 {
        return None;
    }
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    let seals = libc::F_SEAL_SHRINK | libc::F_SEAL_GROW | libc::F_SEAL_SEAL;
    (unsafe { libc::ftruncate(fd.as_raw_fd(), BINDER_MEMFD_SIZE as i64) } == 0
        && unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_ADD_SEALS, seals) } == 0)
        .then_some(fd)
}

/// A non-blocking eventfd and an epoll fd that watches it (see
/// `ProcState::poll_evfd`).
fn poll_pair() -> Option<(OwnedFd, OwnedFd)> {
    let efd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if efd < 0 {
        return None;
    }
    let efd = unsafe { OwnedFd::from_raw_fd(efd) };
    let epfd = unsafe { libc::epoll_create1(libc::EPOLL_CLOEXEC) };
    if epfd < 0 {
        return None;
    }
    let epfd = unsafe { OwnedFd::from_raw_fd(epfd) };
    let mut ev = libc::epoll_event {
        events: libc::EPOLLIN as u32,
        u64: 0,
    };
    let r = unsafe {
        libc::epoll_ctl(
            epfd.as_raw_fd(),
            libc::EPOLL_CTL_ADD,
            efd.as_raw_fd(),
            &mut ev,
        )
    };
    (r == 0).then_some((efd, epfd))
}

// ---- Userspace over real processes ------------------------------------

/// The Android processes the driver serves, reached through their pinned
/// memory, their receive memfds and the seccomp listener.
struct SeccompUserspace {
    listener: RawFd,
    resp_sz: usize,
    addfd_sz: usize,
    procs: HashMap<ProcId, ProcState>,
    captured: HashMap<u64, OwnedFd>,
    next_token: u64,
}

impl SeccompUserspace {
    fn addfd(&self, a: &SeccompNotifAddfd) -> i32 {
        unsafe {
            libc::ioctl(
                self.listener,
                notif_addfd_cmd(self.addfd_sz) as _,
                a as *const _,
            )
        }
    }
}

impl Userspace for SeccompUserspace {
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
        let r = self.addfd(&SeccompNotifAddfd {
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
        send_resp(self.listener, self.resp_sz, call.id, 0, -errno, false)
    }
    fn make_pollable(&mut self, proc: ProcId, call: Call) {
        let Some(p) = self.procs.get(&proc) else {
            return;
        };
        let Some((efd, epfd)) = poll_pair() else {
            return;
        };
        let r = self.addfd(&SeccompNotifAddfd {
            id: call.id,
            flags: SECCOMP_ADDFD_FLAG_SETFD,
            srcfd: epfd.as_raw_fd() as u32,
            newfd: p.fd,
            newfd_flags: O_CLOEXEC,
        });
        if r < 0 {
            dlog!("poll swap failed proc={proc}");
            return;
        }
        dlog!("poll swap proc={proc} fd={}", p.fd);
        let p = self.procs.get_mut(&proc).unwrap();
        p.poll = Some((efd, epfd));
        p.poll_ready = false;
    }
    fn set_poll_ready(&mut self, proc: ProcId, ready: bool) {
        let Some(p) = self.procs.get_mut(&proc) else {
            return;
        };
        let Some((efd, _)) = p.poll.as_ref() else {
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
}

// ---- the serve loop ----------------------------------------------------

/// Receive every notification on `listener` (the session's single seccomp
/// listener), serve `openat` and binder `ioctl`s, and forward the rest (bpf
/// and friends, rare) to the C supervisor thread over `sock` as one
/// `seccomp_notif` per SOCK_SEQPACKET message. Called by the broker helper
/// (`src/main.rs`).
pub fn serve(sock: RawFd, listener: RawFd) {
    crate::init_log();
    let mut sizes = SeccompNotifSizes::default();
    let r = unsafe { libc::syscall(libc::SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &mut sizes) };
    if r != 0 {
        eprintln!("broker: SECCOMP_GET_NOTIF_SIZES failed");
        return;
    }
    let notif_sz = (sizes.seccomp_notif as usize).max(size_of::<SeccompNotif>());
    let resp_sz = (sizes.seccomp_notif_resp as usize).max(size_of::<SeccompNotifResp>());
    let (tx, rx) = mpsc::channel();
    // Single-uid mode (docs/sandbox.md): virtual ids for every process.
    let vid = (std::env::var("LINUXAPK_SINGLE_UID").as_deref() == Ok("1"))
        .then(|| crate::creds::Vid::new(listener, resp_sz));
    let sup = Arc::new(Mutex::new(Supervisor {
        broker: BinderDriver::new(),
        us: SeccompUserspace {
            listener,
            resp_sz,
            addfd_sz: size_of::<SeccompNotifAddfd>(),
            procs: HashMap::new(),
            captured: HashMap::new(),
            next_token: 0,
        },
        vid: vid.clone(),
        fd_index: HashMap::new(),
        tgids: HashMap::new(),
        next_memfd: 1,
        watch: tx,
    }));
    {
        let sup = Arc::clone(&sup);
        std::thread::spawn(move || monitor(sup, rx));
    }
    let recv_cmd = ioc(3, 0, notif_sz);
    let mut notif = vec![0u8; notif_sz];
    loop {
        notif.fill(0);
        if unsafe { libc::ioctl(listener, recv_cmd as _, notif.as_mut_ptr()) } != 0 {
            let e = std::io::Error::last_os_error();
            if matches!(e.raw_os_error(), Some(libc::EINTR) | Some(libc::ENOENT)) {
                continue;
            }
            eprintln!("broker: NOTIF_RECV: {e}");
            return;
        }
        let req: SeccompNotif = unsafe { std::ptr::read_unaligned(notif.as_ptr() as *const _) };
        let nr = req.data.nr as i64;
        if let Some(v) = &vid
            && crate::creds::is_cred_syscall(nr, &req.data.args)
        {
            v.serve(&req);
            continue;
        }
        if !is_binder_syscall(nr, &req.data.args) {
            // Never wait for the C supervisor: if it is stuck (or behind),
            // binder must keep working, so the forwarded call fails instead.
            let flags = libc::MSG_DONTWAIT;
            if unsafe { libc::send(sock, notif.as_ptr() as *const _, notif_sz, flags) } < 0 {
                eprintln!("broker: forward: {}", std::io::Error::last_os_error());
                send_resp(listener, resp_sz, req.id, 0, -libc::EAGAIN, false);
            }
            continue;
        }
        // Single-uid mode: /proc/<pid>/status with the virtual ids.
        if nr == libc::SYS_openat
            && let Some(v) = &vid
            && v.proc_status(&req)
        {
            continue;
        }
        // Every openat in the session lands here: let the ones that can't be a
        // binder device through without taking the lock.
        if nr == libc::SYS_openat && !maybe_dev_path(req.pid as i32, req.data.args[1]) {
            send_continue(listener, resp_sz, req.id);
            continue;
        }
        sup.lock().unwrap().dispatch(req);
    }
}

/// Is this notification the broker's to serve (every `openat`, and binder
/// `ioctl`s) rather than the C supervisor's?
fn is_binder_syscall(nr: i64, args: &[u64; 6]) -> bool {
    nr == libc::SYS_openat || (nr == libc::SYS_ioctl && is_binder_ioctl(args[1] as u32))
}

/// Could the path at `addr` start with "/dev/"? (Reads at most to the end of
/// its page.)
fn maybe_dev_path(pid: i32, addr: u64) -> bool {
    let n = (4096 - (addr as usize & 4095)).min(5);
    match vm_read(pid, addr, n) {
        Some(b) => b"/dev/".starts_with(&b) || b == b"/dev/",
        None => false,
    }
}

/// Is notification `id` still pending (its task still in the syscall)?
pub(crate) fn notif_id_valid(listener: RawFd, id: u64) -> bool {
    unsafe { libc::ioctl(listener, notif_id_valid_cmd() as _, &id as *const u64) == 0 }
}

fn send_continue(listener: RawFd, resp_sz: usize, id: u64) {
    send_resp(listener, resp_sz, id, 0, 0, true);
}

/// Answer notification `id` (`error` is a negative errno, or 0). False if it
/// is gone (a signal ended its wait, or its task died).
pub(crate) fn send_resp(
    listener: RawFd,
    resp_sz: usize,
    id: u64,
    val: i64,
    error: i32,
    cont: bool,
) -> bool {
    let resp = SeccompNotifResp {
        id,
        val,
        error,
        flags: if cont {
            SECCOMP_USER_NOTIF_FLAG_CONTINUE
        } else {
            0
        },
    };
    let mut buf = vec![0u8; resp_sz];
    unsafe {
        std::ptr::write_unaligned(buf.as_mut_ptr() as *mut SeccompNotifResp, resp);
        libc::ioctl(listener, notif_send_cmd(resp_sz) as _, buf.as_mut_ptr()) == 0
    }
}

/// Watch for process death (pidfds) and for binder fds that were closed or
/// lost across exec (kcmp sweep), and release those broker processes.
fn monitor(sup: Arc<Mutex<Supervisor>>, rx: mpsc::Receiver<i32>) {
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
        unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as _, 100) };
        let dead: Vec<i32> = pfds
            .iter()
            .zip(&pids)
            .filter(|(p, _)| p.revents != 0)
            .map(|(_, &pid)| pid)
            .collect();
        let sweep = last_sweep.elapsed() >= std::time::Duration::from_millis(500);
        if dead.is_empty() && !sweep {
            continue;
        }
        let mut s = sup.lock().unwrap();
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
    /// Answer a notification that isn't a binder ioctl (`errno` positive).
    fn respond(&self, id: u64, errno: i32, cont: bool) {
        send_resp(self.us.listener, self.us.resp_sz, id, 0, -errno, cont);
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
        // asked (so the path read above was its too).
        let Some(mem) = open_mem(tid) else {
            self.respond(req.id, libc::EPERM, false);
            return;
        };
        if !notif_id_valid(self.us.listener, req.id) {
            return;
        }
        // Drop any earlier binder fds of this process that are gone (exec).
        self.release_closed(Some(pid));
        let flags = req.data.args[2] as u32;
        let name = format!("lxbinder-{}", self.next_memfd);
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
        let newfd = self.us.addfd(&SeccompNotifAddfd {
            id: req.id,
            flags: SECCOMP_ADDFD_FLAG_SEND,
            srcfd: ps.memfd.as_raw_fd() as u32,
            newfd: 0,
            newfd_flags: flags & O_CLOEXEC,
        });
        if newfd < 0 {
            return; // target gone
        }
        // The opener's euid, as the kernel records proc->cred at open; in
        // single-uid mode its virtual one.
        let euid = match &self.vid {
            Some(v) => v.creds(pid).map(|c| c.euid).unwrap_or(0),
            None => status_field(pid, "Uid:")
                .and_then(|u| u.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
                .unwrap_or(0),
        };
        // NSpid: <this ns> <android ns> [...]; report the Android-namespace pid.
        let ns_pid = status_field(pid, "NSpid:")
            .and_then(|v| {
                let f: Vec<&str> = v.split_whitespace().collect();
                f.get(1).or(f.first()).and_then(|x| x.parse().ok())
            })
            .unwrap_or(pid);
        let procid = self.broker.open(ns_pid, euid, ctx, flags & O_NONBLOCK != 0);
        ps.fd = newfd as u32;
        self.us.procs.insert(procid, ps);
        self.fd_index.insert((pid, newfd as u32), procid);
        let _ = self.watch.send(pid);
        dlog!("open pid={pid} nspid={ns_pid} euid={euid} ctx={ctx} fd={newfd} proc={procid}");
    }

    fn handle_ioctl(&mut self, req: SeccompNotif) {
        let fd = req.data.args[0] as u32;
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
        let cmd = req.data.args[1] as u32;
        self.broker.ioctl(procid, req.pid, cmd, call, &mut self.us);
    }

    fn release_proc(&mut self, procid: ProcId) {
        self.broker.release_proc(procid, &mut self.us);
        if let Some(p) = self.us.procs.remove(&procid) {
            self.fd_index.remove(&(p.pid, p.fd));
        }
        dlog!("release proc={procid}");
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

    /// Release the broker processes of `pid` (all if `None`) that no longer
    /// hold their binder fd.
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

    /// Android gets the receive memfd writable; it must not be able to resize
    /// it under the broker's mapping.
    #[test]
    fn recv_memfd_is_sealed() {
        let m = recv_memfd("lxbinder-test").unwrap();
        let fd = m.as_raw_fd();
        assert_eq!(unsafe { libc::ftruncate(fd, 0) }, -1);
        assert_eq!(
            unsafe { libc::ftruncate(fd, 2 * BINDER_MEMFD_SIZE as i64) },
            -1
        );
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        assert_eq!(unsafe { libc::fstat(fd, &mut st) }, 0);
        assert_eq!(st.st_size as usize, BINDER_MEMFD_SIZE);
        // Writes through mappings still work.
        assert_eq!(
            unsafe { libc::pwrite(fd, b"x".as_ptr() as *const _, 1, 0) },
            1
        );
    }

    fn readable(fd: RawFd) -> bool {
        let mut p = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        unsafe { libc::poll(&mut p, 1, 0) == 1 }
    }

    /// The target polls the epoll fd; whatever it does to that fd, the
    /// broker's eventfd stays non-blocking and only the broker drains it.
    #[test]
    fn poll_fd_is_not_the_brokers() {
        let (efd, epfd) = poll_pair().unwrap();
        let (e, ep) = (efd.as_raw_fd(), epfd.as_raw_fd());
        assert!(!readable(ep));
        let one = 1u64;
        assert_eq!(
            unsafe { libc::write(e, &one as *const u64 as *const _, 8) },
            8
        );
        assert!(readable(ep));
        // The target clears O_NONBLOCK and tries to drain or saturate it.
        assert_eq!(unsafe { libc::fcntl(ep, libc::F_SETFL, 0) }, 0);
        let mut b = [0u8; 8];
        assert_eq!(unsafe { libc::read(ep, b.as_mut_ptr() as *mut _, 8) }, -1);
        let max = u64::MAX - 1;
        assert_eq!(
            unsafe { libc::write(ep, &max as *const u64 as *const _, 8) },
            -1
        );
        assert!(readable(ep));
        // The broker lowers the level, twice: never blocks.
        assert_eq!(unsafe { libc::read(e, b.as_mut_ptr() as *mut _, 8) }, 8);
        assert_eq!(unsafe { libc::read(e, b.as_mut_ptr() as *mut _, 8) }, -1);
        assert!(!readable(ep));
    }

    /// The first mapping of `pid` (an address that is mapped).
    fn first_mapping(pid: i32) -> u64 {
        let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
        let start = maps.split('-').next().unwrap();
        u64::from_str_radix(start, 16).unwrap()
    }

    /// A process's pinned mem fd never reaches the image it exec'd into,
    /// though its numeric pid does (the broker's writes after a failed
    /// response, or racing an exec, must not land there). The binder path
    /// and the credential syscalls' workers (paths read, lstat results written) use it.
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
        // (The ELF header there has a NUL early on.)
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
