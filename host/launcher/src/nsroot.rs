//! Host mode's Android root, assembled by pid 1 of the session's outer user,
//! mount, pid and cgroup namespaces on a tmpfs: read-only overlays of each
//! partition's layers, `/dev`, `/proc`, masked `/sys`, then pivot-exec
//! pivots into it and starts init in nested, locked namespaces
//! (supervisor/pivot-exec/pivot-exec.c, docs/session.md).

use std::fs;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::conf::Config;
use crate::sys::{self, MS_NODEV, MS_NOEXEC, MS_NOSUID, MS_PRIVATE, MS_REC};
use crate::{init, root};

fn tmpfs(dst: &Path, flags: libc::c_ulong, opts: &str, name: &str) -> Result<()> {
    fs::create_dir_all(dst)?;
    sys::mount(name, dst, Some("tmpfs"), flags, Some(opts))
}

/// An overlay of a lowerdir path: `\`, `:` and `,` escaped.
fn lowerdir_escape(p: &Path) -> String {
    let mut s = String::new();
    for c in p.to_string_lossy().chars() {
        if matches!(c, '\\' | ':' | ',') {
            s.push('\\');
        }
        s.push(c);
    }
    s
}

/// Mounts a read-only overlay of the existing dirs among `layers` (topmost
/// first) on `dst`, or a read-only bind if only one exists.
fn layer_mount(dst: &Path, layers: &[PathBuf]) -> Result<()> {
    let have: Vec<&PathBuf> = layers.iter().filter(|l| l.is_dir()).collect();
    fs::create_dir_all(dst)?;
    match have.as_slice() {
        [] => bail!("no layer for {}", dst.display()),
        [one] => {
            sys::bind(one, dst, false)?;
            sys::remount_ro(dst, 0)
        }
        many => {
            let lower: Vec<String> = many.iter().map(|l| lowerdir_escape(l)).collect();
            let opts = format!("ro,lowerdir={}", lower.join(":"));
            sys::mount("overlay", dst, Some("overlay"), 0, Some(&opts))
        }
    }
}

/// The host's mounts at or under `top`, deepest first (from mountinfo, whose
/// paths escape space, tab, newline and backslash as octal).
fn submounts(top: &Path) -> Result<Vec<PathBuf>> {
    let info = fs::read_to_string("/proc/self/mountinfo")?;
    let top = top.to_string_lossy().into_owned();
    let mut v: Vec<String> = info
        .lines()
        .filter_map(|l| l.split(' ').nth(4))
        .map(unescape_mountinfo)
        .filter(|m| m.starts_with(&format!("{top}/")))
        .collect();
    v.sort();
    v.dedup();
    v.reverse();
    Ok(v.into_iter().map(PathBuf::from).collect())
}

