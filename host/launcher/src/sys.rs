//! Thin wrappers over the syscalls the launcher makes. Errors name the path
//! involved.

use std::ffi::{CString, OsStr};
use std::fs;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use anyhow::{Context, Result};

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

/// Opens a pidfd for `pid`. The fd becomes readable once the process exits.
pub fn pidfd(pid: u32) -> io::Result<OwnedFd> {
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd as RawFd) })
}

/// Calls poll(2) on `fds`, retrying on EINTR, and returns each fd's revents.
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

/// Returns the children of `pid` whose command name is in `names`, or all of
/// them if `names` is empty. Scans /proc.
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
        // The format is "pid (comm) state ppid ...". comm may contain spaces
        // and parentheses.
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

/// Returns (pid, ppid) for every process in /proc.
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

/// Kills every descendant of this process. They are all stopped first so that
/// none is reparented to the sandbox's pid 1, out of reach, when its parent
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

/// Blocks `sigs` in this thread and the threads it starts, and returns a set
/// for [`wait_signal`]. Child processes get an empty mask from std.
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
