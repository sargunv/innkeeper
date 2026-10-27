//! Seccomp user-notification binding: drives the [`crate::Broker`] against real
//! Android processes in a Linux.apk session.
//!
//! The session's `pivot-exec` installs one seccomp filter on Android init
//! (inherited by every process) that turns `bpf()`, `openat()` and binder
//! `ioctl()`s into user notifications. Its C supervisor (`bpfemu.c`) forwards
//! the binder ones over a SOCK_SEQPACKET socket to this serve loop, which runs
//! in a separate helper process outside the Android namespaces with the host
//! user's power, so every byte from Android is hostile.
//!
//!  * `openat` of a binder device path -> inject a memfd (`ADDFD`+`SEND`); the
//!    process mmaps it as its receive region, and the broker writes
//!    transactions there through its own shared mapping.
//!  * `ioctl(fd, BINDER_*, argp)` -> serviced via `process_vm_{readv,writev}`.
//!  * a blocking `BINDER_WRITE_READ` with no work is parked: we keep the
//!    notification id and answer it when the broker wakes the thread. Waits
//!    are interruptible (ptrace and job control must work), so a signal can
//!    cancel a notification and the kernel restarts the ioctl. That is made
//!    idempotent: `write_consumed` is written back before parking, and read
//!    bytes whose response failed (ENOENT) are stashed and returned by the
//!    thread's next `BINDER_WRITE_READ`.
//!  * a thread that sets up polling (`IPCThreadState::setupPolling`) gets its
//!    process's binder fd swapped for an epoll fd (`ADDFD`+`SETFD`; the mapping
//!    keeps the memfd alive) watching a broker-only eventfd whose level the
//!    broker drives (`binder_poll`).
//!
//! A monitor thread watches a pidfd per process (death) and periodically
//! checks with `kcmp` that each process still holds its binder fd (close,
//! exec), releasing the broker process otherwise, which fires death
//! notifications.
#![cfg(any(target_os = "linux", target_os = "android"))]
#![allow(clippy::missing_safety_doc)]

use std::collections::{HashMap, HashSet};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};

use crate::broker::{Broker, Mem, ProcId, Wakeup};
use crate::defs::*;
use crate::{DEBUG, log};

macro_rules! dlog {
    ($($a:tt)*) => {
        if DEBUG.load(Ordering::Relaxed) {
            log(format_args!("ubinder: {}", format_args!($($a)*)));
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
/// Largest write buffer accepted in one BINDER_WRITE_READ.
const MAX_WRITE: usize = 16 * 1024 * 1024;

// ---- per-process state -------------------------------------------------

struct ProcState {
    pid: i32,
    /// The binder fd number in the target.
    fd: u32,
    memfd: OwnedFd,
    memfd_ino: u64,
    /// The opener's address space, pinned while its notification was valid,
    /// so a numeric pid that exec'd or was recycled is never written. All
    /// of the process's memory goes through it.
    mem: PinnedMem,
    map: *mut u8,
    /// Target mapping (base, usable length), found in /proc/<pid>/maps.
    base: Option<(u64, usize)>,
    free: Vec<(usize, usize)>,
    live: HashMap<u64, (usize, usize)>,
    nonblock: bool,
    pidfd: Option<OwnedFd>,
    /// Present once a thread polls: the eventfd whose level the broker
    /// drives, and the epoll fd watching it, which the target holds instead
    /// of its binder fd. The target can't reach the eventfd itself, so it
    /// can't drain it, saturate it or clear its O_NONBLOCK under the broker.
    poll_evfd: Option<(OwnedFd, OwnedFd)>,
    poll_level: bool,
}

// The raw mapping pointer is owned by the supervisor under its lock.
unsafe impl Send for ProcState {}

impl Drop for ProcState {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.map as *mut _, BINDER_MEMFD_SIZE) };
    }
}

