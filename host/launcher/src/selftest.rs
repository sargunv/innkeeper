//! `selftest`: host-safety probes against a live session; each must report
//! `ok` (docs/session.md, "Host safety"). `mise run test:boot` runs it.

use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::conf::Config;
use crate::paths::Sandbox;
use crate::{session, sys};

struct Report(String);

impl Report {
    fn check(&mut self, ok: bool, what: &str, fail: &str) {
        if ok {
            self.0 += &format!("ok   {what}\n");
        } else {
            self.0 += &format!("FAIL {fail}\n");
        }
    }
}

/// Probes run inside Android, each a shell condition that must fail.
const HOST_PROBES: &[(&str, &str)] = &[
    (
        "umount of the fusectl mask refused",
        "umount /sys/fs/fuse/connections",
    ),
    (
        "lazy umount of a /sys mask refused",
        "umount -l /sys/firmware/efi/efivars",
    ),
    ("remount /sys rw refused", "mount -o remount,bind,rw /sys"),
    ("sysfs not writable", "echo 1 > /sys/kernel/mm/ksm/run"),
    (
        "no FUSE connections visible",
        "[ -n \"$(ls -A /sys/fs/fuse/connections)\" ]",
    ),
    (
        "fresh sysfs mount refused",
        "mkdir -p /tmp/.st && mount -t sysfs s /tmp/.st",
    ),
    ("sysrq refused", "echo h > /proc/sysrq-trigger"),
    (
        "umount of the staged APEX tree refused",
        "umount /linuxapk/apex",
    ),
];

/// Flatpak mode's: the sandbox is the boundary, so these check what it gives
/// an Android process (the probe's own shell, under both seccomp filters).
const FLATPAK_PROBES: &[(&str, &str)] = &[
    ("sysfs not writable", "echo 1 > /sys/kernel/mm/ksm/run"),
    (
        "no FUSE connections visible",
        "[ -n \"$(ls -A /sys/fs/fuse/connections 2>/dev/null)\" ]",
    ),
    (
        "fresh sysfs mount refused",
        "mkdir -p /data/local/tmp/.st && mount -t sysfs s /data/local/tmp/.st; [ -e /data/local/tmp/.st/kernel ]",
    ),
    ("sysrq refused", "echo h > /proc/sysrq-trigger"),
    (
        "no capabilities",
        "grep -q '^CapEff:.*[1-9a-f]' /proc/self/status",
    ),
    (
        "no new privileges",
        "grep -q '^NoNewPrivs:[[:space:]]*0' /proc/self/status",
    ),
    (
        "under Flatpak's seccomp filter and ours",
        "n=$(sed -n 's/^Seccomp_filters:[[:space:]]*//p' /proc/self/status); [ \"${n:-0}\" -lt 2 ]",
    ),
];

fn android_probes(exe: &Path, probes: &[(&str, &str)]) -> Result<String> {
    let mut script = String::from(
        "t() { if eval \"$2\" >/dev/null 2>&1; then echo \"FAIL $1\"; else echo \"ok   $1\"; fi; }\n",
    );
    let sq = |s: &str| format!("'{}'", s.replace('\'', "'\\''"));
    for (what, cond) in probes {
        script += &format!("t {} {}\n", sq(what), sq(cond));
    }
    let o = Command::new(exe)
        .args(["exec", "/system/bin/sh", "-c", &script])
        .stdin(Stdio::null())
        .output()
        .context("run the probes in the session")?;
    let text = String::from_utf8_lossy(&o.stdout) + String::from_utf8_lossy(&o.stderr);
    Ok(text
        .lines()
        .filter(|l| !l.contains("linker"))
        .map(|l| format!("{l}\n"))
        .collect())
}

