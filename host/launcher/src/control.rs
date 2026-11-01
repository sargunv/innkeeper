//! The session's control socket server. innkeeper_profile::control has the
//! protocol and the client.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

use anyhow::{Context, Result, bail};
use innkeeper_profile::control::{addr, recv, send, socket};
pub use innkeeper_profile::control::{exec, status, stop};

use crate::exec;
use crate::profile::System;
use crate::sys;

/// How the session runs `exec`'s commands.
pub struct Exec {
    /// The lock that a running exec holds in shared mode. See
    /// [`crate::lifecycle`].
    pub lock: std::path::PathBuf,
    /// The guest system, whose exec relay runs the commands.
    pub system: System,
}

/// What the session serves.
pub struct Served {
    /// The answer to `status`.
    pub status: String,
    /// None for a waiter, which runs no commands.
    pub exec: Option<Exec>,
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
            let Some(exec) = &served.exec else {
                send(c.as_raw_fd(), b"127", &[])?;
                return Ok(());
            };
            let _running = crate::lifecycle::hold_exec_lock(&exec.lock);
            let code = run_exec(c, exec.system, &argv, fds).unwrap_or(127);
            send(c.as_raw_fd(), code.to_string().as_bytes(), &[])?;
        }
        _ => {}
    }
    Ok(())
}

/// Starts the exec relay with the client's stdio and waits for it, killing it
/// if the client goes away.
fn run_exec(c: &OwnedFd, system: System, argv: &[&OsStr], fds: Vec<OwnedFd>) -> Result<i32> {
    let mut fds = fds.into_iter();
    let (Some(i), Some(o), Some(e)) = (fds.next(), fds.next(), fds.next()) else {
        bail!("exec without stdio");
    };
    if argv.is_empty() {
        bail!("exec without a command");
    }
    let mut cmd = Command::new(exec::relay(system));
    cmd.args(argv)
        .env_clear()
        .envs(exec::env(system))
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