impl ProcState {
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

struct Waiter {
    notif_id: u64,
    bwr_addr: u64,
    bwr: BinderWriteRead,
}

pub struct Supervisor {
    broker: Broker,
    /// Single-uid mode's virtual ids (binder's sender_euid comes from them).
    vid: Option<Arc<crate::vidsys::Vid>>,
    listener: RawFd,
    resp_sz: usize,
    addfd_sz: usize,
    procs: HashMap<ProcId, ProcState>,
    /// (tgid, fd) -> broker process
    fd_index: HashMap<(i32, u32), ProcId>,
    waiters: HashMap<(ProcId, u32), Waiter>,
    captured: HashMap<u64, OwnedFd>,
    next_token: u64,
    tgids: HashMap<i32, i32>,
    next_memfd: u64,
    watch: mpsc::Sender<i32>,
    /// Wakeups still to deliver.
    pending: Vec<Wakeup>,
    /// Processes whose poll eventfd needs re-evaluating.
    dirty: HashSet<ProcId>,
    /// Read bytes a thread never received (its notification was interrupted).
    stash: HashMap<(ProcId, u32), Vec<u8>>,
    /// Tests: every response sent, as (id, error).
    #[cfg(test)]
    sent: std::cell::RefCell<Vec<(u64, i32)>>,
}

unsafe impl Send for Supervisor {}

// ---- libc helpers ------------------------------------------------------

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

/// A task's address space, pinned: `/proc/<tid>/mem` and `/proc/<tid>/maps`,
/// opened from one `/proc/<tid>` directory. Both hold the mm the task had
/// when they were opened, so after an exec they reach nothing. Callers open
/// it and then check SECCOMP_IOCTL_NOTIF_ID_VALID: an mm switch (exec) or tid
/// reuse only happens after the caller's notification is gone, so a valid
/// id afterwards means both are the caller's.
pub(crate) struct PinnedMem {
    mem: OwnedFd,
    maps: OwnedFd,
}

/// The raw fds of a [`PinnedMem`], for passing around under a borrow.
#[derive(Clone, Copy)]
pub(crate) struct MemFds {
    mem: RawFd,
    maps: RawFd,
}

impl PinnedMem {
    pub(crate) fn raw(&self) -> MemFds {
        MemFds {
            mem: self.mem.as_raw_fd(),
            maps: self.maps.as_raw_fd(),
        }
    }
}

pub(crate) fn open_mem(tid: i32) -> Option<PinnedMem> {
    let dir = std::ffi::CString::new(format!("/proc/{tid}")).ok()?;
    let d = unsafe {
        libc::open(
            dir.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if d < 0 {
        return None;
    }
    let d = unsafe { OwnedFd::from_raw_fd(d) };
    let at = |name: &std::ffi::CStr, flags| {
        let fd = unsafe { libc::openat(d.as_raw_fd(), name.as_ptr(), flags | libc::O_CLOEXEC) };
        (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
    };
    Some(PinnedMem {
        mem: at(c"mem", libc::O_RDWR)?,
        maps: at(c"maps", libc::O_RDONLY)?,
    })
}

const PROCMAP_QUERY: u64 = 0xc068_6611; // _IOWR('f', 17, struct procmap_query)
const VMA_READABLE: u64 = 1;
const VMA_WRITABLE: u64 = 2;

#[repr(C)]
#[derive(Default)]
struct ProcmapQuery {
    size: u64,
    query_flags: u64,
    query_addr: u64,
    vma_start: u64,
    vma_end: u64,
    vma_flags: u64,
    vma_page_size: u64,
    vma_offset: u64,
    inode: u64,
    dev_major: u32,
    dev_minor: u32,
    vma_name_size: u32,
    build_id_size: u32,
    vma_name_addr: u64,
    build_id_addr: u64,
}

/// Is [addr, addr+len) mapped with `need` (VMA_READABLE/VMA_WRITABLE) in the
/// pinned mm? `/proc/<pid>/mem` accesses use FOLL_FORCE, which ignores page
/// protections, so this is what keeps them to what the task could access
/// itself (and what process_vm_* and the kernel's copy_to_user allow).
fn accessible(maps: RawFd, addr: u64, len: usize, need: u64) -> bool {
    let Some(end) = addr.checked_add(len as u64) else {
        return false;
    };
    let mut a = addr;
    while a < end {
        let mut q = ProcmapQuery {
            size: size_of::<ProcmapQuery>() as u64,
            query_addr: a,
            ..Default::default()
        };
        if unsafe { libc::ioctl(maps, PROCMAP_QUERY as _, &mut q) } != 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOTTY) {
                return accessible_text(maps, addr, end, need);
            }
            return false;
        }
        if q.vma_start > a || q.vma_flags & need != need {
            return false;
        }
        a = q.vma_end;
    }
    true
}

/// `accessible` on kernels before 6.11 (no PROCMAP_QUERY): the maps text of
/// the same pinned mm.
fn accessible_text(maps: RawFd, addr: u64, end: u64, need: u64) -> bool {
    let mut text = Vec::new();
    let mut buf = [0u8; 16384];
    let mut off = 0i64;
    loop {
        let n = unsafe { libc::pread(maps, buf.as_mut_ptr() as *mut _, buf.len(), off) };
        if n <= 0 {
            break;
        }
        text.extend_from_slice(&buf[..n as usize]);
        off += n as i64;
    }
    let mut a = addr;
    for line in String::from_utf8_lossy(&text).lines() {
        let mut it = line.split_ascii_whitespace();
        let (Some(range), Some(perms)) = (it.next(), it.next()) else {
            continue;
        };
        let Some((s, e)) = range.split_once('-') else {
            continue;
        };
        let (Ok(s), Ok(e)) = (u64::from_str_radix(s, 16), u64::from_str_radix(e, 16)) else {
            continue;
        };
        if e <= a {
            continue;
        }
        let p = perms.as_bytes();
        let ok = (need & VMA_READABLE == 0 || p.first() == Some(&b'r'))
            && (need & VMA_WRITABLE == 0 || p.get(1) == Some(&b'w'));
        if s > a || !ok {
            return false;
        }
        a = e;
        if a >= end {
            return true;
        }
    }
    false
}

/// Read target memory through its pinned address space (see [`PinnedMem`]),
/// only where the task could read it itself.
pub(crate) fn mem_read(m: MemFds, addr: u64, len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
    }
    let off = i64::try_from(addr).ok().filter(|&a| a != 0)?;
    if !accessible(m.maps, addr, len, VMA_READABLE) {
        return None;
    }
    let mut buf = vec![0u8; len];
    let n = unsafe { libc::pread(m.mem, buf.as_mut_ptr() as *mut _, len, off) };
    (n == len as isize).then_some(buf)
}

/// Write target memory through its pinned address space, only where the
/// task could write it itself.
pub(crate) fn mem_write(m: MemFds, addr: u64, data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    let Some(off) = i64::try_from(addr).ok().filter(|&a| a != 0) else {
        return false;
    };
    if !accessible(m.maps, addr, data.len(), VMA_WRITABLE) {
        return false;
    }
    let n = unsafe { libc::pwrite(m.mem, data.as_ptr() as *const _, data.len(), off) };
    n == data.len() as isize
}

/// Read a NUL-terminated path, never crossing into a page that need not be
/// mapped before it is known to be needed.
pub(crate) fn read_path(pid: i32, addr: u64) -> Option<Vec<u8>> {
    read_path_with(addr, |a, n| vm_read(pid, a, n))
}

/// `read_path` through a pinned `/proc/<pid>/mem` fd.
pub(crate) fn mem_read_path(mem: MemFds, addr: u64) -> Option<Vec<u8>> {
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

// ---- Mem over real processes ------------------------------------------

struct SysMem<'a> {
    procs: &'a mut HashMap<ProcId, ProcState>,
    listener: RawFd,
    addfd_sz: usize,
    captured: &'a mut HashMap<u64, OwnedFd>,
    next_token: &'a mut u64,
    /// The reading thread's process and outstanding notification (for ADDFD).
    reader: Option<(ProcId, u64)>,
}

impl Mem for SysMem<'_> {
    fn read(&mut self, proc: ProcId, addr: u64, len: usize) -> Option<Vec<u8>> {
        mem_read(self.procs.get(&proc)?.mem.raw(), addr, len)
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
            if off
                .checked_add(data.len())
                .map(|e| e <= len)
                .unwrap_or(false)
            {
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
        let p = self.procs.get(&from)?;
        let pidfd = p.pidfd.as_ref()?;
        let got =
            unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), from_fd as i32, 0) };
        if got < 0 {
            return None;
        }
        *self.next_token += 1;
        let tok = *self.next_token;
        self.captured
            .insert(tok, unsafe { OwnedFd::from_raw_fd(got as RawFd) });
        Some(tok)
    }
    fn install_fd(&mut self, to: ProcId, token: u64) -> Option<u32> {
        let (rp, notif_id) = self.reader?;
        if rp != to {
            return None;
        }
        let fd = self.captured.get(&token)?;
        let addfd = SeccompNotifAddfd {
            id: notif_id,
            flags: 0,
            srcfd: fd.as_raw_fd() as u32,
            newfd: 0,
            newfd_flags: O_CLOEXEC,
        };
        let r = unsafe {
            libc::ioctl(
                self.listener,
                notif_addfd_cmd(self.addfd_sz) as _,
                &addfd as *const _,
            )
        };
        if r < 0 { None } else { Some(r as u32) }
    }
    fn drop_fd(&mut self, token: u64) {
        self.captured.remove(&token);
    }
}

