//! Host mode's requirements from the host, checked before `prepare` and
//! `start` so that whatever is missing is named before a download or a boot
//! fails on it (docs/development.md lists them). Fixing any of them needs
//! root, so each says what to ask for and whether Linux.apk's Flatpak, which
//! runs without most of them (docs/session.md, "Flatpak mode"), avoids it.

use std::env;
use std::ffi::{CStr, CString};
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Result, bail};

use crate::sys;

/// The first line of the error, which the manager recognizes.
pub const HEADLINE: &str = "this computer is missing what Android needs here";

/// One missing requirement.
struct Missing {
    what: String,
    /// The Flatpak doesn't need it.
    flatpak: bool,
}

/// Fails with one message listing every missing requirement. Warns (on
/// stderr) about what only costs a feature: with `sound`, a PulseAudio
/// socket.
pub fn check(sound: bool) -> Result<()> {
    let mut miss = vec![];
    let mut need = |what: String, flatpak: bool| miss.push(Missing { what, flatpak });
    if let Err(e) = userns() {
        need(e, true);
    }
    if !crate::host::user_manager("systemctl")
        .arg("show-environment")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
    {
        need(
            "no systemd user session (`systemctl --user`): the session runs as a \
             systemd user service"
                .into(),
            true,
        );
    }
    for (prog, package, why) in [
        ("pasta", "passt", "Android's network goes through it"),
        ("curl", "curl", "it downloads the Android image"),
        ("python3", "python3", "it unpacks the Android image"),
    ] {
        if !on_path(prog) {
            need(
                format!("{prog} isn't installed (package {package}): {why}"),
                true,
            );
        }
    }
    // fetch.sh reads the pin with tomllib (3.11).
    if on_path("python3")
        && !Command::new("python3")
            .args(["-Ic", "import tomllib"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    {
        need(
            "python3 is older than 3.11: unpacking the Android image needs 3.11 or later".into(),
            true,
        );
    }
    // The Flatpak needs /dev/udmabuf too (--device=dri).
    let udmabuf = CString::new("/dev/udmabuf").unwrap();
    if fs::metadata("/dev/udmabuf").is_err() {
        need(
            "/dev/udmabuf doesn't exist: the udmabuf kernel module isn't loaded \
             (`modprobe udmabuf`, as root); Android's graphics buffers come from it"
                .into(),
            false,
        );
    } else if unsafe { libc::access(udmabuf.as_ptr(), libc::R_OK | libc::W_OK) } != 0 {
        need(
            "you can't read and write /dev/udmabuf: systemd-logind grants it to the \
             user logged in at this computer's own screen; a login elsewhere needs a \
             udev rule (as root)"
                .into(),
            false,
        );
    }
    if sound && !has_pulse() {
        eprintln!(
            "linuxapk-session: no PulseAudio socket (pipewire-pulse): Android apps will have no sound"
        );
    }
    if miss.is_empty() {
        return Ok(());
    }
    let mut msg = format!("{HEADLINE}:\n");
    for m in &miss {
        msg.push_str(&format!("- {}\n", m.what));
    }
    msg.push_str("Fixing this needs an administrator (root).");
    if miss.iter().any(|m| m.flatpak) {
        msg.push_str(
            " Without one, use Linux.apk's Flatpak (dev.linuxapk.Manager): it needs \
             no user namespaces, systemd, pasta, curl or Python from this computer.",
        );
    }
    bail!("{msg}");
}

fn on_path(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|p| env::split_paths(&p).any(|d| d.join(name).is_file()))
}

/// Whether there is a PulseAudio server for the bridge, where the
/// `pulseaudio` crate looks for one.
fn has_pulse() -> bool {
    if env::var_os("PULSE_SERVER").is_some_and(|v| !v.is_empty()) {
        return true;
    }
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| format!("/run/user/{}", sys::uid()).into())
        .join("pulse/native")
        .exists()
}

/// Whether an unprivileged process can make a user namespace and mount in
/// it, as the session does: a child tries, and its exit code says how far it
/// got. The manager calls this from a thread, so the child makes only raw
/// system calls.
fn userns() -> Result<(), String> {
    let maps: [(&CStr, CString); 3] = [
        (c"/proc/self/setgroups", c"deny".into()),
        (
            c"/proc/self/uid_map",
            CString::new(format!("0 {} 1", sys::uid())).unwrap(),
        ),
        (
            c"/proc/self/gid_map",
            CString::new(format!("0 {} 1", sys::gid())).unwrap(),
        ),
    ];
    let pid = sys::fork().map_err(|e| format!("{e:#}"))?;
    if pid == 0 {
        // SAFETY: only async-signal-safe calls between fork and _exit.
        let code = unsafe {
            let write = |path: &CStr, text: &CStr| {
                let fd = libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
                let n = text.count_bytes();
                let ok = fd >= 0 && libc::write(fd, text.as_ptr().cast(), n) == n as isize;
                if fd >= 0 {
                    libc::close(fd);
                }
                ok
            };
            if libc::unshare(libc::CLONE_NEWUSER | libc::CLONE_NEWNS) != 0 {
                1
            } else if !maps.iter().all(|(p, t)| write(p, t)) {
                2
            } else if libc::mount(
                c"none".as_ptr(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            ) != 0
                || libc::mount(
                    c"none".as_ptr(),
                    c"/".as_ptr(),
                    c"tmpfs".as_ptr(),
                    0,
                    std::ptr::null(),
                ) != 0
            {
                3
            } else {
                0
            }
        };
        unsafe { libc::_exit(code) };
    }
    let restricted = fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns")
        .is_ok_and(|v| v.trim() == "1");
    match sys::wait(pid).map_err(|e| format!("wait: {e}"))? {
        0 => Ok(()),
        1 => Err("unprivileged user namespaces are turned off (sysctl \
                  user.max_user_namespaces, or kernel.unprivileged_userns_clone)"
            .into()),
        _ if restricted => Err("AppArmor keeps unprivileged user namespaces from mounting \
                                (sysctl kernel.apparmor_restrict_unprivileged_userns=1)"
            .into()),
        _ => Err("a security policy keeps unprivileged user namespaces from mounting".into()),
    }
}
