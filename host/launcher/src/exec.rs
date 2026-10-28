//! `exec`: a command inside the live session as Android's root, through the
//! exec relay. Binder needs the seccomp filter only init's descendants have,
//! so the relay (a static binary, android/system/exec) hands the command to
//! its server under init, which runs it there.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

use crate::paths::{Paths, Sandbox};
use crate::{control, session, sys};

pub const RELAY: &str = "/system/bin/linuxapk-exec";

/// Android's environment for a command: init.environ.rc's and the derived
/// classpath's `export NAME VALUE` lines, read from the session's root (this
/// process's `/`), plus a shell's PATH and HOME.
pub fn android_env() -> Vec<(String, String)> {
    let mut env = vec![];
    for f in ["/init.environ.rc", "/data/system/environ/classpath"] {
        for l in fs::read_to_string(f).unwrap_or_default().lines() {
            let Some(rest) = l.trim_start().strip_prefix("export ") else {
                continue;
            };
            if let Some((k, v)) = rest.trim_start().split_once(' ') {
                env.push((k.to_string(), v.to_string()));
            }
        }
    }
    env.push((
        "PATH".into(),
        "/product/bin:/apex/com.android.runtime/bin:/apex/com.android.art/bin:/system_ext/bin:\
         /system/bin:/system/xbin:/odm/bin:/vendor/bin:/vendor/xbin"
            .into(),
    ));
    env.push(("HOME".into(), "/data/local/tmp".into()));
    env.push(("TMPDIR".into(), "/data/local/tmp".into()));
    env
}

/// Runs argv in the session; returns its exit code.
pub fn run(paths: &Paths, argv: &[&OsStr]) -> Result<i32> {
    if paths.sandbox == Sandbox::Flatpak {
        return control::exec(&paths.control(), argv);
    }
    let pid = session::ns_pid(paths).context("not running")?;
    // Keeps the session up while the command runs (crate::lifecycle).
    let _running = crate::lifecycle::hold_exec_lock(&paths.exec_lock());
    // Join the session's cgroup, so nothing started here outlives `stop`.
    if let Some(cg) = crate::host::cgroup(paths) {
        let _ = fs::write(cg.join("cgroup.procs"), format!("{}\n", sys::pid()));
    }
    // Android's root and namespaces (init's), user first: it grants the
    // rights to join the others.
    let proc = Path::new("/proc").join(pid.to_string());
    let open = |n: &str| {
        sys::open_path(&proc.join("ns").join(n)).with_context(|| format!("open {n} namespace"))
    };
    let user = open("user")?;
    let others = [
        (open("cgroup")?, libc::CLONE_NEWCGROUP),
        (open("ipc")?, libc::CLONE_NEWIPC),
        (open("uts")?, libc::CLONE_NEWUTS),
        (open("net")?, libc::CLONE_NEWNET),
        (open("pid")?, libc::CLONE_NEWPID),
        (open("mnt")?, libc::CLONE_NEWNS),
    ];
    let root = sys::open_path(&proc.join("root")).context("open the session's root")?;
    sys::setns(&user, libc::CLONE_NEWUSER).context("join the user namespace")?;
    for (fd, t) in &others {
        sys::setns(fd, *t).with_context(|| format!("join namespace {t:#x}"))?;
    }
    sys::fchdir(&root)?;
    sys::chroot_here().context("chroot")?;
    std::env::set_current_dir("/")?;
    // A new pid namespace applies to children only.
    let child = sys::fork()?;
    if child == 0 {
        sys::die_with_parent(libc::SIGKILL);
        let e = Command::new(RELAY)
            .args(argv)
            .env_clear()
            .envs(android_env())
            .exec();
        eprintln!("linuxapk-session: exec {RELAY}: {e}");
        unsafe { libc::_exit(127) };
    }
    Ok(sys::wait(child)?)
}
