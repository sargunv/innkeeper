//! The credential syscalls that the session's seccomp filter traps
//! (supervisor/README.md), answered with the model in [`crate::creds::table`].
//!
//!  * `get*id`/`getgroups` return the caller's virtual credentials.
//!  * `set*id`/`setgroups` apply the kernel's rules to the virtual
//!    credentials. The new credentials are interned and stored in the
//!    process's soft `RLIMIT_RTTIME` (see `table.rs`). Virtual root is
//!    privileged (see `caps`).
//!  * The `chown` family is emulated, because unmapped ids would fail with
//!    EINVAL. The file is resolved in the caller's root, a no-op
//!    `fchownat(-1, -1)` keeps the kernel's checks (EROFS, ENOENT and so on),
//!    and the virtual owner goes into the `user.innkeeper.owner` xattr. This
//!    runs on worker threads, because a path on a FUSE mount could otherwise
//!    block the broker on a FUSE daemon that is waiting for binder.
//!  * `getsockopt(SO_PEERCRED)` returns the peer's virtual euid/egid and its
//!    pid in Android's pid namespace.
//!  * `lstat()`, meaning `newfstatat(AT_FDCWD, path, st, AT_SYMLINK_NOFOLLOW)`,
//!    is emulated on a worker thread with the recorded owner for paths that
//!    can carry one (`/data`, the eBPF pins, clatd). Code that verifies owners
//!    uses lstat (libcutils' fs_prepare_dir, ClatCoordinator), and the
//!    LD_PRELOAD shim's stat wrappers don't reach code in isolated linker
//!    namespaces such as the Tethering APEX. Every other lstat continues
//!    natively. Other stat calls aren't trapped.

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::{Arc, Mutex};

use crate::creds::table::{self, CredTable, Creds, SetId};
use crate::fs::Fs;
use crate::notify::{
    Listener, MemFd, PinnedMem, Pool, SeccompNotif, mem_read, mem_read_path, mem_write, open_mem,
    read_path, status_field, vm_read,
};

const RLIMIT_RTTIME: i32 = 15;
const SYS_FACCESSAT2: i64 = 439;
const W_OK: u64 = 2;
const AT_EACCESS: u64 = 0x200;
const SYS_OPENAT2: i64 = 437;
const RESOLVE_NO_MAGICLINKS: u64 = 0x02;
const RESOLVE_IN_ROOT: u64 = 0x10;
const RESOLVE_NO_SYMLINKS: u64 = 0x04;
const PIDFD_THREAD: i32 = 0o200; // O_EXCL
const AT_FDCWD: i32 = -100;
const AT_SYMLINK_NOFOLLOW: u64 = 0x100;
const AT_EMPTY_PATH: u64 = 0x1000;
/// AID_NOBODY, the identity reported for a peer that is gone.
const AID_NOBODY: u32 = 9999;

/// What to answer a trapped syscall with.
pub(crate) enum Answer {
    Val(i64),
    Errno(i32),
    /// Answered later by a worker thread.
    Deferred,
    /// Let the kernel run it.
    Continue,
}

/// Whether the absolute `path` can carry a recorded owner. It is false for
/// FUSE mounts (/storage, /mnt, /sdcard), so emulated lookups can't block on
/// MediaProvider.
fn owner_tracked(path: &[u8]) -> bool {
    const PREFIXES: [&[u8]; 4] = [
        b"/data",
        b"/sys/fs/bpf",
        b"/metadata",
        b"/apex/com.android.tethering/bin",
    ];
    PREFIXES
        .iter()
        .any(|p| path.starts_with(p) && (path.len() == p.len() || path[p.len()] == b'/'))
}

/// Whether this notification is a credential syscall.
pub(crate) fn is_cred_syscall(nr: i64, args: &[u64; 6]) -> bool {
    const NRS: [i64; 20] = [
        libc::SYS_getuid,
        libc::SYS_geteuid,
        libc::SYS_getgid,
        libc::SYS_getegid,
        libc::SYS_getresuid,
        libc::SYS_getresgid,
        libc::SYS_getgroups,
        libc::SYS_setuid,
        libc::SYS_setgid,
        libc::SYS_setreuid,
        libc::SYS_setregid,
        libc::SYS_setresuid,
        libc::SYS_setresgid,
        libc::SYS_setfsuid,
        libc::SYS_setfsgid,
        libc::SYS_setgroups,
        libc::SYS_chown,
        libc::SYS_fchown,
        libc::SYS_lchown,
        libc::SYS_fchownat,
    ];
    NRS.contains(&nr)
        || (nr == libc::SYS_newfstatat
            && args[0] as i32 == AT_FDCWD
            && args[3] == AT_SYMLINK_NOFOLLOW)
        || ((nr == libc::SYS_faccessat || nr == SYS_FACCESSAT2) && args[2] & W_OK != 0)
        || (nr == libc::SYS_getsockopt
            && args[1] as i32 == libc::SOL_SOCKET
            && args[2] as i32 == libc::SO_PEERCRED)
}