fn unescape_mountinfo(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 3 < b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Creates an empty file at `dst` and binds `src` over it.
fn bind_file(src: &Path, dst: &Path) -> Result<()> {
    fs::write(dst, b"")?;
    sys::bind(src, dst, false)
}

/// Assembles the root and execs pivot-exec. Runs as pid 1 of the outer
/// namespaces; returns only on error.
pub fn run(cfg: &Config) -> Result<()> {
    // Never run Android init outside a fresh pid and user namespace.
    if sys::pid() != 1 {
        bail!(
            "refusing to continue: not pid 1 in a new pid namespace (pid {})",
            sys::pid()
        );
    }
    let map = fs::read_to_string("/proc/self/uid_map")?;
    if map.split_whitespace().nth(1) == Some("0") {
        bail!("refusing to continue: in the initial user namespace");
    }
    sys::mount("none", Path::new("/"), None, MS_REC | MS_PRIVATE, None)?;

    let p = &cfg.paths;
    let img = &cfg.image.img;
    let run = &p.run;
    let r = run.join("root");
    tmpfs(&r, 0, "mode=0755", "rootfs")?;

    // Each partition's stack, top first: the start-time layer, the addons,
    // our Android tree, the layers derived from the image, the image.
    let addons = p.addon_layers();
    for (part, src) in root::PARTITIONS
        .iter()
        .map(|&n| (n, root::image_partition(img, n)))
    {
        let mut layers = vec![];
        if root::LAYERED.contains(&part) {
            layers.push(cfg.run_layer().join(part));
            layers.extend(addons.iter().map(|a| a.join(part)));
            layers.push(p.android().join(part));
            layers.push(cfg.image.derived.join("common").join(part));
            layers.push(cfg.uid_layer().join(part));
        }
        layers.push(src);
        layer_mount(&r.join(part), &layers)?;
    }

    // Top-level entries of the system-as-root image (symlinks,
    // init.environ.rc, empty mount points).
    root::copy_top_level(img, &r, true)?;
    let _ = fs::remove_dir_all(r.join("lost+found"));

    // /apex: init mounts an empty tmpfs over it early in second stage, so the
    // flattened tree is staged at /linuxapk/apex and init.rc binds it over
    // /apex in early-init: one mount of the whole tree (no submounts), so
    // non-recursive views of it stay complete.
    fs::create_dir_all(r.join("apex"))?;
    fs::create_dir_all(r.join("bootstrap-apex"))?;
    layer_mount(
        &r.join("linuxapk/apex"),
        &[
            cfg.run_layer().join("apex"),
            cfg.uid_layer().join("apex"),
            cfg.image.apexdir.clone(),
        ],
    )?;
    root::linuxapk_dir(cfg, &r.join("linuxapk"))?;

    // Writable state. (The image has no cache partition: /cache ->
    // /data/cache.)
    fs::create_dir_all(&p.data)?;
    fs::create_dir_all(r.join("data"))?;
    sys::bind(&p.data, &r.join("data"), false)?;
    for d in [
        "linkerconfig",
        "metadata",
        "mnt",
        "storage",
        "tmp",
        "data_mirror",
    ] {
        tmpfs(&r.join(d), 0, "mode=0755", d)?;
    }

    dev(cfg, &r.join("dev"))?;

    // A clean procfs for this (outer) pid namespace. pivot-exec mounts the
    // nested namespace's own /proc on top (the kernel only allows that while
    // a fully visible procfs exists, so nothing may be mounted over this one)
    // and binds the stand-ins in /linuxapk/proc over it.
    fs::create_dir_all(r.join("proc"))?;
    sys::mount(
        "proc",
        &r.join("proc"),
        Some("proc"),
        MS_NOSUID | MS_NODEV | MS_NOEXEC,
        None,
    )?;

    sys_masks(&r.join("sys"))?;
    let mut mounts = String::new();
    for l in fs::read_to_string("/proc/self/mountinfo")?.lines() {
        let f: Vec<&str> = l.split(' ').collect();
        let sep = f.iter().position(|w| *w == "-").unwrap_or(f.len());
        if let (Some(t), Some(fs)) = (f.get(4), f.get(sep + 1))
            && unescape_mountinfo(t).starts_with(&format!("{}", r.join("sys").display()))
        {
            mounts += &format!(
                "{} {} {}\n",
                unescape_mountinfo(t),
                fs,
                f.get(5).unwrap_or(&"")
            );
        }
    }
    fs::write(run.join("sys-mounts.txt"), mounts)?;

    init::exec(cfg, &r, &[])
}

/// /dev: a tmpfs with binds of the device nodes Android uses, the bridge's
/// socket dir, the kmsg FIFO, its own devpts and the userspace binder nodes.
fn dev(cfg: &Config, d: &Path) -> Result<()> {
    tmpfs(d, MS_NOSUID, "mode=0755", "dev")?;
    // udmabuf: dma-bufs from memfds for the gralloc HAL (a logind ACL on the
    // host).
    for n in [
        "null", "zero", "full", "random", "urandom", "fuse", "udmabuf",
    ] {
        let host = Path::new("/dev").join(n);
        if host.exists() {
            bind_file(&host, &d.join(n))?;
        }
    }
    // TUN (VPN apps, Thread networking): interfaces it creates live in the
    // session's own network namespace. init.rc links /dev/net/tun -> ../tun.
    if Path::new("/dev/net/tun").exists() {
        bind_file(Path::new("/dev/net/tun"), &d.join("tun"))?;
    }
    // The bridge's socket dir (it may restart and recreate its socket in
    // there). Group audio (AID 1005, a subordinate gid on the host) can enter
    // it: the audio HALs run as audioserver with that group. In single mode
    // every Android process is uid 0 to the kernel.
    let bridge = cfg.paths.run.join("bridge");
    if cfg.single() {
        fs::set_permissions(&bridge, fs::Permissions::from_mode(0o700))?;
    } else {
        std::os::unix::fs::chown(&bridge, Some(0), Some(1005))?;
        fs::set_permissions(&bridge, fs::Permissions::from_mode(0o750))?;
    }
    fs::create_dir_all(d.join("socket/linuxapk"))?;
    sys::bind(&bridge, &d.join("socket/linuxapk"), false)?;
    // Android init logs to /dev/kmsg: hand it the host-side FIFO instead.
    bind_file(&cfg.paths.run.join("kmsg"), &d.join("kmsg"))?;
    // gid 5 (tty) isn't mapped in single mode.
    let pts_gid = if cfg.single() { 0 } else { 5 };
    fs::DirBuilder::new().mode(0o755).create(d.join("pts"))?;
    sys::mount(
        "devpts",
        &d.join("pts"),
        Some("devpts"),
        0,
        Some(&format!(
            "newinstance,ptmxmode=0666,mode=0620,gid={pts_gid}"
        )),
    )?;
    symlink("pts/ptmx", d.join("ptmx"))?;
    if Path::new("/dev/dri").is_dir() {
        fs::create_dir_all(d.join("dri"))?;
        sys::bind(Path::new("/dev/dri"), &d.join("dri"), true)?;
    }
    root::binder_nodes(d)
}

/// /sys: the host's, read-only, with every submount masked.
///
/// Inside this user namespace Android's root is the host user, so sysfs
/// submounts expose files the user owns: fusectl's per-connection `abort`
/// files belong to the FUSE mount's owner, and vold's
/// AbortFuseConnections() on shutdown once wrote to them, killing the host's
/// AppImage and xdg-document-portal FUSE mounts. A plain bind isn't allowed
/// (the submounts are locked to /sys), so rbind it, cover every submount with
/// an empty read-only tmpfs and make the whole tree read-only.
fn sys_masks(s: &Path) -> Result<()> {
    fs::create_dir_all(s)?;
    sys::bind(Path::new("/sys"), s, true)?;
    for m in submounts(s)? {
        sys::mount(
            "masked",
            &m,
            Some("tmpfs"),
            libc::MS_RDONLY | MS_NOSUID | MS_NODEV | MS_NOEXEC,
            Some("mode=0755"),
        )
        .with_context(|| format!("mask {}", m.display()))?;
    }
    sys::remount_ro(s, MS_NOSUID | MS_NODEV | MS_NOEXEC)?;
    // Hiding /sys/fs/selinux (now an empty tmpfs) makes libselinux report
    // SELinux disabled (supervisor/shim). /sys/fs/cgroup gets a writable
    // tmpfs: init (libprocessgroup) mounts cgroup2 there, rooted at the
    // session's own delegated cgroup in this cgroup namespace.
    sys::mount(
        "nocgroup",
        &s.join("fs/cgroup"),
        Some("tmpfs"),
        MS_NOSUID | MS_NODEV | MS_NOEXEC,
        Some("mode=0755"),
    )?;
    // /sys/fs/bpf: a writable session-private tmpfs for the bpf() emulator's
    // pins (supervisor/pivot-exec/bpfemu.c).
    sys::mount(
        "bpf",
        &s.join("fs/bpf"),
        Some("tmpfs"),
        MS_NOSUID | MS_NODEV | MS_NOEXEC,
        Some("mode=1777"),
    )?;

    // Refuse to boot if any host-user-owned control file is still reachable.
    if fs::read_dir(s.join("fs/fuse/connections")).is_ok_and(|mut d| d.next().is_some()) {
        bail!("refusing to continue: host FUSE connections visible in the session");
    }
    if fs::write(s.join(".linuxapk-rw-probe"), b"").is_ok() {
        bail!("refusing to continue: /sys is writable in the session");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mountinfo_paths() {
        assert_eq!(unescape_mountinfo("/a\\040b\\134c"), "/a b\\c");
        assert_eq!(unescape_mountinfo("/plain"), "/plain");
        assert_eq!(unescape_mountinfo("/end\\04"), "/end\\04");
    }

    #[test]
    fn lowerdirs() {
        assert_eq!(lowerdir_escape(Path::new("/a:b,c\\d")), "/a\\:b\\,c\\\\d");
    }
}
