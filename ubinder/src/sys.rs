//! Seccomp user-notification binding: drives the [`crate::Broker`] against real
//! Android processes in a Linux.apk session.
//!
//! The session's `pivot-exec` installs a second seccomp filter on Android init
//! (inherited by every process) that traps `openat` and `ioctl`; it hands the
//! listener fd to this serve loop, which runs as a thread of the supervisor
//! (outside the Android namespaces, with the host user's power — treat every
//! byte from Android as hostile). The existing bpf supervisor (`bpfemu.c`) is a
//! separate, disjoint filter/listener and is left untouched, so kernel binder
//! stays the default.
//!
//!  * `openat` of a binder device path  -> inject a memfd (`ADDFD`); the process
//!    mmaps it as its receive region, and the broker writes transactions there.
//!  * `ioctl(fd, BINDER_*, argp)`        -> service via `process_vm_{readv,writev}`.
//!  * a blocking `BINDER_WRITE_READ` with no work is deferred: we keep the
//!    notification id and answer it when a later enqueue wakes the thread.
//!
//! A death-monitor thread polls a pidfd per process and calls
//! [`Broker::release_proc`] on exit, firing death notifications.
#![cfg(target_os = "linux")]
#![allow(clippy::missing_safety_doc)]

use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};

use crate::broker::{Broker, Mem, ProcId, Wakeup};
use crate::defs::*;

// ---- seccomp uapi (x86_64) --------------------------------------------