/// A chown, lstat or access call to run on a worker thread.
struct Job {
    id: u64,
    tid: i32,
    nr: i64,
    args: [u64; 6],
    /// The path, already read, for lstat and access.
    path: Vec<u8>,
    /// The caller's ids, for access.
    creds: Option<Creds>,
}

pub(crate) struct Vid {
    table: Mutex<CredTable>,
    listener: Listener,
    pool: Arc<Pool>,
    fs: Arc<Fs>,
    /// Init's pid, which SO_PEERCRED reports as 1. Init isn't pid 1, but its
    /// property service treats any peer that isn't pid 1 as a client. It would
    /// then treat init's own setprop from its restorecon thread as a new
    /// restorecon request, forever.
    init: i32,
}

impl Vid {
    pub(crate) fn new(listener: Listener, pool: Arc<Pool>, fs: Arc<Fs>, init: i32) -> Arc<Vid> {
        Arc::new(Vid {
            table: Mutex::new(CredTable::new()),
            listener,
            pool,
            fs,
            init,
        })
    }

    /// Runs a chown, lstat or access call on a worker thread.
    fn run(&self, job: Job) {
        // A queued job's task may be gone after a signal, exit or exec, and
        // its tid reused. Pin the address space tid has now, then drop the job
        // unless tid is still the caller. The job's memory access goes through
        // that pinned fd, so nothing reaches a later image.
        let mem = open_mem(job.tid);
        if !self.listener.valid(job.id) {
            return;
        }
        let Some(mem) = mem else {
            return self.finish(job.id, Answer::Errno(libc::ESRCH));
        };
        let mem = mem.raw();
        let r = match job.nr {
            libc::SYS_newfstatat => lstat(&job, self, mem),
            libc::SYS_faccessat | SYS_FACCESSAT2 => access(&job, &self.fs),
            _ => chown(&job, self, mem),
        };
        self.finish(job.id, r);
    }

    /// Answers one notification on the receive thread.
    pub(crate) fn serve(self: &Arc<Self>, req: &SeccompNotif) {
        let a = self.handle(req);
        self.finish(req.id, a);
    }

    fn finish(&self, id: u64, a: Answer) {
        if matches!(a, Answer::Continue) {
            self.listener.cont(id);
        } else {
            self.send(id, a);
        }
    }

    /// The virtual credentials of process or thread `pid`, in the
    /// supervisor's pid namespace.
    pub(crate) fn creds(&self, pid: i32) -> Option<Creds> {
        let soft = get_rttime(pid)?.0;
        Some(self.table.lock().unwrap().lookup(soft).clone())
    }

    fn set_creds(&self, pid: i32, c: &Creds) -> Result<(), i32> {
        let id = self.table.lock().unwrap().intern(c);
        let (_, hard) = get_rttime(pid).ok_or(libc::ESRCH)?;
        let soft = CredTable::encode(id);
        if soft > hard {
            return Err(libc::EAGAIN);
        }
        set_rttime(pid, soft, hard)
    }

    pub(crate) fn send(&self, id: u64, a: Answer) {
        let (val, error) = match a {
            Answer::Val(v) => (v, 0),
            Answer::Errno(e) => (0, e),
            Answer::Deferred | Answer::Continue => return,
        };
        self.listener.reply(id, val, error);
    }

