//! `linuxapk-session`'s commands.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};

use anyhow::{Context, Result, bail};

use crate::conf::Config;
use crate::paths::{Paths, Sandbox};
use crate::uid::{self, Uids};
use crate::{control, exec, flatpak, host, image, layer, requirements, selftest, session, sys};

const USAGE: &str = "\
Linux.apk session launcher: boots the pinned Android image (image/pin.toml) in
rootless namespaces, or inside Linux.apk's Flatpak sandbox (docs/session.md).

  linuxapk-session start [-f]    boot (in the background unless -f),
                                 preparing the image first if needed. Each
                                 app gets its own host Wayland window
    --headless                   (tests only) open no windows
  linuxapk-session prepare       fetch, extract, flatten and derive the image
                                 without booting
  linuxapk-session image         print the selected image and its paths
  linuxapk-session stop          kill the session and clean up
  linuxapk-session status        running or stopped, then uid-mode=MODE
  linuxapk-session reset         delete Android's /data (session stopped)
  linuxapk-session log [-f]      Android init's kernel log (/dev/kmsg)
  linuxapk-session exec CMD...   run CMD inside the live session as root
  linuxapk-session shell         interactive /system/bin/sh inside
  linuxapk-session selftest      host-safety probes against the live session

The uid mode is automatic. \"subuid\": Android's uids are subordinate ids of
the host user, when /etc/subuid and /etc/subgid give it at least 49999 each
and newuidmap is installed. \"single\": otherwise, and always in Flatpak;
every Android uid is the host user and the supervisor virtualizes them. An
existing /data keeps the mode it was made in.

In Flatpak, start -f runs the session in this sandbox, for as long as it
runs; plain start runs it in a sandbox of its own (the Flatpak portal's
Spawn), which the other commands reach through a control socket.

It runs from an installed prefix (libexec/linuxapk; in a checkout, out/stage
after `mise run build`) and builds nothing. Its files:
  $XDG_RUNTIME_DIR/linuxapk[-<instance>]/  session.conf, pid files, the kmsg
    FIFO, the bridge's socket, the start-time layer over the image (in
    Flatpak, under $XDG_RUNTIME_DIR/app/<app id>/)
  $XDG_STATE_HOME/linuxapk[-<instance>]/   logs (session, kmsg, bridge) and
    the documents service's grants
  $XDG_DATA_HOME/linuxapk/                 images/, data[-<instance>]/
    (Android's /data), addons/ (layers over the partitions, e.g. microG),
    root[-<instance>]/ (Flatpak mode's composed root)

Environment:
  LINUXAPK_AUDIO_SINK      PipeWire sink for the session's audio (default:
                           the desktop's default output)
Tests only:
  LINUXAPK_INSTANCE        run an independent, named session (default: the
                           main one). Named sessions are test sessions: they
                           only log links, and also notifications and media
                           players unless they talk to a private bus (a
                           nested test desktop).
  LINUXAPK_OPEN_URLS=1     a test session opens links in the host browser
  LINUXAPK_DATA            Android's /data (default
                           $XDG_DATA_HOME/linuxapk/data[-<instance>])
  LINUXAPK_IMAGES, LINUXAPK_PIN  the image cache and the pin
  LINUXAPK_SUBUID_FILE, LINUXAPK_SUBGID_FILE  stand-ins for /etc/subuid and
                           /etc/subgid when choosing the uid mode
Debugging:
  LINUXAPK_BRIDGE_VERBOSE=1  per-stream stats in bridge.log
  LINUXAPK_BROKER_DEBUG=1    broker tracing, and single-mode syscall counters
                             every 10 s, in session.log; another value is a
                             log file path inside the session root
";

/// The oldest kernel the supervisor works on: seccomp ADDFD with SEND (5.14)
/// for binder's injected fds, openat2 (5.6).
const KERNEL_FLOOR: (u32, u32) = (5, 14);
/// Flatpak mode's: unprivileged PSI triggers (6.5), without which lmkd exits
/// and every ActivityManager write to it stalls 3 s.
const FLATPAK_KERNEL_FLOOR: (u32, u32) = (6, 5);

pub fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    match run(&args) {
        Ok(code) => exit(code),
        Err(e) => {
            eprintln!("linuxapk-session: {e:#}");
            exit(1);
        }
    }
}