const SECCOMP_IOC_MAGIC: u8 = b'!';
const fn ioc(dir: u32, nr: u8, size: usize) -> u32 {
    (dir << 30) | ((SECCOMP_IOC_MAGIC as u32) << 8) | (nr as u32) | ((size as u32) << 16)
}
fn notif_recv_cmd(sz: usize) -> u32 {
    ioc(3, 0, sz)
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

// openat flag we care about (O_CLOEXEC) for the injected fd.
const O_CLOEXEC: u32 = 0o2000000;

// The memfd is a full 1 MiB, but libbinder maps only
// `BINDER_VM_SIZE = 1 MiB - 2 pages` of it (ProcessState.cpp), so the broker
// must hand out offsets within the smaller, actually-mapped window.
const BINDER_MMAP_SIZE: usize = 1024 * 1024;
const BINDER_VM_USABLE: usize = 1024 * 1024 - 2 * 4096;

// ---- per-process state -------------------------------------------------

struct ProcState {
    pid: i32,
    #[allow(dead_code)]
    fd_in_target: u32,
    // Held open as an RAII guard; the mapping and the target's ADDFD'd copy
    // both reference the same file.
    #[allow(dead_code)]
    memfd: OwnedFd,
    /// supervisor's shared mapping of the memfd (page cache; writes here are
    /// visible to the target's read-only MAP_PRIVATE mapping)
    map: *mut u8,
    map_len: usize,
    /// the target's mmap base for the memfd, discovered from /proc/<pid>/maps
    base: Option<u64>,
    /// simple offset allocator over the receive region
    free: Vec<(usize, usize)>, // (offset, len) free spans
    /// live buffer allocations: user_addr -> (offset, len)
    live: HashMap<u64, (usize, usize)>,
    name: String,
}

// Raw pointer in ProcState: the supervisor owns it exclusively under the lock.
unsafe impl Send for ProcState {}

struct Waiter {
    notif_id: u64,
    bwr_addr: u64,
    read_buf: u64,
    read_size: usize,
    write_consumed: u64,
}

/// A captured fd held by the supervisor until it is installed into a target.
struct Captured {
    fd: OwnedFd,
}

pub struct Supervisor {
    broker: Broker,
    listener: RawFd,
    resp_sz: usize,
    addfd_sz: usize,
    procs: HashMap<ProcId, ProcState>,
    /// (pid, fd) -> ProcId, to resolve ioctls to a broker process
    fd_index: HashMap<(i32, u32), ProcId>,
    waiters: HashMap<(ProcId, u32), Waiter>,
    captured: HashMap<u64, Captured>,
    next_token: u64,
    /// pids we have opened a pidfd for (death monitoring)
    pidfds: HashMap<i32, OwnedFd>,
}

unsafe impl Send for Supervisor {}

// ---- libc helpers ------------------------------------------------------

unsafe fn vm_read(pid: i32, addr: u64, len: usize) -> Option<Vec<u8>> {
    if len == 0 {
        return Some(Vec::new());
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

unsafe fn vm_write(pid: i32, addr: u64, data: &[u8]) -> bool {
    if data.is_empty() {
        return true;
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

fn read_cstr(pid: i32, addr: u64, max: usize) -> Option<String> {
    // Read in page-bounded chunks until a NUL.
    let mut out = Vec::new();
    let mut a = addr;
    while out.len() < max {
        let chunk = 256.min(max - out.len());
        let buf = unsafe { vm_read(pid, a, chunk) }?;
        if let Some(p) = buf.iter().position(|&b| b == 0) {
            out.extend_from_slice(&buf[..p]);
            return String::from_utf8(out).ok();
        }
        out.extend_from_slice(&buf);
        a += chunk as u64;
    }
    None
}

// ---- Mem over real processes ------------------------------------------

/// A borrow of the supervisor's process table + fd machinery used while
/// servicing one notification. Split out so the broker can call back into it
/// without re-locking.
struct SysMem<'a> {
    procs: &'a mut HashMap<ProcId, ProcState>,
    pid_of: &'a HashMap<ProcId, i32>,
    listener: RawFd,
    addfd_sz: usize,
    captured: &'a mut HashMap<u64, Captured>,
    next_token: &'a mut u64,
    /// id of the notification currently being serviced on the *target* — only
    /// set when the broker is emitting into a process that has an outstanding
    /// read notification (needed for ADDFD during delivery)
    deliver_notif: &'a HashMap<ProcId, u64>,
}

impl ProcState {
    fn ensure_base(&mut self) -> Option<u64> {
        if self.base.is_some() {
            return self.base;
        }
        let maps = std::fs::read_to_string(format!("/proc/{}/maps", self.pid)).ok()?;
        for line in maps.lines() {
            if line.contains(&self.name) {
                let start = line.split('-').next()?;
                self.base = u64::from_str_radix(start, 16).ok();
                break;
            }
        }
        self.base
    }
    fn alloc(&mut self, size: usize) -> Option<usize> {
        let size = (size + 7) & !7;
        let mut best: Option<usize> = None;
        for (i, &(_off, len)) in self.free.iter().enumerate() {
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
        self.free.push((off, size));
        // coalesce
        self.free.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(self.free.len());
        for &(o, l) in &self.free {
            if let Some(last) = merged.last_mut()
                && last.0 + last.1 == o
            {
                last.1 += l;
                continue;
            }
            merged.push((o, l));
        }
        self.free = merged;
    }
}

impl Mem for SysMem<'_> {
    fn read(&mut self, proc: ProcId, addr: u64, len: usize) -> Option<Vec<u8>> {
        let pid = *self.pid_of.get(&proc)?;
        unsafe { vm_read(pid, addr, len) }
    }
    fn alloc_recv(&mut self, proc: ProcId, size: usize, _is_async: bool) -> Option<u64> {
        let p = self.procs.get_mut(&proc)?;
        let base = p.ensure_base()?;
        let off = p.alloc(size)?;
        let addr = base + off as u64;
        p.live.insert(addr, (off, size));
        Some(addr)
    }
    fn write_recv(&mut self, proc: ProcId, addr: u64, data: &[u8]) {
        // Write through the supervisor's shared mapping of the memfd.
        if let Some(p) = self.procs.get_mut(&proc)
            && let Some(base) = p.base
        {
            let off = (addr - base) as usize;
            if off + data.len() <= p.map_len {
                unsafe {
                    std::ptr::copy_nonoverlapping(data.as_ptr(), p.map.add(off), data.len());
                }
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
        let pid = *self.pid_of.get(&from)?;
        let pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if pidfd < 0 {
            return None;
        }
        let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd as RawFd) };
        let got =
            unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), from_fd as i32, 0) };
        if got < 0 {
            return None;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(got as RawFd) };
        *self.next_token += 1;
        let tok = *self.next_token;
        self.captured.insert(tok, Captured { fd });
        Some(tok)
    }
    fn install_fd(&mut self, to: ProcId, token: u64) -> Option<u32> {
        let notif_id = *self.deliver_notif.get(&to)?;
        let cap = self.captured.get(&token)?;
        let addfd = SeccompNotifAddfd {
            id: notif_id,
            flags: 0,
            srcfd: cap.fd.as_raw_fd() as u32,
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

impl Supervisor {
    fn pid_map(&self) -> HashMap<ProcId, i32> {
        self.procs.iter().map(|(&k, v)| (k, v.pid)).collect()
    }
}

// ---- the serve loop ----------------------------------------------------

/// Serve binder notifications on `listener` until it breaks. Spawns a
/// death-monitor thread. Intended to be called from a dedicated supervisor
/// thread (see the C ABI `ubinder_serve`).
pub fn serve(listener: RawFd) {
    let mut sizes = SeccompNotifSizes::default();
    let r = unsafe { libc::syscall(libc::SYS_seccomp, SECCOMP_GET_NOTIF_SIZES, 0, &mut sizes) };
    if r != 0 {
        eprintln!("ubinder: SECCOMP_GET_NOTIF_SIZES failed");
        return;
    }
    let notif_sz = (sizes.seccomp_notif as usize).max(size_of::<SeccompNotif>());
    let resp_sz = (sizes.seccomp_notif_resp as usize).max(size_of::<SeccompNotifResp>());
    let addfd_sz = size_of::<SeccompNotifAddfd>();

    let sup = Arc::new(Mutex::new(Supervisor {
        broker: Broker::new(),
        listener,
        resp_sz,
        addfd_sz,
        procs: HashMap::new(),
        fd_index: HashMap::new(),
        waiters: HashMap::new(),
        captured: HashMap::new(),
        next_token: 0,
        pidfds: HashMap::new(),
    }));

    // Death monitor.
    {
        let sup = Arc::clone(&sup);
        std::thread::spawn(move || death_monitor(sup));
    }

    let mut notif = vec![0u8; notif_sz];
    loop {
        notif.iter_mut().for_each(|b| *b = 0);
        let r = unsafe { libc::ioctl(listener, notif_recv_cmd(notif_sz) as _, notif.as_mut_ptr()) };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) | Some(libc::ENOENT) => continue,
                _ => {
                    eprintln!("ubinder: NOTIF_RECV: {e}");
                    return;
                }
            }
        }
        let req: SeccompNotif = unsafe { std::ptr::read_unaligned(notif.as_ptr() as *const _) };
        let mut s = sup.lock().unwrap();
        s.dispatch(req);
    }
}

fn death_monitor(sup: Arc<Mutex<Supervisor>>) {
    loop {
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut dead: Vec<(i32, Vec<ProcId>)> = Vec::new();
        {
            let s = sup.lock().unwrap();
            for &pid in s.pidfds.keys() {
                if !std::path::Path::new(&format!("/proc/{pid}")).exists() {
                    let procs: Vec<ProcId> = s
                        .procs
                        .iter()
                        .filter(|(_, v)| v.pid == pid)
                        .map(|(&k, _)| k)
                        .collect();
                    dead.push((pid, procs));
                }
            }
        }
        if dead.is_empty() {
            continue;
        }
        let mut s = sup.lock().unwrap();
        for (pid, procs) in dead {
            s.pidfds.remove(&pid);
            for p in procs {
                let wakeups = s.broker.release_proc(p);
                s.deliver_wakeups(wakeups);
                s.procs.remove(&p);
                s.fd_index.retain(|_, v| *v != p);
                s.waiters.remove(&(p, 0));
            }
        }
    }
}

impl Supervisor {
    fn respond(&self, id: u64, val: i64, error: i32, cont: bool) {
        let mut resp = SeccompNotifResp {
            id,
            val,
            error,
            flags: 0,
        };
        if cont {
            resp.flags = SECCOMP_USER_NOTIF_FLAG_CONTINUE;
        }
        let mut buf = vec![0u8; self.resp_sz];
        unsafe {
            std::ptr::write_unaligned(buf.as_mut_ptr() as *mut SeccompNotifResp, resp);
            libc::ioctl(
                self.listener,
                notif_send_cmd(self.resp_sz) as _,
                buf.as_mut_ptr(),
            );
        }
    }

    fn id_valid(&self, id: u64) -> bool {
        unsafe { libc::ioctl(self.listener, notif_id_valid_cmd() as _, &id as *const u64) == 0 }
    }

    fn dispatch(&mut self, req: SeccompNotif) {
        let nr = req.data.nr as i64;
        if nr == libc::SYS_openat {
            self.handle_openat(req);
        } else if nr == libc::SYS_ioctl {
            self.handle_ioctl(req);
        } else {
            self.respond(req.id, 0, 0, true); // continue; not ours
        }
    }

    fn handle_openat(&mut self, req: SeccompNotif) {
        // seccomp reports the task (thread) id; the binder fd lands in the
        // shared (tgid) fd table.
        let tid = req.pid as i32;
        let pid = tgid_of(tid).unwrap_or(tid);
        let path = read_cstr(pid, req.data.args[1], 256);
        let ctx = match path.as_deref() {
            Some("/dev/binder") => Some(0u8),
            Some("/dev/hwbinder") => Some(1),
            Some("/dev/vndbinder") => Some(2),
            Some(p) if p.starts_with("/dev/binderfs/") => Some(match p.rsplit('/').next() {
                Some("hwbinder") => 1,
                Some("vndbinder") => 2,
                _ => 0,
            }),
            _ => None,
        };
        let Some(ctx) = ctx else {
            self.respond(req.id, 0, 0, true); // not a binder device; continue
            return;
        };
        if !self.id_valid(req.id) {
            return;
        }
        let flags = req.data.args[2] as u32;
        // Create the per-process receive memfd.
        let procid_preview = 0; // real id assigned after we know it; name uses a counter
        let _ = procid_preview;
        let name = format!("lxbinder-{}-{}", pid, req.data.args[1]);
        let cname = std::ffi::CString::new(name.clone()).unwrap();
        let memfd = unsafe { libc::memfd_create(cname.as_ptr(), 1) }; // MFD_CLOEXEC
        if memfd < 0 {
            self.respond(req.id, 0, libc::ENOMEM, false);
            return;
        }
        let memfd = unsafe { OwnedFd::from_raw_fd(memfd) };
        unsafe { libc::ftruncate(memfd.as_raw_fd(), BINDER_MMAP_SIZE as i64) };
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                BINDER_MMAP_SIZE,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                memfd.as_raw_fd(),
                0,
            )
        };
        if map == libc::MAP_FAILED {
            self.respond(req.id, 0, libc::ENOMEM, false);
            return;
        }
        // Inject the memfd into the target; ADDFD with SEND makes openat return
        // the new fd number.
        let addfd = SeccompNotifAddfd {
            id: req.id,
            flags: SECCOMP_ADDFD_FLAG_SEND,
            srcfd: memfd.as_raw_fd() as u32,
            newfd: 0,
            newfd_flags: if flags & O_CLOEXEC != 0 { O_CLOEXEC } else { 0 },
        };
        let newfd = unsafe {
            libc::ioctl(
                self.listener,
                notif_addfd_cmd(self.addfd_sz) as _,
                &addfd as *const _,
            )
        };
        if newfd < 0 {
            unsafe { libc::munmap(map, BINDER_MMAP_SIZE) };
            return; // target likely gone; addfd+SEND already answered or failed
        }
        let euid = pid_euid(pid).unwrap_or(0);
        let procid = self.broker.open(pid, euid, ctx);
        self.procs.insert(
            procid,
            ProcState {
                pid,
                fd_in_target: newfd as u32,
                memfd,
                map: map as *mut u8,
                map_len: BINDER_MMAP_SIZE,
                base: None,
                free: vec![(0, BINDER_VM_USABLE)],
                live: HashMap::new(),
                name,
            },
        );
        self.fd_index.insert((pid, newfd as u32), procid);
        self.ensure_pidfd(pid);
        // ADDFD+SEND already delivered the syscall return; nothing more to send.
    }

    fn ensure_pidfd(&mut self, pid: i32) {
        if self.pidfds.contains_key(&pid) {
            return;
        }
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
        if fd >= 0 {
            self.pidfds
                .insert(pid, unsafe { OwnedFd::from_raw_fd(fd as RawFd) });
        }
    }

    fn handle_ioctl(&mut self, req: SeccompNotif) {
        let fd = req.data.args[0] as u32;
        let cmd = req.data.args[1] as u32;
        let argp = req.data.args[2];
        if !is_binder_ioctl(cmd) {
            self.respond(req.id, 0, 0, true);
            return;
        }
        let tid = req.pid as i32;
        let pid = tgid_of(tid).unwrap_or(tid);
        let Some(&procid) = self.fd_index.get(&(pid, fd)) else {
            // Not a binder fd we manage; let it through.
            self.respond(req.id, 0, 0, true);
            return;
        };
        if !self.id_valid(req.id) {
            return;
        }
        match cmd {
            BINDER_VERSION => {
                let v = BinderVersion {
                    protocol_version: self.broker.protocol_version(),
                };
                let ok = unsafe { vm_write(pid, argp, pod_bytes(&v)) };
                self.respond(req.id, 0, if ok { 0 } else { libc::EFAULT }, false);
            }
            BINDER_SET_MAX_THREADS => {
                if let Some(b) = unsafe { vm_read(pid, argp, 4) } {
                    let n = u32::from_le_bytes(b.try_into().unwrap());
                    self.broker.set_max_threads(procid, n);
                }
                self.respond(req.id, 0, 0, false);
            }
            BINDER_ENABLE_ONEWAY_SPAM_DETECTION => {
                if let Some(b) = unsafe { vm_read(pid, argp, 4) } {
                    let n = u32::from_le_bytes(b.try_into().unwrap());
                    self.broker.enable_oneway_spam_detection(procid, n != 0);
                }
                self.respond(req.id, 0, 0, false);
            }
            BINDER_SET_CONTEXT_MGR => {
                let r = self.broker.set_context_mgr(procid, None);
                self.respond(req.id, 0, errno_of(r), false);
            }
            BINDER_SET_CONTEXT_MGR_EXT => {
                let fbo = unsafe { vm_read(pid, argp, size_of::<FlatBinderObject>()) }
                    .and_then(|b| read_pod::<FlatBinderObject>(&b, 0));
                let r = self.broker.set_context_mgr(procid, fbo);
                self.respond(req.id, 0, errno_of(r), false);
            }
            BINDER_THREAD_EXIT => {
                self.broker.thread_exit(procid, req.pid);
                self.respond(req.id, 0, 0, false);
            }
            BINDER_GET_EXTENDED_ERROR => {
                let ee = self.broker.get_extended_error(procid, req.pid);
                let ok = unsafe { vm_write(pid, argp, pod_bytes(&ee)) };
                self.respond(req.id, 0, if ok { 0 } else { libc::EFAULT }, false);
            }
            BINDER_GET_NODE_DEBUG_INFO => {
                if let Some(b) = unsafe { vm_read(pid, argp, size_of::<BinderNodeDebugInfo>()) } {
                    let info: BinderNodeDebugInfo = read_pod(&b, 0).unwrap_or_default();
                    let out = self.broker.node_debug_info(procid, info.ptr);
                    let ok = unsafe { vm_write(pid, argp, pod_bytes(&out)) };
                    self.respond(req.id, 0, if ok { 0 } else { libc::EFAULT }, false);
                } else {
                    self.respond(req.id, 0, libc::EFAULT, false);
                }
            }
            BINDER_WRITE_READ => {
                self.handle_write_read(procid, req, argp);
            }
            _ => {
                // Freeze ioctls and anything else: accept as a no-op success so
                // libbinder feature probes don't fail hard.
                self.respond(req.id, 0, 0, false);
            }
        }
    }

    fn handle_write_read(&mut self, procid: ProcId, req: SeccompNotif, argp: u64) {
        let pid = req.pid as i32;
        let tid = req.pid; // thread id == notif pid? see note below
        let Some(bwr_bytes) = (unsafe { vm_read(pid, argp, size_of::<BinderWriteRead>()) }) else {
            self.respond(req.id, 0, libc::EFAULT, false);
            return;
        };
        let bwr: BinderWriteRead = read_pod(&bwr_bytes, 0).unwrap();
        let write = if bwr.write_size > 0 {
            unsafe { vm_read(pid, bwr.write_buffer, bwr.write_size as usize) }.unwrap_or_default()
        } else {
            Vec::new()
        };

        let pid_of = self.pid_map();
        let mut deliver = HashMap::new();
        deliver.insert(procid, req.id); // this thread's own read may install fds
        let listener = self.listener;
        let addfd_sz = self.addfd_sz;

        let res = {
            let mut mem = SysMem {
                procs: &mut self.procs,
                pid_of: &pid_of,
                listener,
                addfd_sz,
                captured: &mut self.captured,
                next_token: &mut self.next_token,
                deliver_notif: &deliver,
            };
            self.broker
                .write_read(procid, tid, &write, bwr.read_size as usize, false, &mut mem)
        };

        let wakeups = res.wakeups;
        if res.blocked {
            // Defer: remember how to answer this notification when work arrives.
            self.waiters.insert(
                (procid, tid),
                Waiter {
                    notif_id: req.id,
                    bwr_addr: argp,
                    read_buf: bwr.read_buffer,
                    read_size: bwr.read_size as usize,
                    write_consumed: bwr.write_size,
                },
            );
        } else {
            self.finish_read(pid, req.id, argp, bwr, &res.read);
        }
        self.deliver_wakeups(wakeups);
    }

    // Write back the final bwr + read bytes and answer the notification.
    fn finish_read(
        &mut self,
        pid: i32,
        notif_id: u64,
        argp: u64,
        mut bwr: BinderWriteRead,
        read: &[u8],
    ) {
        if !read.is_empty() {
            unsafe { vm_write(pid, bwr.read_buffer, read) };
        }
        bwr.write_consumed = bwr.write_size;
        bwr.read_consumed = read.len() as u64;
        unsafe { vm_write(pid, argp, pod_bytes(&bwr)) };
        self.respond(notif_id, 0, 0, false);
    }

    /// For each woken thread with a parked read, re-run the read and answer.
    fn deliver_wakeups(&mut self, mut wakeups: Vec<Wakeup>) {
        let mut guard = 0;
        while let Some(w) = wakeups.pop() {
            guard += 1;
            if guard > 100_000 {
                break;
            }
            let Some(waiter) = self.waiters.remove(&(w.proc, w.tid)) else {
                continue;
            };
            if !self.id_valid(waiter.notif_id) {
                // target gone; drop
                continue;
            }
            let pid = match self.procs.get(&w.proc) {
                Some(p) => p.pid,
                None => continue,
            };
            let pid_of = self.pid_map();
            let mut deliver = HashMap::new();
            deliver.insert(w.proc, waiter.notif_id);
            let listener = self.listener;
            let addfd_sz = self.addfd_sz;
            let res = {
                let mut mem = SysMem {
                    procs: &mut self.procs,
                    pid_of: &pid_of,
                    listener,
                    addfd_sz,
                    captured: &mut self.captured,
                    next_token: &mut self.next_token,
                    deliver_notif: &deliver,
                };
                self.broker
                    .write_read(w.proc, w.tid, &[], waiter.read_size, false, &mut mem)
            };
            if res.blocked {
                // Still nothing (spurious); re-park.
                self.waiters.insert((w.proc, w.tid), waiter);
            } else {
                let bwr = BinderWriteRead {
                    write_size: waiter.write_consumed,
                    write_consumed: waiter.write_consumed,
                    write_buffer: 0,
                    read_size: waiter.read_size as u64,
                    read_consumed: 0,
                    read_buffer: waiter.read_buf,
                };
                self.finish_read(pid, waiter.notif_id, waiter.bwr_addr, bwr, &res.read);
            }
            for nw in res.wakeups {
                wakeups.push(nw);
            }
        }
    }
}

fn errno_of(r: Result<(), i32>) -> i32 {
    match r {
        Ok(()) => 0,
        Err(e) => -e, // broker returns negative errno; syscall error is positive
    }
}

fn tgid_of(tid: i32) -> Option<i32> {
    let status = std::fs::read_to_string(format!("/proc/{tid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Tgid:") {
            return rest.trim().parse().ok();
        }
    }
    None
}

fn pid_euid(pid: i32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            // Uid:\treal\teff\tsaved\tfs
            let mut it = rest.split_whitespace();
            let _real = it.next()?;
            return it.next()?.parse().ok();
        }
    }
    None
}

// ---- C ABI for the session supervisor ---------------------------------

/// Entry point the session's `pivot-exec` calls on a dedicated thread, passing
/// the binder seccomp listener fd. Never returns until the listener breaks.
///
/// # Safety
/// `listener` must be a valid seccomp notification fd owned by the caller.
#[unsafe(no_mangle)]
pub extern "C" fn ubinder_serve(listener: RawFd) {
    serve(listener);
}