    /// Answers one notification (see the module docs).
    fn handle(self: &Arc<Self>, req: &SeccompNotif) -> Answer {
        let tid = req.pid as i32;
        let nr = req.data.nr as i64;
        let a = req.data.args;
        let mut path = Vec::new();
        let mut job_creds = None;
        if nr == libc::SYS_faccessat || nr == SYS_FACCESSAT2 {
            // The kernel does root's access checks. Other callers are checked
            // against recorded owners (see access()).
            let Some(c) = self.creds(tid) else {
                return Answer::Continue;
            };
            let eff = nr == SYS_FACCESSAT2 && a[3] & AT_EACCESS != 0;
            if (if eff { c.euid } else { c.ruid }) == 0 {
                return Answer::Continue;
            }
            match read_path(tid, a[1]) {
                Some(p) if owner_tracked(&p) => path = p,
                _ => return Answer::Continue,
            }
            job_creds = Some(c);
        } else if nr == libc::SYS_newfstatat {
            match read_path(tid, a[1]) {
                Some(p) if owner_tracked(&p) => path = p,
                _ => return Answer::Continue,
            }
        }
        if job_creds.is_some()
            || !path.is_empty()
            || matches!(
                nr,
                libc::SYS_chown | libc::SYS_fchown | libc::SYS_lchown | libc::SYS_fchownat
            )
        {
            let job = Job {
                id: req.id,
                tid,
                nr,
                args: a,
                path,
                creds: job_creds,
            };
            let v = Arc::clone(self);
            self.pool.run(move || v.run(job));
            return Answer::Deferred;
        }
        if nr == libc::SYS_getsockopt {
            return self.peercred(req.id, tid, a);
        }
        let Some(c) = self.creds(tid) else {
            return Answer::Errno(libc::ESRCH);
        };
        let u = |i: usize| a[i] as u32;
        let op = match nr {
            libc::SYS_getuid => return Answer::Val(c.ruid as i64),
            libc::SYS_geteuid => return Answer::Val(c.euid as i64),
            libc::SYS_getgid => return Answer::Val(c.rgid as i64),
            libc::SYS_getegid => return Answer::Val(c.egid as i64),
            libc::SYS_getresuid | libc::SYS_getresgid => {
                let mem = match self.pin(req.id, tid) {
                    Ok(m) => m,
                    Err(e) => return e,
                };
                let ids = if nr == libc::SYS_getresuid {
                    [c.ruid, c.euid, c.suid]
                } else {
                    [c.rgid, c.egid, c.sgid]
                };
                return write_ids(mem.raw(), &a, ids);
            }
            libc::SYS_getgroups => {
                let size = a[0] as i32;
                if size < 0 {
                    return Answer::Errno(libc::EINVAL);
                }
                return match table::getgroups_count(&c, size as usize) {
                    Err(e) => Answer::Errno(e),
                    Ok(n) => {
                        if size > 0 {
                            let mem = match self.pin(req.id, tid) {
                                Ok(m) => m,
                                Err(e) => return e,
                            };
                            let b: Vec<u8> =
                                c.groups.iter().flat_map(|g| g.to_ne_bytes()).collect();
                            if !mem_write(mem.raw(), a[1], &b) {
                                return Answer::Errno(libc::EFAULT);
                            }
                        }
                        Answer::Val(n as i64)
                    }
                };
            }
            libc::SYS_setuid => SetId::Uid(u(0)),
            libc::SYS_setgid => SetId::Gid(u(0)),
            libc::SYS_setreuid => SetId::Reuid(u(0), u(1)),
            libc::SYS_setregid => SetId::Regid(u(0), u(1)),
            libc::SYS_setresuid => SetId::Resuid(u(0), u(1), u(2)),
            libc::SYS_setresgid => SetId::Resgid(u(0), u(1), u(2)),
            libc::SYS_setfsuid => SetId::Fsuid(u(0)),
            libc::SYS_setfsgid => SetId::Fsgid(u(0)),
            libc::SYS_setgroups => {
                let n = a[0] as i32;
                if n < 0 || n as usize > table::NGROUPS_MAX {
                    return Answer::Errno(libc::EINVAL);
                }
                let mem = match self.pin(req.id, tid) {
                    Ok(m) => m,
                    Err(e) => return e,
                };
                let Some(b) = mem_read(mem.raw(), a[1], n as usize * 4) else {
                    return Answer::Errno(libc::EFAULT);
                };
                SetId::Groups(
                    b.as_chunks::<4>()
                        .0
                        .iter()
                        .map(|x| u32::from_ne_bytes(*x))
                        .collect(),
                )
            }
            _ => return Answer::Errno(libc::ENOSYS),
        };
        match table::apply(&c, &op, caps(&c)) {
            Err(e) => Answer::Errno(e),
            Ok((new, ret)) => {
                if new != c
                    && let Err(e) = self.set_creds(tid, &new)
                {
                    return Answer::Errno(e);
                }
                Answer::Val(ret)
            }
        }
    }

