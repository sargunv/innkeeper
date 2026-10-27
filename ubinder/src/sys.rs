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
//!    process's binder fd swapped for an eventfd (`ADDFD`+`SETFD`; the mapping
//!    keeps the memfd alive) whose level the broker drives (`binder_poll`).
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
fn notif_addfd_cmd(sz: usize) -> u32 {
    ioc(1, 3, sz)
}

const SECCOMP_USER_NOTIF_FLAG_CONTINUE: u32 = 1;
const SECCOMP_ADDFD_FLAG_SETFD: u32 = 1;
const SECCOMP_ADDFD_FLAG_SEND: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SeccompData {
    nr: i32,
    arch: u32,
    instruction_pointer: u64,
    args: [u64; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct SeccompNotif {
    id: u64,
    pid: u32,
    flags: u32,
    data: SeccompData,
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
struct SeccompNotifAddfd {
    id: u64,
    flags: u32,
    srcfd: u32,
    newfd: u32,
    newfd_flags: u32,
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
    map: *mut u8,
    /// Target mapping (base, usable length), found in /proc/<pid>/maps.
    base: Option<(u64, usize)>,
    free: Vec<(usize, usize)>,
    live: HashMap<u64, (usize, usize)>,
    nonblock: bool,
    pidfd: Option<OwnedFd>,
    /// Present once a thread polls: the fd in the target is this eventfd.
    poll_evfd: Option<OwnedFd>,
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
}

unsafe impl Send for Supervisor {}

// ---- libc helpers ------------------------------------------------------

fn vm_read(pid: i32, addr: u64, len: usize) -> Option<Vec<u8>> {
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

fn vm_write(pid: i32, addr: u64, data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
    }
    if addr == 0 {
        return false;
    }
    let local = libc::iovec {
        iov_base: data.as_ptr() as *mut _,
        iov_len: data.len(),
    };
    let remote = libc::iovec {
        iov_base: addr as *mut _,
        iov_len: data.len(),
    };
    let n = unsafe { libc::process_vm_writev(pid, &local, 1, &remote, 1, 0) };
    n == data.len() as isize
}

/// Read a NUL-terminated path, never crossing into a page that need not be
/// mapped before it is known to be needed.
fn read_path(pid: i32, addr: u64) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut a = addr;
    while out.len() < 4096 {
        let to_page_end = 4096 - (a as usize & 4095);
        let chunk = to_page_end.min(64);
        let buf = vm_read(pid, a, chunk)?;
        if let Some(p) = buf.iter().position(|&b| b == 0) {
            out.extend_from_slice(&buf[..p]);
            return Some(out);
        }
        out.extend_from_slice(&buf);
        a += chunk as u64;
    }
    None
}

fn status_field(pid: i32, key: &str) -> Option<String> {
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
        let pid = self.procs.get(&proc)?.pid;
        vm_read(pid, addr, len)
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

/// Serve binder notifications forwarded by the bpf supervisor over `sock`
/// (one `seccomp_notif` per SOCK_SEQPACKET message), answering them via
/// `listener` (a dup of the single process listener). Called by the broker
/// helper (`src/bin/broker.rs`).
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
    let sup = Arc::new(Mutex::new(Supervisor {
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
    }));
    {
        let sup = Arc::clone(&sup);
        std::thread::spawn(move || monitor(sup, rx));
    }
    let mut notif = vec![0u8; notif_sz];
    loop {
        let n = unsafe { libc::recv(sock, notif.as_mut_ptr() as *mut _, notif_sz, 0) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            eprintln!("ubinder: recv: {e}");
            return;
        }
        if n == 0 {
            return;
        }
        let req: SeccompNotif = unsafe { std::ptr::read_unaligned(notif.as_ptr() as *const _) };
        sup.lock().unwrap().dispatch(req);
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
        unsafe { libc::ioctl(self.listener, notif_id_valid_cmd() as _, &id as *const u64) == 0 }
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
        // Drop any earlier binder fds of this process that are gone (exec).
        self.sweep_pid(pid);
        let flags = req.data.args[2] as u32;
        let name = format!("lxbinder-{}", self.next_memfd);
        self.next_memfd += 1;
        let cname = std::ffi::CString::new(name).unwrap();
        let memfd = unsafe { libc::memfd_create(cname.as_ptr(), libc::MFD_CLOEXEC) };
        if memfd < 0 {
            {
                self.respond(req.id, 0, libc::ENOMEM, false);
                return;
            }
        }
        let memfd = unsafe { OwnedFd::from_raw_fd(memfd) };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if unsafe { libc::ftruncate(memfd.as_raw_fd(), BINDER_MEMFD_SIZE as i64) } != 0
            || unsafe { libc::fstat(memfd.as_raw_fd(), &mut st) } != 0
        {
            {
                self.respond(req.id, 0, libc::ENOMEM, false);
                return;
            }
        }
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
        let euid = status_field(pid, "Uid:")
            .and_then(|u| u.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
            .unwrap_or(0);
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
        let err = match cmd {
            BINDER_WRITE_READ => return self.handle_write_read(procid, req, argp),
            BINDER_VERSION => {
                let v = BinderVersion {
                    protocol_version: self.broker.protocol_version(),
                };
                if vm_write(tid, argp, pod_bytes(&v)) {
                    0
                } else {
                    libc::EFAULT
                }
            }
            BINDER_SET_MAX_THREADS => match vm_read(tid, argp, 4) {
                Some(b) => {
                    self.broker
                        .set_max_threads(procid, u32::from_le_bytes(b.try_into().unwrap()));
                    0
                }
                None => libc::EFAULT,
            },
            BINDER_ENABLE_ONEWAY_SPAM_DETECTION => match vm_read(tid, argp, 4) {
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
                match vm_read(tid, argp, size_of::<FlatBinderObject>())
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
                if vm_write(tid, argp, pod_bytes(&ee)) {
                    0
                } else {
                    libc::EFAULT
                }
            }
            BINDER_GET_NODE_DEBUG_INFO => {
                match vm_read(tid, argp, size_of::<BinderNodeDebugInfo>())
                    .and_then(|b| read_pod::<BinderNodeDebugInfo>(&b, 0))
                {
                    Some(info) => {
                        let out = self.broker.node_debug_info(procid, info.ptr);
                        if vm_write(tid, argp, pod_bytes(&out)) {
                            0
                        } else {
                            libc::EFAULT
                        }
                    }
                    None => libc::EFAULT,
                }
            }
            BINDER_GET_NODE_INFO_FOR_REF => {
                match vm_read(tid, argp, size_of::<BinderNodeInfoForRef>())
                    .and_then(|b| read_pod::<BinderNodeInfoForRef>(&b, 0))
                {
                    Some(info) => match self.broker.node_info_for_ref(procid, info) {
                        Ok(out) => {
                            if vm_write(tid, argp, pod_bytes(&out)) {
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
        let tid = req.pid as i32;
        let Some(mut bwr) = vm_read(tid, argp, size_of::<BinderWriteRead>())
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
                .and_then(|a| vm_read(tid, a, len))
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
        if res.write_err != 0 {
            vm_write(tid, argp, pod_bytes(&bwr));
            self.respond(req.id, 0, res.write_err, false);
        } else if res.blocked {
            // Record progress now so a restarted ioctl never replays the write.
            vm_write(tid, argp, pod_bytes(&bwr));
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
        let tid = key.1 as i32;
        let at = bwr.read_buffer.wrapping_add(bwr.read_consumed);
        if !read.is_empty() && !vm_write(tid, at, &read) {
            vm_write(tid, argp, pod_bytes(&bwr));
            self.respond(notif_id, 0, libc::EFAULT, false);
            return;
        }
        let mut out = bwr;
        out.read_consumed += read.len() as u64;
        vm_write(tid, argp, pod_bytes(&out));
        if !self.respond(notif_id, 0, if eagain { libc::EAGAIN } else { 0 }, false)
            && !read.is_empty()
        {
            // The thread was interrupted after we committed work to it; it
            // will restart the ioctl. Restore its bwr and keep the bytes.
            vm_write(tid, argp, pod_bytes(&bwr));
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

    /// Swap the process's binder fd for an eventfd the broker drives.
    fn make_pollable(&mut self, procid: ProcId, notif_id: u64) {
        let Some(p) = self.procs.get(&procid) else {
            return;
        };
        if p.poll_evfd.is_some() {
            return;
        }
        let efd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
        if efd < 0 {
            return;
        }
        let efd = unsafe { OwnedFd::from_raw_fd(efd) };
        let r = self.addfd(&SeccompNotifAddfd {
            id: notif_id,
            flags: SECCOMP_ADDFD_FLAG_SETFD,
            srcfd: efd.as_raw_fd() as u32,
            newfd: p.fd,
            newfd_flags: O_CLOEXEC,
        });
        if r < 0 {
            dlog!("poll swap failed proc={procid}");
            return;
        }
        dlog!("poll swap proc={procid} fd={}", p.fd);
        let p = self.procs.get_mut(&procid).unwrap();
        p.poll_evfd = Some(efd);
        p.poll_level = false;
        self.dirty.insert(procid);
    }

    fn sync_poll(&mut self, proc: ProcId) {
        let want = self.broker.proc_pollable(proc);
        let Some(p) = self.procs.get_mut(&proc) else {
            return;
        };
        let Some(efd) = p.poll_evfd.as_ref() else {
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
        let ours = p.poll_evfd.as_ref().unwrap_or(&p.memfd).as_raw_fd();
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
