//! Flatpak mode (`LINUXAPK_FLATPAK=1`): the session runs in the sandbox's own
//! namespaces, with no mounts. Android's root is the sandbox's `/`, where
//! `/system`, `/apex`, `/data`... are symlinks into a composed tree on disk
//! (`host/launcher/src/flatpak.rs`). The broker covers what the mounts did:
//!
//!  * `readlink` of `/proc` magic links (`/proc/self/fd/N`, `exe`, `cwd`) and
//!    `getcwd` name the host path behind a symlink, which the linker and
//!    bionic's `realpath` would take as the canonical one (and then refuse a
//!    library outside its namespace's `/system/...` paths). Paths under a
//!    mapped host dir are shown as their Android path.
//!  * `fstat` of a file the user owns reports owner root: there is no
//!    user namespace mapping the user to Android's root, and bionic only
//!    trusts root-owned property areas (as in a single-uid session, the
//!    files are all the user's anyway).
//!  * SO_PEERCRED shows init as pid 1 (see [`init_as_pid1`]).
//!  * lstat of a symlink that stands in for a bind mount inside /data
//!    (/data/user/0 -> /data/data) shows the directory (see [`bind_target`]).
//!  * `mkdir` and the `chmod` family keep a directory's owner rwx: with no
//!    capabilities, Android's root can't write into a 0511 directory such as
//!    `/data/user` that it made itself. (Files keep their mode: code checks
//!    exact modes, such as ClatCoordinator's 0440 eBPF pins.)
//!  * `openat` of a `/proc` file the session fakes (`/linuxapk/proc/<path>`,
//!    which bind mounts covered otherwise) gets a memfd copy of it, and
//!    apexd's `/proc/mounts` lists the pre-mounted APEXes
//!    (`/linuxapk/apexd-mounts`, as `linuxapk-apexd` does with a bind mount).

use std::ffi::OsStr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use crate::notify::{
    SECCOMP_ADDFD_FLAG_SEND, SeccompNotif, SeccompNotifAddfd, mem_write, notif_addfd_cmd,
    notif_id_valid, open_mem, read_path, send_resp, status_field, vm_read,
};

pub(crate) struct Flat {
    listener: RawFd,
    resp_sz: usize,
    /// (host dir, Android path), longest host dir first.
    map: Vec<(Vec<u8>, Vec<u8>)>,
    /// The kernel's uid and gid for every Android process (the user's).
    uid: u32,
    gid: u32,
}

const SYS_FCHMODAT2: i64 = 452;

/// The syscalls [`Flat::serve`] answers.
pub(crate) fn is_flat_syscall(nr: i64) -> bool {
    matches!(
        nr,
        libc::SYS_readlinkat
            | libc::SYS_readlink
            | libc::SYS_getcwd
            | libc::SYS_fstat
            | libc::SYS_mkdir
            | libc::SYS_mkdirat
            | libc::SYS_chmod
            | libc::SYS_fchmod
            | libc::SYS_fchmodat
            | SYS_FCHMODAT2
    )
}

/// Flatpak mode's path map, for the credential side (`creds`): Android
/// paths of mapped host dirs (such as `/data`) as host paths, so lookups
/// that refuse symlinks can start past them. None outside Flatpak mode.
pub(crate) fn to_host(p: &[u8]) -> Option<Vec<u8>> {
    static F: std::sync::OnceLock<Option<Flat>> = std::sync::OnceLock::new();
    let f = F.get_or_init(|| Flat::from_env(-1, 0)).as_ref()?;
    for (host, android) in &f.map {
        if android.as_slice() == b"/" {
            // The composed root: "/" has a symlink to it per partition
            // (/system, /apex...).
            let first = p.get(1..)?.split(|&c| c == b'/').next()?;
            let mut target = host.clone();
            target.push(b'/');
            target.extend_from_slice(first);
            let mut link = b"/".to_vec();
            link.extend_from_slice(first);
            if first.is_empty()
                || std::fs::read_link(OsStr::from_bytes(&link))
                    .map_or(true, |t| t.as_os_str().as_bytes() != target.as_slice())
            {
                continue;
            }
            let mut out = host.clone();
            out.extend_from_slice(p);
            return Some(out);
        }
        let Some(rest) = p.strip_prefix(android.as_slice()) else {
            continue;
        };
        if rest.is_empty() || rest[0] == b'/' {
            let mut out = host.clone();
            out.extend_from_slice(rest);
            return Some(out);
        }
    }
    None
}