    /// Serves a read-only openat of a `/proc/.../status` file by absolute
    /// path. The answer is a memfd holding the real contents with the virtual
    /// Uid, Gid and Groups lines. Code reads uids there, for example
    /// ActivityManager's Process.getUidForPid when it checks that a
    /// provider's process is still the same app, ps, and tests. Returns false
    /// to let the caller handle the openat.
    pub(crate) fn proc_status(&self, req: &SeccompNotif) -> bool {
        let tid = req.pid as i32;
        let a = req.data.args;
        let n = (4096 - (a[1] as usize & 4095)).min(6);
        match vm_read(tid, a[1], n) {
            Some(b) if b"/proc/".starts_with(&b) || b == b"/proc/" => {}
            _ => return false,
        }
        if a[2] as i32 & libc::O_ACCMODE != libc::O_RDONLY {
            return false;
        }
        let Some(path) = read_path(tid, a[1]) else {
            return false;
        };
        let Some(which) = table::parse_proc_status(&path) else {
            return false;
        };
        self.serve_status(req, which)
    }

    fn serve_status(&self, req: &SeccompNotif, which: table::ProcStatus) -> bool {
        let tid = req.pid as i32;
        // The file, and the process whose ids replace the file's own. The
        // kernel handles a missing pid with ENOENT.
        let (path, subject) = match which {
            table::ProcStatus::Myself => {
                let Some(p) = status_field(tid, "Tgid:").and_then(|v| v.parse::<i32>().ok()) else {
                    return false;
                };
                (format!("{p}"), p)
            }
            table::ProcStatus::MyThread => (format!("{tid}"), tid),
            table::ProcStatus::Pid(p) => (format!("{p}"), p as i32),
            table::ProcStatus::Task(p, t) => (format!("{p}/task/{t}"), t as i32),
        };
        let Ok(real) = std::fs::read_to_string(format!("/proc/{path}/status")) else {
            return false;
        };
        let Some(c) = self.creds(subject) else {
            return false;
        };
        let body = table::rewrite_status(&real, &c);
        let name = c"status";
        let fd = unsafe { libc::syscall(libc::SYS_memfd_create, name.as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return false;
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        let b = body.as_bytes();
        if unsafe { libc::write(fd.as_raw_fd(), b.as_ptr() as *const _, b.len()) }
            != b.len() as isize
            || unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET) } != 0
        {
            return false;
        }
        // On failure the target is gone or the id went stale. The open counts
        // as answered only if this succeeds. Otherwise let the kernel run it.
        self.listener.open_with(req, fd.as_raw_fd()) >= 0 || {
            self.listener.cont(req.id);
            true
        }
    }

    /// The caller's address space for a call that transfers memory. It is
    /// pinned (`/proc/<tid>/mem`) and then checked to be the caller's. A
    /// signal can end the notification wait and let the caller exec before
    /// the broker writes, so writing by numeric tid could reach the new image.
    fn pin(&self, id: u64, tid: i32) -> Result<PinnedMem, Answer> {
        let mem = open_mem(tid);
        if !self.listener.valid(id) {
            return Err(Answer::Deferred);
        }
        mem.ok_or(Answer::Errno(libc::ESRCH))
    }