fn exe() -> Result<PathBuf> {
    env::current_exe().context("can't find this program's path")
}

fn run(args: &[OsString]) -> Result<i32> {
    let cmd = args.first().and_then(|a| a.to_str()).unwrap_or("");
    let rest = args.get(1..).unwrap_or_default();
    let words: Vec<&str> = rest.iter().filter_map(|a| a.to_str()).collect();
    match cmd {
        "-h" | "--help" | "help" => {
            print!("{USAGE}");
            return Ok(0);
        }
        // Internal: the processes that run a session.
        "_run" => {
            let cfg = Config::load(Path::new(rest.first().context("_run CONF")?))?;
            host::run_unit(&cfg, &exe()?)?;
            return Ok(0);
        }
        "_outer" => {
            let cfg = Config::load(Path::new(rest.first().context("_outer CONF")?))?;
            return host::outer(&cfg);
        }
        "_userns" => {
            let uids: Uids = toml::from_str(words.first().context("_userns UIDS CMD...")?)?;
            let argv = rest
                .get(1..)
                .filter(|a| !a.is_empty())
                .context("_userns UIDS CMD...")?;
            uids.enter_userns()?;
            let e = Command::new(&argv[0]).args(&argv[1..]).exec();
            bail!("exec {:?}: {e}", argv[0]);
        }
        _ => {}
    }
    let paths = Paths::of_this_exe()?;
    match cmd {
        "start" => start(&paths, &words, false).map(|_| 0),
        "_serve" => {
            let r = start(&paths, &words, true);
            if let Err(e) = &r {
                session::log(&paths, &format!("{e:#}"));
            }
            r.map(|_| 0)
        }
        "prepare" => {
            if paths.sandbox == Sandbox::Host {
                requirements::check(false)?;
            }
            let uids = uid::setup(&paths.data, paths.sandbox)?;
            image::prepare(&paths, &paths.image()?, &uids, &exe()?)?;
            Ok(0)
        }
        "image" => {
            let i = paths.image()?;
            println!(
                "image={}\nimg={}\napex={}",
                i.name,
                i.img.display(),
                i.apexdir.display()
            );
            Ok(0)
        }
        "stop" => {
            if paths.sandbox == Sandbox::Flatpak {
                control::stop(&paths.control())?;
                session::remove_pids(&paths);
            } else {
                host::stop(&paths)?;
            }
            println!("session stopped");
            Ok(0)
        }
        "status" => {
            let s = crate::status(&paths);
            print!("{}", s.text());
            Ok(if s.pid.is_some() { 0 } else { 1 })
        }
        "reset" => reset(&paths).map(|_| 0),
        "log" => {
            if words.first() == Some(&"-f") {
                session::tail(&paths.kmsg_log(), &|| true)?;
            } else {
                print!(
                    "{}",
                    fs::read_to_string(paths.kmsg_log()).unwrap_or_default()
                );
            }
            Ok(0)
        }
        "exec" => {
            if rest.is_empty() {
                bail!("usage: exec CMD...");
            }
            let argv: Vec<&OsStr> = rest.iter().map(OsString::as_os_str).collect();
            exec::run(&paths, &argv)
        }
        "shell" => exec::run(&paths, &[OsStr::new("/system/bin/sh"), OsStr::new("-l")]),
        "selftest" => {
            if crate::status(&paths).pid.is_none() {
                bail!("not running");
            }
            let cfg = Config::load(&paths.conf())?;
            Ok(if selftest::run(&cfg, &exe()?)? { 0 } else { 1 })
        }
        _ => {
            eprint!("{USAGE}");
            Ok(1)
        }
    }
}