// ---- the serve loop ----------------------------------------------------

/// Receive every notification on `listener` (the session's single seccomp
/// listener), serve `openat` and binder `ioctl`s, and forward the rest (bpf
/// and friends, rare) to the C supervisor thread over `sock` as one
/// `seccomp_notif` per SOCK_SEQPACKET message. Called by the broker helper
/// (`src/bin/broker.rs`).
pub fn serve(sock: RawFd, listener: RawFd) {
    crate::init_log();
    let mut sizes = SeccompNotifSizes::default();
    let r = unsafe { libc::syscall(libc::SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &mut sizes) };
    if r != 0 {
        eprintln!("ubinder: SECCOMP_GET_NOTIF_SIZES failed");
        return;
    }
    let notif_sz = (sizes.seccomp_notif as usize).max(size_of::<SeccompNotif>());
    let resp_sz = (sizes.seccomp_notif_resp as usize).max(size_of::<SeccompNotifResp>());
    let (tx, rx) = mpsc::channel();
    // Single-uid mode (docs/sandbox.md): virtual ids for every process.
    let vid = (std::env::var("LINUXAPK_SINGLE_UID").as_deref() == Ok("1"))
        .then(|| crate::vidsys::Vid::new(listener, resp_sz));
    let sup = Arc::new(Mutex::new(Supervisor {
        vid: vid.clone(),
        broker: Broker::new(),
        listener,
        resp_sz,
        addfd_sz: size_of::<SeccompNotifAddfd>(),
        procs: HashMap::new(),
        fd_index: HashMap::new(),
        waiters: HashMap::new(),
        captured: HashMap::new(),
        next_token: 0,
        tgids: HashMap::new(),
        next_memfd: 1,
        watch: tx,
        pending: Vec::new(),
        dirty: HashSet::new(),
        stash: HashMap::new(),
        #[cfg(test)]
        sent: Default::default(),
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
            eprintln!("ubinder: NOTIF_RECV: {e}");
            return;
        }
        let req: SeccompNotif = unsafe { std::ptr::read_unaligned(notif.as_ptr() as *const _) };
        let nr = req.data.nr as i64;
        if let Some(v) = &vid
            && crate::vidsys::is_vid(nr, &req.data.args)
        {
            v.serve(&req);
            continue;
        }
        let ours = nr == libc::SYS_openat
            || (nr == libc::SYS_ioctl && is_binder_ioctl(req.data.args[1] as u32));
        if !ours {
            // Never wait for the C supervisor: if it is stuck (or behind),
            // binder must keep working, so the forwarded call fails instead.
            let flags = libc::MSG_DONTWAIT;
            if unsafe { libc::send(sock, notif.as_ptr() as *const _, notif_sz, flags) } < 0 {
                eprintln!("ubinder: forward: {}", std::io::Error::last_os_error());
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

/// Answer notification `id` (`error` is a negative errno, or 0).
pub(crate) fn send_resp(
    listener: RawFd,
    resp_sz: usize,
    id: u64,
    val: i64,
    error: i32,
    cont: bool,
) {
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
        libc::ioctl(listener, notif_send_cmd(resp_sz) as _, buf.as_mut_ptr());
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
            s.sweep_closed();
            let live: HashSet<i32> = s.procs.values().map(|p| p.pid).collect();
            watched.retain(|pid, _| live.contains(pid));
        }
    }
}

impl Supervisor {
    /// Answer a notification; false if it is gone (signal or death).
    fn respond(&self, id: u64, val: i64, errno: i32, cont: bool) -> bool {
        let resp = SeccompNotifResp {
            id,
            val,
            error: if errno > 0 { -errno } else { errno },
            flags: if cont {
                SECCOMP_USER_NOTIF_FLAG_CONTINUE
            } else {
                0
            },
        };
        #[cfg(test)]
        self.sent.borrow_mut().push((id, resp.error));
        let mut buf = vec![0u8; self.resp_sz];
        unsafe {
            std::ptr::write_unaligned(buf.as_mut_ptr() as *mut SeccompNotifResp, resp);
            libc::ioctl(
                self.listener,
                notif_send_cmd(self.resp_sz) as _,
                buf.as_mut_ptr(),
            ) == 0
        }
    }

    fn id_valid(&self, id: u64) -> bool {
        notif_id_valid(self.listener, id)
    }

    fn addfd(&self, a: &SeccompNotifAddfd) -> i32 {
        unsafe {
            libc::ioctl(
                self.listener,
                notif_addfd_cmd(self.addfd_sz) as _,
                a as *const _,
            )
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
        let nr = req.data.nr as i64;
        if nr == libc::SYS_openat {
            self.handle_openat(req);
        } else if nr == libc::SYS_ioctl {
            self.handle_ioctl(req);
        } else {
            self.respond(req.id, 0, 0, true);
        }
        self.drain();
    }

    fn handle_openat(&mut self, req: SeccompNotif) {
        let tid = req.pid as i32;
        let path = read_path(tid, req.data.args[1]);
        let ctx = match path.as_deref() {
            Some(b"/dev/binder") | Some(b"/dev/binderfs/binder") => 0u8,
            Some(b"/dev/hwbinder") | Some(b"/dev/binderfs/hwbinder") => 1,
            Some(b"/dev/vndbinder") | Some(b"/dev/binderfs/vndbinder") => 2,
            _ => {
                self.respond(req.id, 0, 0, true);
                return;
            }
        };
        let Some(pid) = self.tgid(tid) else {
            {
                self.respond(req.id, 0, 0, true);
                return;
            }
        };
        // Pin the address space, then check that tid is still the task that
        // asked (so the path read above was its too).
        let Some(mem) = open_mem(tid) else {
            self.respond(req.id, 0, libc::EPERM, false);
            return;
        };
        if !self.id_valid(req.id) {
            return;
        }
        // Drop any earlier binder fds of this process that are gone (exec).
        self.sweep_pid(pid);
        let flags = req.data.args[2] as u32;
        let name = format!("lxbinder-{}", self.next_memfd);
        self.next_memfd += 1;
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let Some(memfd) =
            recv_memfd(&name).filter(|m| unsafe { libc::fstat(m.as_raw_fd(), &mut st) } == 0)
        else {
            self.respond(req.id, 0, libc::ENOMEM, false);
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
            {
                self.respond(req.id, 0, libc::ENOMEM, false);
                return;
            }
        }
        let ps = ProcState {
            pid,
            fd: 0,
            memfd,
            memfd_ino: st.st_ino,
            mem,
            map: map as *mut u8,
            base: None,
            free: Vec::new(),
            live: HashMap::new(),
            nonblock: flags & O_NONBLOCK != 0,
            pidfd: pidfd_open(pid),
            poll_evfd: None,
            poll_level: false,
        };
        // ADDFD+SEND injects the memfd and completes the openat with its number.
        let newfd = self.addfd(&SeccompNotifAddfd {
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
        let procid = self.broker.open_ns(pid, ns_pid, euid, ctx);
        let mut ps = ps;
        ps.fd = newfd as u32;
        self.procs.insert(procid, ps);
        self.fd_index.insert((pid, newfd as u32), procid);
        let _ = self.watch.send(pid);
        dlog!("open pid={pid} nspid={ns_pid} euid={euid} ctx={ctx} fd={newfd} proc={procid}");
    }

    fn handle_ioctl(&mut self, req: SeccompNotif) {
        let fd = req.data.args[0] as u32;
        let cmd = req.data.args[1] as u32;
        let argp = req.data.args[2];
        let tid = req.pid as i32;
        let procid = match self.tgid(tid).and_then(|pid| self.fd_index.get(&(pid, fd))) {
            Some(&p) if is_binder_ioctl(cmd) && self.broker.proc_exists(p) => p,
            _ => {
                self.respond(req.id, 0, 0, true);
                return;
            }
        };
        let ttid = req.pid;
        let mem = self.procs[&procid].mem.raw();
        let err = match cmd {
            BINDER_WRITE_READ => return self.handle_write_read(procid, req, argp),
            BINDER_VERSION => {
                let v = BinderVersion {
                    protocol_version: self.broker.protocol_version(),
                };
                if mem_write(mem, argp, pod_bytes(&v)) {
                    0
                } else {
                    libc::EFAULT
                }
            }
            BINDER_SET_MAX_THREADS => match mem_read(mem, argp, 4) {
                Some(b) => {
                    self.broker
                        .set_max_threads(procid, u32::from_le_bytes(b.try_into().unwrap()));
                    0
                }
                None => libc::EFAULT,
            },
            BINDER_ENABLE_ONEWAY_SPAM_DETECTION => match mem_read(mem, argp, 4) {
                Some(b) => {
                    self.broker.enable_oneway_spam_detection(
                        procid,
                        u32::from_le_bytes(b.try_into().unwrap()) != 0,
                    );
                    0
                }
                None => libc::EFAULT,
            },
            BINDER_SET_CONTEXT_MGR => -self.broker.set_context_mgr(procid, None).err().unwrap_or(0),
            BINDER_SET_CONTEXT_MGR_EXT => {
                match mem_read(mem, argp, size_of::<FlatBinderObject>())
                    .and_then(|b| read_pod::<FlatBinderObject>(&b, 0))
                {
                    Some(f) => -self
                        .broker
                        .set_context_mgr(procid, Some(f))
                        .err()
                        .unwrap_or(0),
                    None => libc::EFAULT,
                }
            }
            BINDER_THREAD_EXIT => {
                self.broker.thread_exit(procid, ttid);
                self.waiters.remove(&(procid, ttid));
                self.stash.remove(&(procid, ttid));
                0
            }
            BINDER_GET_EXTENDED_ERROR => {
                let ee = self.broker.get_extended_error(procid, ttid);
                if mem_write(mem, argp, pod_bytes(&ee)) {
                    0
                } else {
                    libc::EFAULT
                }
            }
            BINDER_GET_NODE_DEBUG_INFO => {
                match mem_read(mem, argp, size_of::<BinderNodeDebugInfo>())
                    .and_then(|b| read_pod::<BinderNodeDebugInfo>(&b, 0))
                {
                    Some(info) => {
                        let out = self.broker.node_debug_info(procid, info.ptr);
                        if mem_write(mem, argp, pod_bytes(&out)) {
                            0
                        } else {
                            libc::EFAULT
                        }
                    }
                    None => libc::EFAULT,
                }
            }
            BINDER_GET_NODE_INFO_FOR_REF => {
                match mem_read(mem, argp, size_of::<BinderNodeInfoForRef>())
                    .and_then(|b| read_pod::<BinderNodeInfoForRef>(&b, 0))
                {
                    Some(info) => match self.broker.node_info_for_ref(procid, info) {
                        Ok(out) => {
                            if mem_write(mem, argp, pod_bytes(&out)) {
                                0
                            } else {
                                libc::EFAULT
                            }
                        }
                        Err(e) => -e,
                    },
                    None => libc::EFAULT,
                }
            }
            // Freezing is unsupported: Android then disables its app freezer.
            _ => libc::EINVAL,
        };
        self.broker.ioctl_done(procid, ttid);
        self.respond(req.id, 0, err, false);
    }

    fn handle_write_read(&mut self, procid: ProcId, req: SeccompNotif, argp: u64) {
        let mem = self.procs[&procid].mem.raw();
        let Some(mut bwr) = mem_read(mem, argp, size_of::<BinderWriteRead>())
            .and_then(|b| read_pod::<BinderWriteRead>(&b, 0))
        else {
            {
                self.respond(req.id, 0, libc::EFAULT, false);
                return;
            }
        };
        let mut write = Vec::new();
        if bwr.write_size > bwr.write_consumed {
            let len = (bwr.write_size - bwr.write_consumed) as usize;
            if len > MAX_WRITE {
                {
                    self.respond(req.id, 0, libc::EINVAL, false);
                    return;
                }
            }
            match bwr
                .write_buffer
                .checked_add(bwr.write_consumed)
                .and_then(|a| mem_read(mem, a, len))
            {
                Some(w) => write = w,
                None => {
                    self.respond(req.id, 0, libc::EFAULT, false);
                    return;
                }
            }
        }
        let mut read_avail = bwr.read_size.saturating_sub(bwr.read_consumed) as usize;
        let nonblock = self.procs[&procid].nonblock;
        // Replay a read the thread never received (interrupted notification).
        let stash = match self.stash.remove(&(procid, req.pid)) {
            Some(st) if read_avail >= st.len() => {
                read_avail = 0;
                Some(st)
            }
            Some(st) => {
                self.stash.insert((procid, req.pid), st);
                None
            }
            None => None,
        };
        let mut res = self.with_mem(Some((procid, req.id)), |b, m| {
            b.write_read(procid, req.pid, &write, read_avail, nonblock, m)
        });
        if let Some(st) = stash {
            res.read = st;
        }
        bwr.write_consumed += res.write_consumed as u64;
        if res.became_poll {
            self.make_pollable(procid, req.id);
        }
        // Like binder_ioctl_write_read, a bwr that can't be written back
        // fails the ioctl with EFAULT.
        if res.write_err != 0 {
            let ok = mem_write(mem, argp, pod_bytes(&bwr));
            self.respond(
                req.id,
                0,
                if ok { res.write_err } else { libc::EFAULT },
                false,
            );
        } else if res.blocked {
            // Record progress now so a restarted ioctl never replays the
            // write. If that fails, fail now: like an interrupted wait, the
            // thread's work stays queued for its next read.
            if !mem_write(mem, argp, pod_bytes(&bwr)) {
                self.respond(req.id, 0, libc::EFAULT, false);
                self.pending.extend(res.wakeups);
                return;
            }
            self.waiters.insert(
                (procid, req.pid),
                Waiter {
                    notif_id: req.id,
                    bwr_addr: argp,
                    bwr,
                },
            );
        } else {
            self.finish_read((procid, req.pid), req.id, argp, bwr, res.read, res.eagain);
        }
        self.pending.extend(res.wakeups);
    }

    fn finish_read(
        &mut self,
        key: (ProcId, u32),
        notif_id: u64,
        argp: u64,
        bwr: BinderWriteRead,
        read: Vec<u8>,
        eagain: bool,
    ) {
        let Some(mem) = self.procs.get(&key.0).map(|p| p.mem.raw()) else {
            self.respond(notif_id, 0, libc::EBADF, false);
            return;
        };
        let at = bwr.read_buffer.wrapping_add(bwr.read_consumed);
        if !read.is_empty() && !mem_write(mem, at, &read) {
            mem_write(mem, argp, pod_bytes(&bwr));
            self.respond(notif_id, 0, libc::EFAULT, false);
            return;
        }
        let mut out = bwr;
        out.read_consumed += read.len() as u64;
        if !mem_write(mem, argp, pod_bytes(&out)) {
            // The read bytes are lost, as with the driver's failed
            // copy_to_user of the bwr.
            self.respond(notif_id, 0, libc::EFAULT, false);
            return;
        }
        if !self.respond(notif_id, 0, if eagain { libc::EAGAIN } else { 0 }, false)
            && !read.is_empty()
        {
            // The thread was interrupted after we committed work to it; it
            // will restart the ioctl. Restore its bwr and keep the bytes. (If
            // it is gone instead, `mem` reaches nothing else.)
            mem_write(mem, argp, pod_bytes(&bwr));
            dlog!(
                "stash {} bytes for proc={} tid={}",
                read.len(),
                key.0,
                key.1
            );
            self.stash.insert(key, read);
        }
    }

    fn with_mem<R>(
        &mut self,
        reader: Option<(ProcId, u64)>,
        f: impl FnOnce(&mut Broker, &mut SysMem) -> R,
    ) -> R {
        let mut mem = SysMem {
            procs: &mut self.procs,
            listener: self.listener,
            addfd_sz: self.addfd_sz,
            captured: &mut self.captured,
            next_token: &mut self.next_token,
            reader,
        };
        f(&mut self.broker, &mut mem)
    }

    /// Swap the process's binder fd for an epoll fd watching an eventfd the
    /// broker drives.
    fn make_pollable(&mut self, procid: ProcId, notif_id: u64) {
        let Some(p) = self.procs.get(&procid) else {
            return;
        };
        if p.poll_evfd.is_some() {
            return;
        }
        let Some((efd, epfd)) = poll_pair() else {
            return;
        };
        let r = self.addfd(&SeccompNotifAddfd {
            id: notif_id,
            flags: SECCOMP_ADDFD_FLAG_SETFD,
            srcfd: epfd.as_raw_fd() as u32,
            newfd: p.fd,
            newfd_flags: O_CLOEXEC,
        });
        if r < 0 {
            dlog!("poll swap failed proc={procid}");
            return;
        }
        dlog!("poll swap proc={procid} fd={}", p.fd);
        let p = self.procs.get_mut(&procid).unwrap();
        p.poll_evfd = Some((efd, epfd));
        p.poll_level = false;
        self.dirty.insert(procid);
    }

    fn sync_poll(&mut self, proc: ProcId) {
        let want = self.broker.proc_pollable(proc);
        let Some(p) = self.procs.get_mut(&proc) else {
            return;
        };
        let Some((efd, _)) = p.poll_evfd.as_ref() else {
            return;
        };
        if want == p.poll_level {
            return;
        }
        let fd = efd.as_raw_fd();
        if want {
            let one: u64 = 1;
            unsafe { libc::write(fd, &one as *const u64 as *const _, 8) };
        } else {
            let mut buf = [0u8; 8];
            unsafe { libc::read(fd, buf.as_mut_ptr() as *mut _, 8) };
        }
        p.poll_level = want;
    }

    /// Re-run the reads of woken threads and refresh poll eventfds until
    /// nothing changes.
    fn drain(&mut self) {
        let mut guard = 0u32;
        loop {
            let mut work: Vec<Wakeup> = std::mem::take(&mut self.pending);
            work.extend(self.broker.take_wakeups());
            if work.is_empty() {
                break;
            }
            guard += 1;
            if guard > 1_000_000 {
                dlog!("drain: runaway wakeups");
                break;
            }
            for w in work {
                self.deliver(w);
            }
        }
        for p in self.broker.take_poll_dirty() {
            self.dirty.insert(p);
        }
        for p in std::mem::take(&mut self.dirty) {
            self.sync_poll(p);
        }
    }

    fn deliver(&mut self, w: Wakeup) {
        let Some(waiter) = self.waiters.remove(&(w.proc, w.tid)) else {
            return;
        };
        if !self.id_valid(waiter.notif_id) {
            // Interrupted by a signal: the ioctl restarts and reads its work
            // then (or the process is dying and the monitor releases it).
            return;
        }
        let read_avail = waiter
            .bwr
            .read_size
            .saturating_sub(waiter.bwr.read_consumed) as usize;
        let res = self.with_mem(Some((w.proc, waiter.notif_id)), |b, m| {
            b.write_read(w.proc, w.tid, &[], read_avail, false, m)
        });
        self.pending.extend(res.wakeups);
        if res.blocked {
            self.waiters.insert((w.proc, w.tid), waiter);
        } else {
            self.finish_read(
                (w.proc, w.tid),
                waiter.notif_id,
                waiter.bwr_addr,
                waiter.bwr,
                res.read,
                res.eagain,
            );
        }
    }

    fn release_proc(&mut self, procid: ProcId) {
        let wakeups = self.with_mem(None, |b, m| b.release_proc(procid, m));
        self.pending.extend(wakeups);
        if let Some(p) = self.procs.remove(&procid) {
            self.fd_index.remove(&(p.pid, p.fd));
        }
        // Threads still parked on a closed fd return EBADF (the kernel wakes
        // them on flush); a dead process's notifications are simply invalid.
        let parked: Vec<u64> = self
            .waiters
            .iter()
            .filter(|(k, _)| k.0 == procid)
            .map(|(_, w)| w.notif_id)
            .collect();
        self.waiters.retain(|k, _| k.0 != procid);
        self.stash.retain(|k, _| k.0 != procid);
        for id in parked {
            self.respond(id, 0, libc::EBADF, false);
        }
        dlog!("release proc={procid}");
    }

    fn release_pid(&mut self, pid: i32) {
        let ids: Vec<ProcId> = self
            .procs
            .iter()
            .filter(|(_, p)| p.pid == pid)
            .map(|(&k, _)| k)
            .collect();
        for id in ids {
            self.release_proc(id);
        }
        self.tgids.retain(|_, t| *t != pid);
        self.drain();
    }

    fn still_open(&self, p: &ProcState) -> bool {
        let ours = p
            .poll_evfd
            .as_ref()
            .map_or(&p.memfd, |(_, ep)| ep)
            .as_raw_fd();
        kcmp_same_file(p.pid, ours, p.fd)
    }

    fn sweep_pid(&mut self, pid: i32) {
        let gone: Vec<ProcId> = self
            .procs
            .iter()
            .filter(|(_, p)| p.pid == pid && !self.still_open(p))
            .map(|(&k, _)| k)
            .collect();
        for id in gone {
            self.release_proc(id);
        }
    }

    fn sweep_closed(&mut self) {
        let gone: Vec<ProcId> = self
            .procs
            .iter()
            .filter(|(_, p)| !self.still_open(p))
            .map(|(&k, _)| k)
            .collect();
        for id in gone {
            self.release_proc(id);
        }
        self.drain();
    }
}

#[cfg(test)]
pub(crate) mod tests {
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
    /// and vidsys's workers (paths read, lstat results written) use it.
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

    /// Two private pages filled with 0xAA, the second with `prot2`, the first
    /// with `prot1`.
    pub(crate) fn pages(prot1: i32, prot2: i32) -> *mut u8 {
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                8192,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        } as *mut u8;
        assert_ne!(p as *mut libc::c_void, libc::MAP_FAILED);
        unsafe {
            std::ptr::write_bytes(p, 0xAA, 8192);
            assert_eq!(libc::mprotect(p as *mut _, 4096, prot1), 0);
            assert_eq!(libc::mprotect(p.add(4096) as *mut _, 4096, prot2), 0);
        }
        p
    }

    /// The pinned fd is /proc/<pid>/mem, whose accesses ignore page
    /// protections (FOLL_FORCE). Transfers must still fail where the task
    /// couldn't read or write itself, as process_vm_* and the kernel's own
    /// copies do.
    #[test]
    fn pinned_mem_respects_page_protections() {
        let pin = open_mem(unsafe { libc::gettid() }).unwrap();
        let m = pin.raw();
        let rw = libc::PROT_READ | libc::PROT_WRITE;
        // Read-only page.
        let p = pages(rw, libc::PROT_READ);
        let ro = p as u64 + 4096;
        assert!(!mem_write(m, ro, b"x"));
        assert_eq!(unsafe { *p.add(4096) }, 0xAA);
        assert_eq!(mem_read(m, ro, 1), Some(vec![0xAA]));
        // A write that starts on a writable page and runs into it.
        assert!(!mem_write(m, ro - 4, &[0; 8]));
        assert_eq!(unsafe { *p.add(4092) }, 0xAA);
        // The writable page still works.
        assert!(mem_write(m, p as u64, &[1]));
        assert_eq!(unsafe { *p }, 1);
        // PROT_NONE: no reads, no writes.
        let p = pages(rw, libc::PROT_NONE);
        let none = p as u64 + 4096;
        assert!(mem_read(m, none, 1).is_none());
        assert!(mem_read(m, none - 4, 8).is_none());
        assert!(mem_read_path(m, none).is_none());
        assert!(!mem_write(m, none, b"x"));
        unsafe { libc::mprotect(p.add(4096) as *mut _, 4096, libc::PROT_READ) };
        assert_eq!(unsafe { *p.add(4096) }, 0xAA);
        // Unmapped.
        assert!(mem_read(m, 0x1000, 1).is_none());
        // The pre-6.11 fallback (maps text) agrees.
        let q = pages(rw, libc::PROT_READ) as u64;
        for (a, len, need) in [
            (q, 8, VMA_WRITABLE),
            (q + 4092, 8, VMA_WRITABLE),
            (q + 4092, 8, VMA_READABLE),
            (none, 1, VMA_READABLE),
            (0x1000, 1, VMA_READABLE),
        ] {
            assert_eq!(
                accessible_text(m.maps, a, a + len, need),
                accessible(m.maps, a, len as usize, need),
                "{a:#x}+{len} need {need}"
            );
        }
        assert!(accessible_text(m.maps, q, q + 8, VMA_WRITABLE));
        assert!(!accessible_text(m.maps, q + 4092, q + 4100, VMA_WRITABLE));
    }

    /// A supervisor with one binder process: this test process, with its
    /// memory pinned like a real opener's. Responses are only recorded (the
    /// listener is invalid).
    fn test_supervisor() -> (Supervisor, ProcId) {
        let (tx, _rx) = mpsc::channel();
        let mut sup = Supervisor {
            vid: None,
            broker: Broker::new(),
            listener: -1,
            resp_sz: size_of::<SeccompNotifResp>(),
            addfd_sz: size_of::<SeccompNotifAddfd>(),
            procs: HashMap::new(),
            fd_index: HashMap::new(),
            waiters: HashMap::new(),
            captured: HashMap::new(),
            next_token: 0,
            tgids: HashMap::new(),
            next_memfd: 1,
            watch: tx,
            pending: Vec::new(),
            dirty: HashSet::new(),
            stash: HashMap::new(),
            sent: Default::default(),
        };
        let pid = unsafe { libc::getpid() };
        let memfd = recv_memfd("lxbinder-test").unwrap();
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
        assert_ne!(map, libc::MAP_FAILED);
        let procid = sup.broker.open_ns(pid, pid, 0, 0);
        sup.procs.insert(
            procid,
            ProcState {
                pid,
                fd: 0,
                memfd,
                memfd_ino: 0,
                mem: open_mem(unsafe { libc::gettid() }).unwrap(),
                map: map as *mut u8,
                base: None,
                free: Vec::new(),
                live: HashMap::new(),
                nonblock: false,
                pidfd: None,
                poll_evfd: None,
                poll_level: false,
            },
        );
        (sup, procid)
    }

    /// BINDER_WRITE_READ whose bwr can be read but not written back fails
    /// with EFAULT, as binder_ioctl_write_read does, instead of succeeding
    /// with the caller's consumed counters left stale.
    #[test]
    fn write_read_fails_if_bwr_cannot_be_written_back() {
        let rw = libc::PROT_READ | libc::PROT_WRITE;
        // No work at all, and a write of one command (whose consumed count
        // the caller must see).
        for (write, prot, want) in [
            (false, libc::PROT_READ, -libc::EFAULT),
            (false, rw, 0),
            (true, libc::PROT_READ, -libc::EFAULT),
            (true, rw, 0),
        ] {
            let (mut sup, procid) = test_supervisor();
            let p = pages(rw, rw);
            let mut bwr = BinderWriteRead::default();
            if write {
                unsafe { std::ptr::write_unaligned(p as *mut u32, BC_ENTER_LOOPER) };
                bwr.write_size = 4;
                bwr.write_buffer = p as u64;
            }
            unsafe {
                std::ptr::write_unaligned(p.add(4096) as *mut BinderWriteRead, bwr);
                libc::mprotect(p.add(4096) as *mut _, 4096, prot);
            }
            let mut req = SeccompNotif {
                id: 7,
                pid: unsafe { libc::gettid() } as u32,
                ..Default::default()
            };
            req.data.args[2] = p as u64 + 4096;
            sup.handle_write_read(procid, req, p as u64 + 4096);
            assert_eq!(sup.sent.borrow().last(), Some(&(7, want)), "{write} {prot}");
            let back: BinderWriteRead =
                unsafe { std::ptr::read_unaligned(p.add(4096) as *const BinderWriteRead) };
            assert_eq!(back.write_consumed, if write && prot == rw { 4 } else { 0 });
        }
    }
}