    /// getsockopt(fd, SOL_SOCKET, SO_PEERCRED, optval, optlen).
    fn peercred(&self, id: u64, tid: i32, a: [u64; 6]) -> Answer {
        let mem = match self.pin(id, tid) {
            Ok(m) => m,
            Err(e) => return e,
        };
        let mem = mem.raw();
        let Some(len) = mem_read(mem, a[4], 4) else {
            return Answer::Errno(libc::EFAULT);
        };
        let len = i32::from_ne_bytes([len[0], len[1], len[2], len[3]]);
        if len < 0 {
            return Answer::Errno(libc::EINVAL);
        }
        let sock = match getfd(tid, a[0] as i32) {
            Ok(f) => f,
            Err(e) => return Answer::Errno(e),
        };
        let mut uc = libc::ucred {
            pid: 0,
            uid: 0,
            gid: 0,
        };
        let mut ul = size_of::<libc::ucred>() as libc::socklen_t;
        let r = unsafe {
            libc::getsockopt(
                sock.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_PEERCRED,
                &mut uc as *mut _ as *mut _,
                &mut ul,
            )
        };
        if r != 0 {
            return Answer::Errno(errno());
        }
        if uc.pid > 0 {
            let peer = uc.pid;
            match self.creds(peer) {
                Some(c) => {
                    uc.uid = c.euid;
                    uc.gid = c.egid;
                }
                None => {
                    uc.uid = AID_NOBODY;
                    uc.gid = AID_NOBODY;
                }
            }
            uc.pid = if peer == self.init { 1 } else { peer };
        }
        let n = (len as usize).min(size_of::<libc::ucred>());
        let bytes: [u8; 12] = unsafe { std::mem::transmute(uc) };
        if !mem_write(mem, a[3], &bytes[..n]) || !mem_write(mem, a[4], &(n as u32).to_ne_bytes()) {
            return Answer::Errno(libc::EFAULT);
        }
        Answer::Val(0)
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

fn write_ids(mem: MemFd, a: &[u64; 6], ids: [u32; 3]) -> Answer {
    for (i, id) in ids.iter().enumerate() {
        if !mem_write(mem, a[i], &id.to_ne_bytes()) {
            return Answer::Errno(libc::EFAULT);
        }
    }
    Answer::Val(0)
}

#[repr(C)]
#[derive(Default)]
struct Rlimit64 {
    cur: u64,
    max: u64,
}

fn get_rttime(pid: i32) -> Option<(u64, u64)> {
    let mut old = Rlimit64::default();
    let r = unsafe {
        libc::syscall(
            libc::SYS_prlimit64,
            pid,
            RLIMIT_RTTIME,
            std::ptr::null::<Rlimit64>(),
            &mut old,
        )
    };
    (r == 0).then_some((old.cur, old.max))
}

fn set_rttime(pid: i32, soft: u64, hard: u64) -> Result<(), i32> {
    let new = Rlimit64 {
        cur: soft,
        max: hard,
    };
    let r = unsafe {
        libc::syscall(
            libc::SYS_prlimit64,
            pid,
            RLIMIT_RTTIME,
            &new,
            std::ptr::null_mut::<Rlimit64>(),
        )
    };
    if r == 0 { Ok(()) } else { Err(errno()) }
}

/// CAP_SETUID and CAP_SETGID. Virtual root has both, and every other uid has
/// neither. A sandboxed `CapEff` is always 0, so there is no set to read.
fn caps(c: &Creds) -> table::Caps {
    table::Caps {
        setgid: c.euid == 0,
        setuid: c.euid == 0,
    }
}

/// A duplicate of fd `fd` of thread `tid`.
fn getfd(tid: i32, fd: i32) -> Result<OwnedFd, i32> {
    let mut pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, tid, PIDFD_THREAD) };
    if pidfd < 0 {
        pidfd = unsafe { libc::syscall(libc::SYS_pidfd_open, tid, 0) };
    }
    if pidfd < 0 {
        return Err(libc::ESRCH);
    }
    let pidfd = unsafe { OwnedFd::from_raw_fd(pidfd as RawFd) };
    let got = unsafe { libc::syscall(libc::SYS_pidfd_getfd, pidfd.as_raw_fd(), fd, 0) };
    if got < 0 {
        let e = errno();
        return Err(if e == libc::EBADF || e == libc::ESRCH {
            libc::EBADF
        } else {
            e
        });
    }
    Ok(unsafe { OwnedFd::from_raw_fd(got as RawFd) })
}

