//! Host mode: the session is a transient systemd user unit holding the log
//! reader, the bridge, pasta and the outer namespaces, so it outlives the
//! caller and `stop` can take it all down (docs/session.md, "Boot").

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::conf::Config;
use crate::session::{self, append};
use crate::{nsroot, sys};

fn systemctl(args: &[&str]) -> Command {
    let mut c = Command::new("systemctl");
    c.arg("--user").args(args).stdin(Stdio::null());
    c
}

fn unit_active(unit: &str) -> bool {
    systemctl(&["-q", "is-active", unit])
        .status()
        .is_ok_and(|s| s.success())
}

/// Starts the unit, waits for Android init, and with `follow` shows its log
/// until it ends.
pub fn start(cfg: &Config, exe: &Path, follow: bool) -> Result<()> {
    let p = &cfg.paths;
    // Delegate=yes gives the session a cgroup subtree of its own.
    let st = Command::new("systemd-run")
        .args(["--user", "--quiet", "--collect"])
        .arg(format!("--unit={}", p.unit))
        .args(["-p", "Delegate=yes", "-p", "KillMode=control-group"])
        .args([
            "-p",
            "MemoryHigh=6G",
            "-p",
            "MemoryMax=8G",
            "-p",
            "MemorySwapMax=0",
        ])
        .arg("--")
        .arg(exe)
        .arg("_run")
        .arg(p.conf())
        .stdin(Stdio::null())
        .status()
        .context("run systemd-run")?;
    if !st.success() {
        bail!("systemd-run failed ({st})");
    }
    // _run records the pids once Android init is up.
    for _ in 0..100 {
        if fs::metadata(p.pid_file()).is_ok_and(|m| m.len() > 0) || !unit_active(&p.unit) {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    let Some(pid) = session::ns_pid(p) else {
        eprint!(
            "{}",
            fs::read_to_string(p.session_log()).unwrap_or_default()
        );
        bail!("session failed to start");
    };
    println!(
        "session started ({}, uid mode {}): Android pid 1 = host pid {pid}; log: {}",
        cfg.image.name,
        cfg.uids.mode.name(),
        p.kmsg_log().display()
    );
    if follow {
        session::tail(&p.kmsg_log(), &|| sys::alive(pid))?;
    }
    Ok(())
}

/// The unit's main process (`_run`).
pub fn run_unit(cfg: &Config, exe: &Path) -> Result<()> {
    let p = &cfg.paths;
    session::start_kmsg_reader(p)?;
    // start shows session.log when the session fails to come up.
    if let Err(e) = session::start_bridge(cfg) {
        session::log(p, &format!("{e:#}"));
        return Err(e);
    }
    let log = append(&p.session_log())?;
    // Outer namespaces: user, mount, pid, cgroup, made by a fresh
    // single-threaded process. pivot-exec then nests
    // user/mount/pid/cgroup/net/ipc/uts namespaces for Android itself.
    let mut outer = Command::new(exe)
        .arg("_outer")
        .arg(p.conf())
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?)
        .spawn()
        .context("start the outer namespaces")?;
    // Its child is the namespaces' pid 1 (not the newuidmap helper), whose
    // child, pivot-exec's, becomes init.
    let me = outer.id() as i32;
    let mut found = None;
    for _ in 0..200 {
        found = sys::children(me, &[]).into_iter().find_map(|n| {
            let init = sys::children(n, &["init", "sh"]).first().copied()?;
            Some((n, init))
        });
        if found.is_some() || outer.try_wait()?.is_some() {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    let mut pasta = None;
    if let Some((n, i)) = found {
        session::write_pids(p, n, i)?;
        pasta = Some(start_pasta(i, &log)?);
    }
    outer.wait()?;
    if let Some(mut pasta) = pasta {
        let _ = pasta.kill();
        let _ = pasta.wait();
    }
    Ok(())
}

/// User-mode networking for Android's own network namespace (init's). No
/// inbound port forwarding, and `--no-map-gw` keeps the host's loopback
/// services unreachable. Android's EthernetService brings eth1 up (DHCP from
/// pasta) as the default network, with default capabilities since the
/// image's connectivity overlay is blanked (android/overlay/README.md). DHCP
/// hands out 169.254.1.1 as the resolver, which pasta forwards to the host's
/// first IPv4 nameserver (with -D, pasta no longer picks it by itself).
fn start_pasta(init: i32, log: &fs::File) -> Result<std::process::Child> {
    let mut c = Command::new("pasta");
    c.args([
        "--config-net",
        "--quiet",
        "--foreground",
        "--no-netns-quit",
        "--no-map-gw",
    ])
    .args([
        "-I",
        "eth1",
        "-D",
        "169.254.1.1",
        "--dhcp-dns",
        "--dns-forward",
        "169.254.1.1",
    ]);
    if let Some(dns) = host_nameservers().into_iter().find(|n| !n.contains(':')) {
        c.args(["--dns-host", &dns]);
    }
    c.args(["-t", "none", "-u", "none", "-T", "none", "-U", "none"])
        .arg("--userns")
        .arg(format!("/proc/{init}/ns/user"))
        .arg("--netns")
        .arg(format!("/proc/{init}/ns/net"))
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log.try_clone()?);
    c.spawn().context("start pasta")
}

/// `_outer`: makes the outer namespaces, then forks their pid 1, which builds
/// the root and becomes pivot-exec ([`nsroot::run`]). Exits with its status.
pub fn outer(cfg: &Config) -> Result<i32> {
    cfg.uids.enter_userns()?;
    sys::unshare(libc::CLONE_NEWNS | libc::CLONE_NEWPID | libc::CLONE_NEWCGROUP)?;
    let child = sys::fork()?;
    if child == 0 {
        // The whole session ends with this process.
        sys::die_with_parent(libc::SIGKILL);
        if let Err(e) = nsroot::run(cfg) {
            eprintln!("ns-init: {e:#}");
        }
        unsafe { libc::_exit(1) };
    }
    Ok(sys::wait(child)?)
}

/// Kills the session and removes its unit.
pub fn stop(cfg_paths: &crate::paths::Paths) -> Result<()> {
    let p = cfg_paths;
    for pid in [session::outer_pid(p), session::ns_pid(p)]
        .into_iter()
        .flatten()
    {
        sys::kill(pid, libc::SIGKILL);
    }
    let quiet = |args: &[&str]| {
        let _ = systemctl(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    };
    quiet(&["stop", &p.unit]);
    quiet(&["reset-failed", &p.unit]);
    for _ in 0..50 {
        let o = systemctl(&["show", "-P", "LoadState", &p.unit])
            .stderr(Stdio::null())
            .output();
        if o.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == "not-found") {
            break;
        }
        sleep(Duration::from_millis(100));
    }
    session::remove_pids(p);
    Ok(())
}

/// The session unit's cgroup dir, from the outer pid 1's `/proc/<pid>/cgroup`.
pub fn cgroup(p: &crate::paths::Paths) -> Option<std::path::PathBuf> {
    let pid = session::outer_pid(p)?;
    let cg = fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    let path = cg.lines().find_map(|l| l.strip_prefix("0::"))?;
    Some(Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/')))
}

/// The host's resolvers, from /etc/resolv.conf.
pub fn host_nameservers() -> Vec<String> {
    fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut w = l.split_whitespace();
            (w.next() == Some("nameserver")).then(|| w.next().map(str::to_string))?
        })
        .collect()
}
