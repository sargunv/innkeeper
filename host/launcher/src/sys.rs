//! Thin wrappers over the syscalls the launcher makes, with the path in every
//! error.

use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use anyhow::{Context, Result};

pub use libc::{
    MS_BIND, MS_NODEV, MS_NOEXEC, MS_NOSUID, MS_PRIVATE, MS_RDONLY, MS_REC, MS_REMOUNT,
};

fn cstr(s: impl AsRef<OsStr>) -> CString {
    CString::new(s.as_ref().as_bytes()).expect("no NUL in paths")
}

fn cvt(r: libc::c_int) -> io::Result<libc::c_int> {
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r)
    }
}

/// mount(2): the classic call, whose data string may be a page long (the
/// fd-based API caps an option value at 256 bytes, which a stack of long
/// overlay layer paths exceeds).
pub fn mount(
    src: &str,
    dst: &Path,
    fstype: Option<&str>,
    flags: libc::c_ulong,
    data: Option<&str>,
) -> Result<()> {
    let s = cstr(src);
    let d = cstr(dst);
    let t = fstype.map(cstr);
    let o = data.map(cstr);
    cvt(unsafe {
        libc::mount(
            s.as_ptr(),
            d.as_ptr(),
            t.as_ref().map_or(std::ptr::null(), |t| t.as_ptr()),
            flags,
            o.as_ref()
                .map_or(std::ptr::null(), |o| o.as_ptr().cast::<libc::c_void>()),
        )
    })
    .with_context(|| format!("mount {} on {} ({fstype:?}, {data:?})", src, dst.display()))?;
    Ok(())
}

/// A bind mount of `src` on `dst`.
pub fn bind(src: &Path, dst: &Path, recursive: bool) -> Result<()> {
    let flags = MS_BIND | if recursive { MS_REC } else { 0 };
    mount(&src.to_string_lossy(), dst, None, flags, None)
}

/// Remounts the bind mount at `dst` read-only plus `extra`, keeping the
/// flags it has (a user namespace may not clear locked ones).
pub fn remount_ro(dst: &Path, extra: libc::c_ulong) -> Result<()> {
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let p = cstr(dst);
    cvt(unsafe { libc::statvfs(p.as_ptr(), &mut st) })
        .with_context(|| format!("statvfs {}", dst.display()))?;
    let mut keep = 0;
    for (st_flag, ms_flag) in [
        (libc::ST_NOSUID, libc::MS_NOSUID),
        (libc::ST_NODEV, libc::MS_NODEV),
        (libc::ST_NOEXEC, libc::MS_NOEXEC),
        (libc::ST_NOATIME, libc::MS_NOATIME),
        (libc::ST_NODIRATIME, libc::MS_NODIRATIME),
        (libc::ST_RELATIME, libc::MS_RELATIME),
    ] {
        if st.f_flag & st_flag != 0 {
            keep |= ms_flag;
        }
    }
    mount(
        "none",
        dst,
        None,
        MS_REMOUNT | MS_BIND | MS_RDONLY | keep | extra,
        None,
    )
}

pub fn unshare(flags: libc::c_int) -> Result<()> {
    cvt(unsafe { libc::unshare(flags) }).with_context(|| format!("unshare {flags:#x}"))?;
    Ok(())
}

pub fn setns(fd: &OwnedFd, nstype: libc::c_int) -> io::Result<()> {
    cvt(unsafe { libc::setns(fd.as_raw_fd(), nstype) })?;
    Ok(())
}

/// fork(2). Only call it while the process has one thread.
pub fn fork() -> Result<libc::pid_t> {
    let p = unsafe { libc::fork() };
    if p < 0 {
        return Err(io::Error::last_os_error()).context("fork");
    }
    Ok(p)
}