fn open_path(p: &str) -> Result<OwnedFd, i32> {
    let c = CString::new(p).map_err(|_| libc::EINVAL)?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_PATH | libc::O_CLOEXEC | libc::O_DIRECTORY,
        )
    };
    if fd < 0 {
        Err(errno())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

#[repr(C)]
struct OpenHow {
    flags: u64,
    mode: u64,
    resolve: u64,
}

/// Resolves `path` inside thread `tid`'s root and returns an O_PATH fd. A
/// relative path is relative to `dirfd`, which is AT_FDCWD or an fd number in
/// the target.
fn resolve(fs: &Fs, tid: i32, dirfd: i32, path: &[u8], nofollow: bool) -> Result<OwnedFd, i32> {
    resolve_how(fs, tid, dirfd, path, nofollow, 0)
}

fn resolve_how(
    fs: &Fs,
    tid: i32,
    dirfd: i32,
    path: &[u8],
    nofollow: bool,
    extra: u64,
) -> Result<OwnedFd, i32> {
    if path.is_empty() {
        return Err(libc::ENOENT);
    }
    // An absolute path resolves from the root. A relative path gets the
    // directory's path, as the target sees it, as a prefix, so that ".."
    // works and stays inside the root. If that path is unavailable, resolve
    // from the directory itself.
    let mut abs: Option<Vec<u8>> = None;
    if path[0] == b'/' {
        // /data is a symlink to its host dir.
        abs = Some(fs.to_host(path).unwrap_or_else(|| path.to_vec()));
    } else {
        let link = if dirfd == AT_FDCWD {
            format!("/proc/{tid}/cwd")
        } else {
            format!("/proc/{tid}/fd/{dirfd}")
        };
        if let Ok(d) = std::fs::read_link(&link) {
            use std::os::unix::ffi::OsStrExt;
            let d = d.as_os_str().as_bytes();
            if d.first() == Some(&b'/') && !d.ends_with(b" (deleted)") {
                let mut p = d.to_vec();
                p.push(b'/');
                p.extend_from_slice(path);
                abs = Some(p);
            }
        }
    }
    let (base, rel) = match abs {
        Some(p) => (open_path(&format!("/proc/{tid}/root"))?, p),
        None => {
            let base = if dirfd == AT_FDCWD {
                open_path(&format!("/proc/{tid}/cwd"))?
            } else {
                getfd(tid, dirfd)?
            };
            (base, path.to_vec())
        }
    };
    let rel = CString::new(rel).map_err(|_| libc::EINVAL)?;
    let how = OpenHow {
        flags: (libc::O_PATH | libc::O_CLOEXEC | if nofollow { libc::O_NOFOLLOW } else { 0 })
            as u64,
        mode: 0,
        resolve: RESOLVE_IN_ROOT | RESOLVE_NO_MAGICLINKS | extra,
    };
    let fd = unsafe {
        libc::syscall(
            SYS_OPENAT2,
            base.as_raw_fd(),
            rel.as_ptr(),
            &how,
            size_of::<OpenHow>(),
        )
    };
    if fd < 0 {
        Err(errno())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
    }
}

/// The recorded owner of the file behind O_PATH fd `fd`, if any.
fn recorded_owner(fd: &OwnedFd) -> Option<(u32, u32)> {
    let path = CString::new(format!("/proc/self/fd/{}", fd.as_raw_fd())).unwrap();
    let name = CString::new(table::OWNER_XATTR).unwrap();
    let mut buf = [0u8; 32];
    let n = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
        )
    };
    if n > 0 {
        table::parse_owner(&buf[..n as usize])
    } else {
        None
    }
}

/// Emulates lstat(path, st) for a path that can carry a recorded owner.
/// Paths with symlinks before the last component continue natively
/// (RESOLVE_NO_SYMLINKS), so a lookup can't wander onto a FUSE mount.
fn lstat(j: &Job, v: &Vid, mem: MemFd) -> Answer {
    let mut fd = match resolve_how(&v.fs, j.tid, AT_FDCWD, &j.path, true, RESOLVE_NO_SYMLINKS) {
        Ok(f) => f,
        Err(libc::ELOOP) | Err(libc::EXDEV) => return Answer::Continue,
        Err(e) => return Answer::Errno(e),
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::fstatat(fd.as_raw_fd(), c"".as_ptr(), &mut st, AT_EMPTY_PATH as i32) };
    if r != 0 {
        return Answer::Errno(errno());
    }
    // A bind mount inside /data is a symlink to another /data directory, such
    // as /data/user/0 -> /data/data. Report what the link stands for.
    if st.st_mode & libc::S_IFMT == libc::S_IFLNK
        && let Some(target) = crate::fs::bind_target(&fd)
        && let Ok(t) = resolve_how(&v.fs, j.tid, AT_FDCWD, &target, true, RESOLVE_NO_SYMLINKS)
        && unsafe { libc::fstatat(t.as_raw_fd(), c"".as_ptr(), &mut st, AT_EMPTY_PATH as i32) } == 0
    {
        fd = t;
    }
    let fmt = st.st_mode & libc::S_IFMT;
    if (fmt == libc::S_IFREG || fmt == libc::S_IFDIR)
        && let Some((u, g)) = recorded_owner(&fd)
    {
        st.st_uid = u;
        st.st_gid = g;
    } else {
        crate::fs::root_ids(&mut st);
    }
    let b = unsafe {
        std::slice::from_raw_parts(
            &st as *const libc::stat as *const u8,
            size_of::<libc::stat>(),
        )
    };
    // The path was resolved in tid's root. Check that tid is still the
    // caller.
    if !v.listener.valid(j.id) {
        return Answer::Deferred;
    }
    if !mem_write(mem, j.args[2], b) {
        return Answer::Errno(libc::EFAULT);
    }
    Answer::Val(0)
}