fn check_kernel(sandbox: Sandbox) -> Result<()> {
    let rel = sys::kernel_release();
    let Some(v) = sys::kernel_version(&rel) else {
        return Ok(());
    };
    if v < KERNEL_FLOOR {
        bail!(
            "Linux {rel} is too old: the session needs {}.{} or later",
            KERNEL_FLOOR.0,
            KERNEL_FLOOR.1
        );
    }
    if sandbox == Sandbox::Flatpak && v < FLATPAK_KERNEL_FLOOR {
        eprintln!(
            "linuxapk-session: Linux {rel} has no unprivileged PSI triggers ({}.{}+): \
             Android's low-memory killer won't run, and app starts stall",
            FLATPAK_KERNEL_FLOOR.0, FLATPAK_KERNEL_FLOOR.1
        );
    }
    Ok(())
}

/// The host compositor's socket.
fn wayland_socket(sandbox: Sandbox) -> Result<PathBuf> {
    let mut wd = env::var("WAYLAND_DISPLAY").unwrap_or_default();
    if wd.is_empty() && sandbox == Sandbox::Host {
        // The user manager's, as a desktop entry's process may lack it.
        let o = Command::new("systemctl")
            .args(["--user", "show-environment"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        wd = o
            .lines()
            .find_map(|l| l.strip_prefix("WAYLAND_DISPLAY="))
            .unwrap_or_default()
            .to_string();
    }
    if wd.is_empty() {
        wd = "wayland-0".into();
    }
    let p = if wd.starts_with('/') {
        PathBuf::from(&wd)
    } else {
        env::var_os("XDG_RUNTIME_DIR")
            .map_or_else(
                || PathBuf::from(format!("/run/user/{}", sys::uid())),
                PathBuf::from,
            )
            .join(&wd)
    };
    if !fs::metadata(&p).is_ok_and(|m| m.file_type().is_socket()) {
        bail!("no Wayland socket at {} (use --headless)", p.display());
    }
    Ok(p)
}

/// `start`, and `_serve` (`serve`): Flatpak mode's session in a sandbox of
/// its own, which `start` spawns.
fn start(paths: &Paths, opts: &[&str], serve: bool) -> Result<()> {
    let (mut follow, mut headless) = (false, false);
    for o in opts {
        match *o {
            "-f" => follow = true,
            "--headless" => headless = true,
            _ => bail!("unknown start option: {o}"),
        }
    }
    if let Some(p) = crate::status(paths).pid {
        bail!("already running (pid {p})");
    }
    check_kernel(paths.sandbox)?;
    if paths.sandbox == Sandbox::Host {
        requirements::check(!headless)?;
    }
    let uids = uid::setup(&paths.data, paths.sandbox)?;
    // Windows go through the bridge's display service, which connects to the
    // host compositor; --headless shows none.
    let wayland = if headless {
        None
    } else {
        Some(wayland_socket(paths.sandbox)?)
    };
    let image = paths.image()?;
    let exe = exe()?;
    image::prepare(paths, &image, &uids, &exe)?;
    let flatpak = paths.sandbox == Sandbox::Flatpak;
    if flatpak && !follow && !serve {
        return flatpak::spawn(paths, &exe, headless);
    }
    session::fresh_dirs(paths)?;
    let cfg = Config::new(paths.clone(), image, uids, headless, wayland);
    layer::write(&cfg)?;
    cfg.save()?;
    if flatpak {
        flatpak::run(&cfg, follow)
    } else {
        host::start(&cfg, &exe, follow)
    }
}

/// Deletes Android's /data, as Android's root: its files belong to Android's
/// uids.
fn reset(paths: &Paths) -> Result<()> {
    if let Some(p) = crate::status(paths).pid {
        bail!("running (pid {p}); stop it first");
    }
    if paths.data.symlink_metadata().is_err() {
        return Ok(());
    }
    if paths.sandbox == Sandbox::Flatpak {
        flatpak::remove_tree(&paths.data)?;
    } else {
        let uids = uid::setup(&paths.data, paths.sandbox)?;
        let argv: [&OsStr; 3] = ["rm".as_ref(), "-rf".as_ref(), paths.data.as_os_str()];
        uid::as_android(&exe()?, &uids, paths.sandbox, &argv)?;
    }
    println!("removed {}", paths.data.display());
    Ok(())
}