/// Flatpak mode: init isn't pid 1, but its property service treats a peer
/// that isn't pid 1 as a client (and init's own setprop from its restorecon
/// thread as a new restorecon request, forever). Show init as pid 1 to
/// SO_PEERCRED; pivot-exec passes its pid in `LINUXAPK_INIT_PID`.
pub(crate) fn init_as_pid1(pid: i32) -> i32 {
    static INIT: std::sync::OnceLock<Option<i32>> = std::sync::OnceLock::new();
    let init = INIT.get_or_init(|| std::env::var("LINUXAPK_INIT_PID").ok()?.parse().ok());
    if *init == Some(pid) { 1 } else { pid }
}

/// In Flatpak mode, the Android path a symlink (O_PATH fd) that stands in
/// for a bind mount inside /data points to (see host/launcher/src/flatpak.rs).
pub(crate) fn bind_target(link: &impl AsRawFd) -> Option<Vec<u8>> {
    std::env::var_os("LINUXAPK_FLATPAK").filter(|v| v == "1")?;
    let mut buf = [0u8; 256];
    let n = unsafe {
        libc::readlinkat(
            link.as_raw_fd(),
            c"".as_ptr(),
            buf.as_mut_ptr() as *mut _,
            buf.len(),
        )
    };
    let t = buf.get(..usize::try_from(n).ok()?)?;
    (t.starts_with(b"/data/") && !t.split(|&c| c == b'/').any(|c| c == b"..")).then(|| t.to_vec())
}

/// In Flatpak mode, `st`'s owner as Android sees it: the user's ids are
/// root's (see [`Flat::fstat`]).
pub(crate) fn root_ids(st: &mut libc::stat) {
    static IDS: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    let ids = IDS.get_or_init(|| {
        (std::env::var("LINUXAPK_FLATPAK").as_deref() == Ok("1"))
            .then(|| unsafe { (libc::getuid(), libc::getgid()) })
    });
    if let Some((u, g)) = *ids {
        if st.st_uid == u {
            st.st_uid = 0;
        }
        if st.st_gid == g {
            st.st_gid = 0;
        }
    }
}

impl Flat {
    /// From `LINUXAPK_FLATPAK=1` and `LINUXAPK_ROOT_MAP` (lines of
    /// `<host dir>=<Android path>`), or None outside Flatpak mode.
    pub(crate) fn from_env(listener: RawFd, resp_sz: usize) -> Option<Flat> {
        if std::env::var("LINUXAPK_FLATPAK").as_deref() != Ok("1") {
            return None;
        }
        let spec = std::env::var("LINUXAPK_ROOT_MAP").unwrap_or_default();
        let mut map: Vec<(Vec<u8>, Vec<u8>)> = spec
            .lines()
            .filter_map(|l| l.rsplit_once('='))
            .map(|(h, a)| {
                (
                    h.trim_end_matches('/').as_bytes().to_vec(),
                    a.as_bytes().to_vec(),
                )
            })
            .filter(|(h, _)| !h.is_empty())
            .collect();
        map.sort_by_key(|(h, _)| std::cmp::Reverse(h.len()));
        Some(Flat {
            listener,
            resp_sz,
            map,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
        })
    }

    /// `p` with a mapped host dir replaced by its Android path.
    fn to_android(&self, p: &[u8]) -> Vec<u8> {
        for (host, android) in &self.map {
            let Some(rest) = p.strip_prefix(host.as_slice()) else {
                continue;
            };
            if !rest.is_empty() && rest[0] != b'/' {
                continue;
            }
            let mut out = android.clone();
            if out.last() == Some(&b'/') && !rest.is_empty() {
                out.pop();
            }
            out.extend_from_slice(rest);
            if out.is_empty() {
                out.push(b'/');
            }
            return out;
        }
        p.to_vec()
    }