/// Every file at or under `paths` (not crossing filesystems) owned by group
/// `gid`: the first one.
fn owned_by_group(paths: &[PathBuf], gid: u32) -> Option<PathBuf> {
    fn walk(p: &Path, dev: u64, gid: u32) -> Option<PathBuf> {
        let m = p.symlink_metadata().ok()?;
        if m.gid() == gid {
            return Some(p.to_path_buf());
        }
        if m.is_dir() && m.dev() == dev {
            for e in fs::read_dir(p).ok()?.flatten() {
                if let Some(f) = walk(&e.path(), dev, gid) {
                    return Some(f);
                }
            }
        }
        None
    }
    paths
        .iter()
        .find_map(|p| walk(p, p.symlink_metadata().ok()?.dev(), gid))
}

fn status_field(pid: i32, key: &str) -> String {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
        .unwrap_or_default()
}

/// Single mode (docs/security.md): without a subgid range the kernel denies
/// setgroups, so the host user's supplementary groups can't be dropped.
/// They must stay unusable instead: setgroups denied (no way to change
/// them), unmapped inside, and no host path bound into the session owned by
/// one of them.
///
/// In Flatpak mode the sandbox's user namespace maps no host group either:
/// they all show as the overflow gid, as does every host file whose group
/// isn't the user's, so bound paths can't be told apart. The sandbox is the
/// boundary there, as for any Flatpak app.
fn single_mode_groups(r: &mut Report, cfg: &Config, pid: i32) {
    let me = sys::gid();
    let overflow: u32 = fs::read_to_string("/proc/sys/kernel/overflowgid")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(65534);
    let flatpak = cfg.flatpak();
    let extra: Vec<u32> = status_field(pid, "Groups:")
        .split_whitespace()
        .filter_map(|g| g.parse().ok())
        .filter(|&g| g != me && !(flatpak && g == overflow))
        .collect();
    let setgroups = fs::read_to_string(format!("/proc/{pid}/setgroups")).unwrap_or_default();
    r.check(
        setgroups.trim() == "deny",
        "single mode: setgroups denied in the session",
        "single mode: setgroups allowed in the session",
    );
    if extra.is_empty() {
        let what = if flatpak {
            "no host supplementary groups mapped in the sandbox"
        } else {
            "no host supplementary groups"
        };
        r.check(true, what, "");
        return;
    }
    let gid_map = fs::read_to_string(format!("/proc/{pid}/gid_map")).unwrap_or_default();
    let mapped = gid_map.lines().any(|l| {
        let f: Vec<u64> = l
            .split_whitespace()
            .filter_map(|w| w.parse().ok())
            .collect();
        f.len() == 3
            && extra
                .iter()
                .any(|&g| (f[0]..f[0] + f[2]).contains(&(g as u64)))
    });
    let list = extra
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" ");
    r.check(
        !mapped,
        &format!("single mode: host supplementary groups ({list}) unmapped"),
        "single mode: a host supplementary group is mapped",
    );
    // The host paths bound or linked into the session: /data, the device
    // nodes, the kmsg FIFO and the bridge's socket dir.
    let p = &cfg.paths;
    let mut paths = vec![p.data.clone(), p.run.join("kmsg"), p.run.join("bridge")];
    for n in [
        "null", "zero", "full", "random", "urandom", "fuse", "udmabuf", "net/tun", "dri",
    ] {
        let d = Path::new("/dev").join(n);
        if d.exists() {
            paths.push(d);
        }
    }
    let bad = extra.iter().find_map(|&g| owned_by_group(&paths, g));
    r.check(
        bad.is_none(),
        "single mode: no bound host path belongs to them",
        &format!(
            "single mode: bound host path owned by a host group: {}",
            bad.unwrap_or_default().display()
        ),
    );
}

