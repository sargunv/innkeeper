//! The session's control socket. Each `flatpak run` is a sandbox of its own,
//! with its own pid namespace and `/`, so the CLI or the manager can't enter
//! the session's sandbox. The session's launcher listens on
//! `$XDG_RUNTIME_DIR/app/<app id>/innkeeper-<profile>/control.sock`, which
//! every instance of the app shares, and serves `status`, `stop` and `exec`.
//!
//! One SOCK_SEQPACKET message per request: NUL-separated words (the command,
//! then exec's argv), with exec's stdin, stdout and stderr as SCM_RIGHTS. The
//! reply is one message: status's text, exec's exit code, or `ok` for stop
//! (the connection then closes when the session has ended).

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::mem::{size_of, zeroed};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::exec;
use crate::sys;

/// A request's largest size (an exec's argv).
const MAX_MSG: usize = 256 << 10;

fn socket() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

fn addr(path: &Path) -> Result<(libc::sockaddr_un, libc::socklen_t)> {
    let mut a: libc::sockaddr_un = unsafe { zeroed() };
    a.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let b = path.as_os_str().as_bytes();
    if b.len() >= a.sun_path.len() {
        bail!("socket path too long: {}", path.display());
    }
    for (d, s) in a.sun_path.iter_mut().zip(b) {
        *d = *s as libc::c_char;
    }
    Ok((
        a,
        (size_of::<libc::sa_family_t>() + b.len() + 1) as libc::socklen_t,
    ))
}