    fn reply(&self, id: u64, val: i64, errno: i32) {
        send_resp(self.listener, self.resp_sz, id, val, -errno, false);
    }

    fn cont(&self, id: u64) {
        send_resp(self.listener, self.resp_sz, id, 0, 0, true);
    }

    /// Answer readlink/readlinkat/getcwd/fstat.
    pub(crate) fn serve(&self, req: &SeccompNotif) {
        let tid = req.pid as i32;
        let a = req.data.args;
        let nr = req.data.nr as i64;
        match nr {
            libc::SYS_fstat => return self.fstat(req),
            libc::SYS_mkdir | libc::SYS_mkdirat => return self.mkdir(req),
            libc::SYS_chmod | libc::SYS_fchmod | libc::SYS_fchmodat | SYS_FCHMODAT2 => {
                return self.chmod(req);
            }
            _ => {}
        }
        let (link, buf, size) = if nr == libc::SYS_getcwd {
            (format!("/proc/{tid}/cwd").into_bytes(), a[0], a[1] as usize)
        } else {
            let (path, buf, size) = if nr == libc::SYS_readlink {
                (a[0], a[1], a[2] as usize)
            } else {
                // Only absolute paths: the /proc magic links are named so.
                (a[1], a[2], a[3] as usize)
            };
            match read_path(tid, path).and_then(|p| proc_path(tid, &p)) {
                Some(p) => (p, buf, size),
                None => return self.cont(req.id),
            }
        };
        let target = match std::fs::read_link(OsStr::from_bytes(&link)) {
            Ok(t) => t,
            // E.g. a non-dumpable process's fds: it can read its own.
            Err(_) => return self.cont(req.id),
        };
        let mut out = self.to_android(target.as_os_str().as_bytes());
        let n = if nr == libc::SYS_getcwd {
            out.push(0);
            if out.len() > size {
                return self.reply(req.id, 0, libc::ERANGE);
            }
            out.len()
        } else {
            out.len().min(size)
        };
        let Some(mem) = open_mem(tid) else {
            return self.cont(req.id);
        };
        if !notif_id_valid(self.listener, req.id) {
            return;
        }
        if !mem_write(mem.raw(), buf, &out[..n]) {
            return self.reply(req.id, 0, libc::EFAULT);
        }
        self.reply(req.id, n as i64, 0);
    }

    /// fstat(fd, st) through the caller's /proc fd link, with the user's ids
    /// shown as root's.
    fn fstat(&self, req: &SeccompNotif) {
        let tid = req.pid as i32;
        let (fd, buf) = (req.data.args[0] as i32, req.data.args[1]);
        let Ok(link) = std::ffi::CString::new(format!("/proc/{tid}/fd/{fd}")) else {
            return self.cont(req.id);
        };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        if fd < 0
            || unsafe { libc::stat(link.as_ptr(), &mut st) } != 0
            || (st.st_uid != self.uid && st.st_gid != self.gid)
        {
            return self.cont(req.id);
        }
        if st.st_uid == self.uid {
            st.st_uid = 0;
        }
        if st.st_gid == self.gid {
            st.st_gid = 0;
        }
        let b = unsafe {
            std::slice::from_raw_parts(
                &st as *const libc::stat as *const u8,
                size_of::<libc::stat>(),
            )
        };
        let Some(mem) = open_mem(tid) else {
            return self.cont(req.id);
        };
        if !notif_id_valid(self.listener, req.id) {
            return;
        }
        if !mem_write(mem.raw(), buf, b) {
            return self.reply(req.id, 0, libc::EFAULT);
        }
        self.reply(req.id, 0, 0);
    }