/// Waits for `pid`; returns its exit code (128 + signal if killed).
pub fn wait(pid: libc::pid_t) -> io::Result<i32> {
    let mut st = 0;
    loop {
        if unsafe { libc::waitpid(pid, &mut st, 0) } >= 0 {
            break;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
    Ok(if libc::WIFEXITED(st) {
        libc::WEXITSTATUS(st)
    } else {
        128 + libc::WTERMSIG(st)
    })
}

/// The calling process gets `sig` when its parent dies.
pub fn die_with_parent(sig: libc::c_int) {
    unsafe { libc::prctl(libc::PR_SET_PDEATHSIG, sig as libc::c_ulong, 0, 0, 0) };
}

pub fn kill(pid: i32, sig: libc::c_int) {
    if pid > 0 {
        unsafe { libc::kill(pid, sig) };
    }
}

pub fn mkfifo(p: &Path, mode: libc::mode_t) -> Result<()> {
    let c = cstr(p);
    cvt(unsafe { libc::mkfifo(c.as_ptr(), mode) })
        .with_context(|| format!("mkfifo {}", p.display()))?;
    Ok(())
}

pub fn chroot_here() -> io::Result<()> {
    cvt(unsafe { libc::chroot(c".".as_ptr()) })?;
    Ok(())
}

pub fn fchdir(fd: &OwnedFd) -> io::Result<()> {
    cvt(unsafe { libc::fchdir(fd.as_raw_fd()) })?;
    Ok(())
}

pub fn open_path(p: &Path) -> io::Result<OwnedFd> {
    let c = cstr(p);
    let fd = cvt(unsafe { libc::open(c.as_ptr(), libc::O_RDONLY | libc::O_CLOEXEC) })?;
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// A pidfd for `pid`, readable once it exits.
pub fn pidfd(pid: u32) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// poll(2) on `fds` for `events`; returns each fd's revents.
pub fn poll(fds: &[(RawFd, i16)], timeout_ms: i32) -> io::Result<Vec<i16>> {
    let mut p: Vec<libc::pollfd> = fds
        .iter()
        .map(|&(fd, events)| libc::pollfd {
            fd,
            events,
            revents: 0,
        })
        .collect();
    loop {
        let r = unsafe { libc::poll(p.as_mut_ptr(), p.len() as libc::nfds_t, timeout_ms) };
        if r >= 0 {
            return Ok(p.iter().map(|p| p.revents).collect());
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

pub fn uid() -> u32 {
    unsafe { libc::getuid() }
}

pub fn gid() -> u32 {
    unsafe { libc::getgid() }
}

pub fn pid() -> i32 {
    std::process::id() as i32
}

/// The user's login name, from the passwd database.
pub fn user_name() -> Option<String> {
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut out = std::ptr::null_mut();
    let r = unsafe { libc::getpwuid_r(uid(), &mut pw, buf.as_mut_ptr(), buf.len(), &mut out) };
    if r != 0 || out.is_null() {
        return None;
    }
    let name = unsafe { std::ffi::CStr::from_ptr(pw.pw_name) };
    Some(name.to_string_lossy().into_owned())
}

/// The running kernel's release (`uname -r`).
pub fn kernel_release() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// (major, minor) of a kernel release such as `7.1.9-200.fc44.x86_64`.
pub fn kernel_version(release: &str) -> Option<(u32, u32)> {
    let mut it = release.split(|c: char| !c.is_ascii_digit());
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// Whether `pid` is a live process.
pub fn alive(pid: i32) -> bool {
    pid > 0 && Path::new(&format!("/proc/{pid}")).exists()
}

/// The children of `pid` whose command name is one of `names`, by scanning
/// /proc (any name if `names` is empty).
pub fn children(pid: i32, names: &[&str]) -> Vec<i32> {
    let mut v = vec![];
    let Ok(dir) = fs::read_dir("/proc") else {
        return v;
    };
    for e in dir.flatten() {
        let Some(p) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{p}/stat")) else {
            continue;
        };
        // "pid (comm) state ppid ...": comm may hold spaces and parens.
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let comm = &stat[open + 1..close];
        let ppid = stat[close + 1..]
            .split_whitespace()
            .nth(1)
            .and_then(|s| s.parse::<i32>().ok());
        if ppid == Some(pid) && (names.is_empty() || names.contains(&comm)) {
            v.push(p);
        }
    }
    v.sort();
    v
}

/// Every process's parent, from /proc.
fn parents() -> Vec<(i32, i32)> {
    let mut v = vec![];
    for e in fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Some(p) = e.file_name().to_str().and_then(|s| s.parse::<i32>().ok()) else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(format!("/proc/{p}/stat")) else {
            continue;
        };
        if let Some(ppid) = stat
            .rfind(')')
            .and_then(|c| stat[c + 1..].split_whitespace().nth(1))
            .and_then(|s| s.parse().ok())
        {
            v.push((p, ppid));
        }
    }
    v
}

/// Kills every descendant of this process. They are all stopped first, so
/// none is reparented out of reach (to the sandbox's pid 1) while its parent
/// dies.
pub fn kill_descendants() {
    let me = pid();
    let mut stopped: Vec<i32> = vec![];
    for _ in 0..50 {
        let all = parents();
        let mut tree = vec![me];
        let mut i = 0;
        while i < tree.len() {
            let parent = tree[i];
            tree.extend(all.iter().filter(|(_, pp)| *pp == parent).map(|(p, _)| *p));
            i += 1;
        }
        let new: Vec<i32> = tree[1..]
            .iter()
            .copied()
            .filter(|p| !stopped.contains(p))
            .collect();
        if new.is_empty() {
            break;
        }
        for p in new {
            kill(p, libc::SIGSTOP);
            stopped.push(p);
        }
    }
    for p in stopped {
        kill(p, libc::SIGKILL);
    }
}

/// Blocks `sigs` in this thread and the threads it starts (child processes
/// get an empty mask from std), and returns a set for [`wait_signal`].
pub fn block_signals(sigs: &[libc::c_int]) -> libc::sigset_t {
    unsafe {
        let mut set: libc::sigset_t = std::mem::zeroed();
        libc::sigemptyset(&mut set);
        for &s in sigs {
            libc::sigaddset(&mut set, s);
        }
        libc::pthread_sigmask(libc::SIG_BLOCK, &set, std::ptr::null_mut());
        set
    }
}

/// Waits for one of the blocked signals in `set`.
pub fn wait_signal(set: &libc::sigset_t) -> libc::c_int {
    let mut sig = 0;
    unsafe { libc::sigwait(set, &mut sig) };
    sig
}

pub fn ppid() -> i32 {
    unsafe { libc::getppid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_versions() {
        assert_eq!(kernel_version("7.1.9-200.fc44.x86_64"), Some((7, 1)));
        assert_eq!(kernel_version("6.5.0"), Some((6, 5)));
        assert_eq!(kernel_version("5.14"), Some((5, 14)));
        assert_eq!(kernel_version("bogus"), None);
    }
}