/// The recorded owner of `path` or, if it has none, of its closest ancestor
/// that has one. Files that an app creates in its directories without a chown
/// belong to the app, and APKs that system_server writes under /data/app
/// belong to system.
fn inherited_owner(fs: &Fs, tid: i32, path: &[u8], fd: &OwnedFd) -> (u32, u32) {
    if let Some(o) = recorded_owner(fd) {
        return o;
    }
    let mut p = path;
    for _ in 0..16 {
        let Some(i) = p.iter().rposition(|&b| b == b'/') else {
            break;
        };
        p = &p[..i];
        if !owner_tracked(p) {
            break;
        }
        if let Ok(f) = resolve(fs, tid, AT_FDCWD, p, false)
            && let Some(o) = recorded_owner(&f)
        {
            return o;
        }
    }
    (0, 0)
}

/// Emulates faccessat(AT_FDCWD, path, mode) for a non-root caller and a path
/// that can carry a recorded owner. The permission bits are checked against
/// the recorded or inherited owner and the caller's virtual ids, the way the
/// kernel checks real ones. ART refuses to load a dex file that the app can
/// write, which it checks with `access(W_OK)`, and every APK is writable to
/// the single kernel uid that the session runs as.
fn access(j: &Job, fs: &Fs) -> Answer {
    let Some(c) = &j.creds else {
        return Answer::Continue;
    };
    let mode = j.args[2] as u32 & 7;
    let flags = if j.nr == SYS_FACCESSAT2 { j.args[3] } else { 0 };
    let fd = match resolve(
        fs,
        j.tid,
        AT_FDCWD,
        &j.path,
        flags & AT_SYMLINK_NOFOLLOW != 0,
    ) {
        Ok(f) => f,
        Err(e) => return Answer::Errno(e),
    };
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstatat(fd.as_raw_fd(), c"".as_ptr(), &mut st, AT_EMPTY_PATH as i32) } != 0 {
        return Answer::Errno(errno());
    }
    let (owner, group) = inherited_owner(fs, j.tid, &j.path, &fd);
    let (uid, gid) = if flags & AT_EACCESS != 0 {
        (c.euid, c.egid)
    } else {
        (c.ruid, c.rgid)
    };
    if !table::may_access(st.st_mode, owner, group, uid, gid, &c.groups, mode) {
        return Answer::Errno(libc::EACCES);
    }
    if mode & 2 != 0 {
        let mut sv: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::fstatvfs(fd.as_raw_fd(), &mut sv) } == 0
            && sv.f_flag & libc::ST_RDONLY != 0
        {
            return Answer::Errno(libc::EROFS);
        }
    }
    Answer::Val(0)
}

