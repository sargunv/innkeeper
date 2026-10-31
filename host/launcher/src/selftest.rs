//! `selftest`: host-safety probes against a live session; each must report
//! `ok` (docs/session.md, "Host safety"). `mise run test:e2e -- boot` runs it.

use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};

use crate::conf::Config;
use crate::sys;

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

/// The sandbox is the boundary, so these check what it gives an Android
/// process (the probe's own shell, under both seccomp filters).
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

fn status_field(pid: i32, key: &str) -> String {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix(key).map(|v| v.trim().to_string()))
        .unwrap_or_default()
}

/// No host supplementary group is mapped into the sandbox.
fn single_mode_groups(r: &mut Report, pid: i32) {
    let me = sys::gid();
    let overflow: u32 = fs::read_to_string("/proc/sys/kernel/overflowgid")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(65534);
    let extra: Vec<u32> = status_field(pid, "Groups:")
        .split_whitespace()
        .filter_map(|g| g.parse().ok())
        .filter(|&g| g != me && g != overflow)
        .collect();
    let setgroups = fs::read_to_string(format!("/proc/{pid}/setgroups")).unwrap_or_default();
    r.check(
        setgroups.trim() == "deny",
        "single mode: setgroups denied in the session",
        "single mode: setgroups allowed in the session",
    );
    r.check(
        extra.is_empty(),
        "no host supplementary groups mapped in the sandbox",
        "a host supplementary group is mapped",
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
    // What it shows: the home dir holds only the app's own dir and the
    // user's adb public key (read-only, for adb into a profile), and /dev no
    // block devices. Input access is an accepted desktop-parity grant.
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let seen: Vec<String> = fs::read_dir(&home)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n != ".var")
        .filter(|n| n != ".android" || !only_adb_key(&home.join(n)))
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
        block.is_empty(),
        "no host block devices",
        &format!("host block devices visible: {}", block.join(" ")),
    );
}

/// Runs the probes; true if all pass. Writes `selftest.txt` into the
/// runtime dir.
pub fn run(cfg: &Config, exe: &Path) -> Result<bool> {
    let p = &cfg.paths;
    let mut r = Report(String::new());
    flatpak_info(&mut r);
    single_mode_groups(&mut r, sys::pid());
    r.0 += &android_probes(exe, FLATPAK_PROBES)?;
    print!("{}", r.0);
    let _ = fs::write(p.run.join("selftest.txt"), &r.0);
    Ok(!r.0.lines().any(|l| l.starts_with("FAIL")) && r.0.lines().any(|l| l.starts_with("ok")))
}

/// Whether `dir` shows nothing but the adb public key.
fn only_adb_key(dir: &std::path::Path) -> bool {
    let names: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    names.iter().all(|n| n == "adbkey.pub")
}