    /// The host path a path argument names for task `tid` (`dirfd` for
    /// relative ones, AT_FDCWD for its cwd); None to let the kernel run it.
    fn target_path(&self, tid: i32, dirfd: i32, addr: u64) -> Option<std::ffi::CString> {
        let p = read_path(tid, addr)?;
        if p.is_empty() {
            return None;
        }
        let full = if p[0] == b'/' {
            p
        } else {
            let link = if dirfd == libc::AT_FDCWD {
                format!("/proc/{tid}/cwd")
            } else {
                format!("/proc/{tid}/fd/{dirfd}")
            };
            let mut d = std::fs::read_link(link).ok()?.into_os_string().into_vec();
            d.push(b'/');
            d.extend_from_slice(&p);
            d
        };
        std::ffi::CString::new(full).ok()
    }

    /// mkdir/mkdirat with the owner's rwx kept (the caller's umask applies).
    fn mkdir(&self, req: &SeccompNotif) {
        let tid = req.pid as i32;
        let a = req.data.args;
        let (dirfd, path, mode) = if req.data.nr as i64 == libc::SYS_mkdir {
            (libc::AT_FDCWD, a[0], a[1] as u32)
        } else {
            (a[0] as i32, a[1], a[2] as u32)
        };
        let Some(p) = self.target_path(tid, dirfd, path) else {
            return self.cont(req.id);
        };
        let umask = status_field(tid, "Umask:")
            .and_then(|u| u32::from_str_radix(&u, 8).ok())
            .unwrap_or(0o022);
        let mode = (mode & 0o7777 & !umask) | 0o700;
        if !notif_id_valid(self.listener, req.id) {
            return;
        }
        // The broker's own umask is 0 (main()).
        let r = unsafe { libc::mkdir(p.as_ptr(), mode) };
        self.answer(req.id, r);
    }

    /// The chmod family, with a directory's owner rwx kept.
    fn chmod(&self, req: &SeccompNotif) {
        let tid = req.pid as i32;
        let a = req.data.args;
        let nr = req.data.nr as i64;
        let (p, mode, nofollow) = match nr {
            libc::SYS_fchmod => {
                match std::ffi::CString::new(format!("/proc/{tid}/fd/{}", a[0] as i32)) {
                    Ok(p) => (Some(p), a[1] as u32, false),
                    Err(_) => (None, 0, false),
                }
            }
            libc::SYS_chmod => (
                self.target_path(tid, libc::AT_FDCWD, a[0]),
                a[1] as u32,
                false,
            ),
            _ => (
                self.target_path(tid, a[0] as i32, a[1]),
                a[2] as u32,
                nr == SYS_FCHMODAT2 && a[3] & libc::AT_SYMLINK_NOFOLLOW as u64 != 0,
            ),
        };
        let Some(p) = p else {
            return self.cont(req.id);
        };
        let mut st: libc::stat = unsafe { std::mem::zeroed() };
        let r = if nofollow {
            unsafe { libc::lstat(p.as_ptr(), &mut st) }
        } else {
            unsafe { libc::stat(p.as_ptr(), &mut st) }
        };
        if r != 0 || st.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return self.cont(req.id);
        }
        if !notif_id_valid(self.listener, req.id) {
            return;
        }
        let r = unsafe { libc::chmod(p.as_ptr(), (mode & 0o7777) | 0o700) };
        self.answer(req.id, r);
    }

    /// Answer with a libc return value (0, or -1 and errno).
    fn answer(&self, id: u64, r: libc::c_int) {
        if r == 0 {
            self.reply(id, 0, 0);
        } else {
            self.reply(
                id,
                0,
                std::io::Error::last_os_error()
                    .raw_os_error()
                    .unwrap_or(libc::EIO),
            );
        }
    }

    /// Serve an `openat` of a faked /proc file; false if it isn't one.
    pub(crate) fn openat(&self, req: &SeccompNotif) -> bool {
        let tid = req.pid as i32;
        let a = req.data.args;
        let n = (4096 - (a[1] as usize & 4095)).min(6);
        match vm_read(tid, a[1], n) {
            Some(b) if b"/proc/".starts_with(&b) || b == b"/proc/" => {}
            _ => return false,
        }
        let Some(path) = read_path(tid, a[1]) else {
            return false;
        };
        if path.split(|&c| c == b'/').any(|c| c == b"..") {
            return false;
        }
        // apexd's mount table (init watches its own with epoll, which a memfd
        // can't serve).
        let body = if (path == b"/proc/mounts" || path == b"/proc/self/mounts")
            && std::fs::read(format!("/proc/{tid}/comm")).is_ok_and(|c| c == b"apexd\n")
        {
            let (Ok(mut m), Ok(apex)) = (
                std::fs::read(format!("/proc/{tid}/mounts")),
                std::fs::read("/linuxapk/apexd-mounts"),
            ) else {
                return false;
            };
            m.extend_from_slice(&apex);
            m
        } else {
            let rest = &path[b"/proc/".len()..];
            if rest.is_empty() || rest.split(|&c| c == b'/').any(|c| c.is_empty()) {
                return false;
            }
            let mut fake = b"/linuxapk/proc/".to_vec();
            fake.extend_from_slice(rest);
            match std::fs::read(OsStr::from_bytes(&fake)) {
                Ok(b) => b,
                Err(_) => return false,
            }
        };
        self.inject(req, &body);
        true
    }

    /// Complete `req` (an openat) with a memfd holding `body`.
    fn inject(&self, req: &SeccompNotif, body: &[u8]) {
        let fd =
            unsafe { libc::syscall(libc::SYS_memfd_create, c"proc".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return self.reply(req.id, 0, libc::ENOMEM);
        }
        let fd = unsafe { OwnedFd::from_raw_fd(fd as RawFd) };
        if unsafe { libc::write(fd.as_raw_fd(), body.as_ptr() as *const _, body.len()) }
            != body.len() as isize
            || unsafe { libc::lseek(fd.as_raw_fd(), 0, libc::SEEK_SET) } != 0
        {
            return self.reply(req.id, 0, libc::EIO);
        }
        let add = SeccompNotifAddfd {
            id: req.id,
            flags: SECCOMP_ADDFD_FLAG_SEND,
            srcfd: fd.as_raw_fd() as u32,
            newfd: 0,
            newfd_flags: (req.data.args[2] as u32) & libc::O_CLOEXEC as u32,
        };
        unsafe {
            libc::ioctl(
                self.listener,
                notif_addfd_cmd(size_of::<SeccompNotifAddfd>()) as _,
                &add as *const _,
            )
        };
    }
}