/// Emulates one chown-family call (see the module docs).
fn chown(j: &Job, v: &Vid, mem: MemFd) -> Answer {
    let a = j.args;
    let (fd, uid, gid) = match j.nr {
        libc::SYS_fchown => (getfd(j.tid, a[0] as i32), a[1] as u32, a[2] as u32),
        libc::SYS_chown | libc::SYS_lchown => {
            let Some(p) = mem_read_path(mem, a[0]) else {
                return Answer::Errno(libc::EFAULT);
            };
            (
                resolve(&v.fs, j.tid, AT_FDCWD, &p, j.nr == libc::SYS_lchown),
                a[1] as u32,
                a[2] as u32,
            )
        }
        _ => {
            // fchownat(dirfd, path, uid, gid, flags)
            let Some(p) = mem_read_path(mem, a[1]) else {
                return Answer::Errno(libc::EFAULT);
            };
            let flags = a[4];
            let fd = if p.is_empty() && flags & AT_EMPTY_PATH != 0 {
                if a[0] as i32 == AT_FDCWD {
                    open_path(&format!("/proc/{}/cwd", j.tid))
                } else {
                    getfd(j.tid, a[0] as i32)
                }
            } else {
                resolve(
                    &v.fs,
                    j.tid,
                    a[0] as i32,
                    &p,
                    flags & AT_SYMLINK_NOFOLLOW != 0,
                )
            };
            (fd, a[2] as u32, a[3] as u32)
        }
    };
    let fd = match fd {
        Ok(f) => f,
        Err(e) => return Answer::Errno(e),
    };
    // The path and fd were resolved through tid. Check that tid is still the
    // caller.
    if !v.listener.valid(j.id) {
        return Answer::Deferred;
    }
    // Run the kernel's checks (read-only mounts, immutable files and so on)
    // and side effects (ctime, clearing set-id bits) without changing ids.
    let empty = c"";
    let r = unsafe {
        libc::fchownat(
            fd.as_raw_fd(),
            empty.as_ptr(),
            u32::MAX,
            u32::MAX,
            AT_EMPTY_PATH as i32,
        )
    };
    if r != 0 {
        return Answer::Errno(errno());
    }
    if uid == table::NO_ID && gid == table::NO_ID {
        return Answer::Val(0);
    }
    // Record the virtual owner. user.* xattrs exist only on regular files and
    // directories. On sockets, devices and symlinks the owner is dropped.
    let path = CString::new(format!("/proc/self/fd/{}", fd.as_raw_fd())).unwrap();
    let name = CString::new(table::OWNER_XATTR).unwrap();
    let mut buf = [0u8; 32];
    let n = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
        )
    };
    let cur = if n > 0 {
        table::parse_owner(&buf[..n as usize]).unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    let new = table::chown_owner(cur, uid, gid);
    if new == (0, 0) {
        if n > 0 {
            owner_writable(&path, || unsafe {
                libc::removexattr(path.as_ptr(), name.as_ptr())
            });
        }
    } else if new != cur {
        let v = table::format_owner(new.0, new.1);
        owner_writable(&path, || unsafe {
            libc::setxattr(
                path.as_ptr(),
                name.as_ptr(),
                v.as_ptr() as *const _,
                v.len(),
                0,
            )
        });
    }
    Answer::Val(0)
}

/// Runs `f`, an xattr write on `path`. If it fails with EACCES, retries with
/// the owner's write bit set for the call. Without a user namespace, as in
/// Flatpak mode, Android's root has no CAP_DAC_OVERRIDE. A user.* xattr needs
/// write permission, which a 0440 file such as an eBPF pin doesn't give.
fn owner_writable(path: &CString, f: impl Fn() -> libc::c_int) {
    if f() == 0 || errno() != libc::EACCES {
        return;
    }
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::stat(path.as_ptr(), &mut st) } != 0 || st.st_uid != unsafe { libc::getuid() }
    {
        return;
    }
    let mode = st.st_mode & 0o7777;
    if unsafe { libc::chmod(path.as_ptr(), mode | 0o200) } == 0 {
        f();
        unsafe { libc::chmod(path.as_ptr(), mode) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notify::vm_read;

    /// The inline answers (getresuid and friends, SO_PEERCRED, getgroups)
    /// write through the caller's pinned address space. Once the caller has
    /// exec'd, the write fails instead of landing in the new image, which a
    /// write by numeric tid would reach.
    #[test]
    fn inline_writes_do_not_follow_exec() {
        use std::io::Write;
        let mut child = std::process::Command::new("sh")
            .args(["-c", "read x; exec sleep 10"])
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let mem = open_mem(pid).unwrap();
        child.stdin.take().unwrap().write_all(b"go\n").unwrap();
        let exe = |p: i32| std::fs::read_link(format!("/proc/{p}/exe")).unwrap_or_default();
        for _ in 0..500 {
            if exe(pid).ends_with("sleep") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(exe(pid).ends_with("sleep"));
        // A writable address in the new image.
        let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap();
        let line = maps.lines().find(|l| l.contains(" rw-p ")).unwrap();
        let addr = u64::from_str_radix(line.split('-').next().unwrap(), 16).unwrap();
        let before = vm_read(pid, addr, 12).unwrap();
        let a = [addr, addr + 4, addr + 8, 0, 0, 0];
        let r = write_ids(mem.raw(), &a, [0x1111, 0x2222, 0x3333]);
        assert!(matches!(r, Answer::Errno(libc::EFAULT)));
        assert_eq!(vm_read(pid, addr, 12).unwrap(), before);
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