fn send(fd: RawFd, data: &[u8], fds: &[RawFd]) -> io::Result<()> {
    let mut iov = libc::iovec {
        iov_base: data.as_ptr() as *mut libc::c_void,
        iov_len: data.len(),
    };
    let space = unsafe { libc::CMSG_SPACE(std::mem::size_of_val(fds) as u32) } as usize;
    let mut cbuf = vec![0u8; space.max(1)];
    let mut msg: libc::msghdr = unsafe { zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    if !fds.is_empty() {
        msg.msg_control = cbuf.as_mut_ptr().cast();
        msg.msg_controllen = space;
        unsafe {
            let cm = libc::CMSG_FIRSTHDR(&msg);
            (*cm).cmsg_level = libc::SOL_SOCKET;
            (*cm).cmsg_type = libc::SCM_RIGHTS;
            (*cm).cmsg_len = libc::CMSG_LEN(std::mem::size_of_val(fds) as u32) as usize;
            std::ptr::copy_nonoverlapping(
                fds.as_ptr(),
                libc::CMSG_DATA(cm).cast::<RawFd>(),
                fds.len(),
            );
        }
    }
    if unsafe { libc::sendmsg(fd, &msg, libc::MSG_NOSIGNAL) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// One message and the fds it carried; an empty message is EOF.
fn recv(fd: RawFd) -> io::Result<(Vec<u8>, Vec<OwnedFd>)> {
    let mut buf = vec![0u8; MAX_MSG];
    let mut iov = libc::iovec {
        iov_base: buf.as_mut_ptr().cast(),
        iov_len: buf.len(),
    };
    let space = unsafe { libc::CMSG_SPACE(RawFd::BITS) } as usize;
    let mut cbuf = vec![0u8; space];
    let mut msg: libc::msghdr = unsafe { zeroed() };
    msg.msg_iov = &mut iov;
    msg.msg_iovlen = 1;
    msg.msg_control = cbuf.as_mut_ptr().cast();
    msg.msg_controllen = space;
    let n = loop {
        let n = unsafe { libc::recvmsg(fd, &mut msg, libc::MSG_CMSG_CLOEXEC) };
        if n >= 0 {
            break n as usize;
        }
        let e = io::Error::last_os_error();
        if e.kind() != io::ErrorKind::Interrupted {
            return Err(e);
        }
    };
    let mut fds = vec![];
    unsafe {
        let mut cm = libc::CMSG_FIRSTHDR(&msg);
        while !cm.is_null() {
            if (*cm).cmsg_level == libc::SOL_SOCKET && (*cm).cmsg_type == libc::SCM_RIGHTS {
                let len = (*cm).cmsg_len as usize - libc::CMSG_LEN(0) as usize;
                let p = libc::CMSG_DATA(cm).cast::<RawFd>();
                for i in 0..len / size_of::<RawFd>() {
                    fds.push(OwnedFd::from_raw_fd(*p.add(i)));
                }
            }
            cm = libc::CMSG_NXTHDR(&msg, cm);
        }
    }
    buf.truncate(n);
    Ok((buf, fds))
}

/// Sends a request to the session at `path`. `None` when no session listens.
fn request(path: &Path, words: &[&[u8]], fds: &[RawFd]) -> Result<Option<OwnedFd>> {
    let s = socket()?;
    let (a, len) = addr(path)?;
    if unsafe { libc::connect(s.as_raw_fd(), (&a as *const libc::sockaddr_un).cast(), len) } < 0 {
        return Ok(None);
    }
    let msg = words.join(&b'\0');
    if msg.len() > MAX_MSG {
        bail!("request too long");
    }
    send(s.as_raw_fd(), &msg, fds).context("send to the session")?;
    Ok(Some(s))
}

/// The session's `status` text, if one listens at `path`.
pub fn status(path: &Path) -> Option<String> {
    let s = request(path, &[b"status"], &[]).ok()??;
    let (m, _) = recv(s.as_raw_fd()).ok()?;
    (!m.is_empty()).then(|| String::from_utf8_lossy(&m).into_owned())
}

/// Stops the session at `path` and waits until it is gone; false if none
/// listens.
pub fn stop(path: &Path) -> Result<bool> {
    let Some(s) = request(path, &[b"stop"], &[])? else {
        return Ok(false);
    };
    while !recv(s.as_raw_fd())?.0.is_empty() {}
    Ok(true)
}

/// Runs argv through the session at `path` with this process's stdio;
/// returns its exit code.
pub fn exec(path: &Path, argv: &[&OsStr]) -> Result<i32> {
    let mut words: Vec<&[u8]> = vec![b"exec"];
    words.extend(argv.iter().map(|a| a.as_bytes()));
    let s = request(path, &words, &[0, 1, 2])?.context("not running")?;
    let (m, _) = recv(s.as_raw_fd())?;
    String::from_utf8_lossy(&m)
        .trim()
        .parse()
        .context("the session ended during exec")
}

/// What the session serves.
pub struct Served {
    /// The answer to `status`.
    pub status: String,
    /// The lock a running exec holds shared ([`crate::lifecycle`]); none
    /// for a waiter, which runs no commands.
    pub exec_lock: Option<std::path::PathBuf>,
    /// Ends the session.
    pub stop: Box<dyn Fn() + Send + Sync>,
}

/// Listens on `path` and serves each connection on a thread of its own.
pub fn serve(path: &Path, served: Served) -> Result<()> {
    let _ = fs::remove_file(path);
    let s = socket()?;
    let (a, len) = addr(path)?;
    if unsafe { libc::bind(s.as_raw_fd(), (&a as *const libc::sockaddr_un).cast(), len) } < 0 {
        return Err(io::Error::last_os_error()).with_context(|| format!("bind {}", path.display()));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    if unsafe { libc::listen(s.as_raw_fd(), 16) } < 0 {
        return Err(io::Error::last_os_error()).context("listen");
    }
    let served = std::sync::Arc::new(served);
    thread::spawn(move || {
        loop {
            let c = unsafe {
                libc::accept4(
                    s.as_raw_fd(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    libc::SOCK_CLOEXEC,
                )
            };
            if c < 0 {
                continue;
            }
            let c = unsafe { OwnedFd::from_raw_fd(c) };
            let served = served.clone();
            thread::spawn(move || {
                let _ = handle(&c, &served);
            });
        }
    });
    Ok(())
}

fn handle(c: &OwnedFd, served: &Served) -> Result<()> {
    let (m, fds) = recv(c.as_raw_fd())?;
    let mut words = m.split(|&b| b == 0);
    match words.next() {
        Some(b"status") => send(c.as_raw_fd(), served.status.as_bytes(), &[])?,
        Some(b"stop") => {
            (served.stop)();
            send(c.as_raw_fd(), b"ok", &[])?;
            // The connection closes when this process exits.
            loop {
                thread::park();
            }
        }
        Some(b"exec") => {
            let argv: Vec<&OsStr> = words.map(OsStr::from_bytes).collect();
            let Some(lock) = &served.exec_lock else {
                send(c.as_raw_fd(), b"127", &[])?;
                return Ok(());
            };
            let _running = crate::lifecycle::hold_exec_lock(lock);
            let code = run_exec(c, &argv, fds).unwrap_or(127);
            send(c.as_raw_fd(), code.to_string().as_bytes(), &[])?;
        }
        _ => {}
    }
    Ok(())
}

/// Starts the exec relay with the client's stdio and waits for it, killing it
/// if the client goes away.
fn run_exec(c: &OwnedFd, argv: &[&OsStr], fds: Vec<OwnedFd>) -> Result<i32> {
    let mut fds = fds.into_iter();
    let (Some(i), Some(o), Some(e)) = (fds.next(), fds.next(), fds.next()) else {
        bail!("exec without stdio");
    };
    if argv.is_empty() {
        bail!("exec without a command");
    }
    let mut cmd = Command::new(exec::RELAY);
    cmd.args(argv)
        .env_clear()
        .envs(exec::android_env())
        .current_dir("/")
        .stdin(Stdio::from(i))
        .stdout(Stdio::from(o))
        .stderr(Stdio::from(e));
    let mut child = cmd.spawn().context("start the exec relay")?;
    let pidfd = sys::pidfd(child.id())?;
    loop {
        let r = sys::poll(
            &[
                (pidfd.as_raw_fd(), libc::POLLIN),
                (c.as_raw_fd(), libc::POLLIN | libc::POLLRDHUP),
            ],
            -1,
        )?;
        if r[0] != 0 {
            break;
        }
        if r[1] != 0 {
            let _ = child.kill();
            break;
        }
    }
    let st = child.wait()?;
    use std::os::unix::process::ExitStatusExt;
    Ok(st.code().unwrap_or_else(|| 128 + st.signal().unwrap_or(0)))
}