/// `/proc/self/...` and `/proc/thread-self/...` as the broker must name them
/// for task `tid`; None for paths outside /proc.
fn proc_path(tid: i32, p: &[u8]) -> Option<Vec<u8>> {
    let rest = p.strip_prefix(b"/proc/")?;
    let tgid = || status_field(tid, "Tgid:");
    let mut out = b"/proc/".to_vec();
    let word = |w: &[u8]| {
        rest.strip_prefix(w)
            .filter(|r| r.is_empty() || r[0] == b'/')
            .map(|r| r.to_vec())
    };
    if let Some(r) = word(b"self") {
        out.extend_from_slice(tgid()?.as_bytes());
        out.extend_from_slice(&r);
    } else if let Some(r) = word(b"thread-self") {
        out.extend_from_slice(format!("{}/task/{tid}", tgid()?).as_bytes());
        out.extend_from_slice(&r);
    } else {
        out.extend_from_slice(rest);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(map: &str) -> Flat {
        unsafe { std::env::set_var("LINUXAPK_FLATPAK", "1") };
        unsafe { std::env::set_var("LINUXAPK_ROOT_MAP", map) };
        Flat::from_env(-1, 0).unwrap()
    }

    #[test]
    fn maps_host_dirs_to_android_paths() {
        let f = flat("/var/r=/\n/var/d/data-x=/data");
        assert_eq!(
            f.to_android(b"/var/r/system/lib64/libc.so"),
            b"/system/lib64/libc.so"
        );
        assert_eq!(f.to_android(b"/var/r"), b"/");
        assert_eq!(f.to_android(b"/var/d/data-x/app/x.apk"), b"/data/app/x.apk");
        assert_eq!(f.to_android(b"/var/d/data-x"), b"/data");
        assert_eq!(f.to_android(b"/var/rr/x"), b"/var/rr/x");
        assert_eq!(f.to_android(b"socket:[1]"), b"socket:[1]");
    }
}