/// What the sandbox grants, from /.flatpak-info: no host filesystem or home,
/// no host /dev, no way out through the Flatpak service.
fn flatpak_info(r: &mut Report) {
    let info = fs::read_to_string("/.flatpak-info").unwrap_or_default();
    let mut section = "";
    let mut kv = vec![];
    for l in info.lines() {
        if let Some(s) = l.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            section = s;
        } else if let Some((k, v)) = l.split_once('=') {
            kv.push((section.to_string(), k.to_string(), v.to_string()));
        }
    }
    let get = |s: &str, k: &str| {
        kv.iter()
            .find(|(a, b, _)| a == s && b == k)
            .map(|t| t.2.clone())
            .unwrap_or_default()
    };
    let list = |s: &str, k: &str| -> Vec<String> {
        get(s, k)
            .split(';')
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .collect()
    };
    let devices = list("Context", "devices");
    r.check(
        !devices.iter().any(|d| d == "all"),
        &format!("sandbox devices: {}", devices.join(",")),
        "the sandbox has --device=all (the host's /dev)",
    );
    let fs_host: Vec<String> = list("Context", "filesystems")
        .into_iter()
        .filter(|f| {
            let f = f
                .trim_end_matches(":ro")
                .trim_end_matches(":rw")
                .trim_end_matches(":create");
            matches!(f, "host" | "host-os" | "host-etc" | "home" | "~" | "/")
        })
        .collect();
    r.check(
        fs_host.is_empty(),
        "no host filesystem or home in the sandbox",
        &format!("the sandbox has host filesystems: {}", fs_host.join(",")),
    );
    r.check(
        get("Session Bus Policy", "org.freedesktop.Flatpak").is_empty(),
        "no access to org.freedesktop.Flatpak (flatpak-spawn --host)",
        "the sandbox may talk to org.freedesktop.Flatpak",
    );
    // What it shows: the home dir holds only the app's own dir, and /dev no
    // block devices or input devices.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let seen: Vec<String> = fs::read_dir(&home)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".var")
        .collect();
    r.check(
        seen.is_empty(),
        "no host $HOME beyond the app's own dir",
        &format!("host home entries visible: {}", seen.join(" ")),
    );
    let block: Vec<String> = fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_block_device()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    r.check(
        block.is_empty() && !Path::new("/dev/input").exists(),
        "no host block or input devices",
        &format!("host devices visible: {} /dev/input", block.join(" ")),
    );
}

/// Runs the probes; true if all pass. Writes `selftest.txt` into the
/// runtime dir.
pub fn run(cfg: &Config, exe: &Path) -> Result<bool> {
    let p = &cfg.paths;
    let mut r = Report(String::new());
    if p.sandbox == Sandbox::Flatpak {
        flatpak_info(&mut r);
        single_mode_groups(&mut r, cfg, sys::pid());
        r.0 += &android_probes(exe, FLATPAK_PROBES)?;
    } else {
        let pid = session::ns_pid(p).context("not running")?;
        let ns = |n: &str, of: &str| fs::read_link(format!("/proc/{of}/ns/{n}")).ok();
        let (pid_s, me) = (pid.to_string(), "self");
        r.check(
            ns("net", &pid_s) != ns("net", me),
            "own network namespace",
            "shares the host network namespace",
        );
        r.check(
            ns("ipc", &pid_s) != ns("ipc", me),
            "own IPC namespace",
            "shares the host IPC namespace",
        );
        let cg = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap_or_default();
        r.check(
            cg.contains(&format!("/{}.service", p.unit)),
            &format!("runs in {}.service's cgroup", p.unit),
            &format!("cgroup: {}", cg.trim()),
        );
        if cfg.single() {
            single_mode_groups(&mut r, cfg, pid);
        } else {
            let g = status_field(pid, "Groups:");
            r.check(
                g.is_empty(),
                "no host supplementary groups",
                &format!("Groups: {g}"),
            );
        }
        r.0 += &android_probes(exe, HOST_PROBES)?;
    }
    print!("{}", r.0);
    let _ = fs::write(p.run.join("selftest.txt"), &r.0);
    Ok(!r.0.lines().any(|l| l.starts_with("FAIL")) && r.0.lines().any(|l| l.starts_with("ok")))
}
