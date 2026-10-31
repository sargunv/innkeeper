//! `innkeeper-session`'s commands.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::FileTypeExt;
use std::path::{Path, PathBuf};
use std::process::exit;

use anyhow::{Context, Result, bail};

use crate::conf::Config;
use crate::paths::Paths;
use crate::{control, flatpak, image, layer, selftest, session, sys};

const USAGE: &str = "\
Innkeeper session launcher: boots the pinned Android image (image/pin.toml)
inside Innkeeper's Flatpak sandbox (docs/session.md).

  innkeeper-session start [-f]    boot (in the background unless -f),
                                 preparing the image first if needed. Each
                                 app gets its own host Wayland window
    --headless                   (tests only) open no windows
  innkeeper-session prepare       fetch, extract, flatten and derive the image
                                 without booting
  innkeeper-session image         print the selected image and its paths
  innkeeper-session stop          kill the session and clean up
  innkeeper-session status        running or stopped
  innkeeper-session reset         delete Android's /data (session stopped)
  innkeeper-session log [-f]      Android init's kernel log (/dev/kmsg)
  innkeeper-session exec CMD...   run CMD inside the live session as root
  innkeeper-session shell         interactive /system/bin/sh inside
  innkeeper-session selftest      host-safety probes against the live session

Every Android process runs as the host user; the supervisor virtualizes
Android uids. start -f runs the session in this sandbox, for as long as it
runs; plain start runs it in a sandbox of its own (the Flatpak portal's
Spawn), which the other commands reach through a control socket.

It runs from an installed prefix (libexec/innkeeper; in a checkout, out/stage
after `mise run build`) and builds nothing. Its files:
  $XDG_RUNTIME_DIR/app/<app id>/innkeeper-<profile>/  session.conf, pid
    files, the kmsg FIFO, the bridge's socket, the control socket, the
    start-time layer over the image
  $XDG_STATE_HOME/innkeeper-<profile>/   logs (session, kmsg, bridge) and the
    documents service's grants
  $XDG_DATA_HOME/innkeeper/              images/, data-<profile>/ (Android's
    /data), addons/ (layers over the partitions, e.g. microG),
    root-<profile>/ (the composed root)

Environment:
  INNKEEPER_AUDIO_SINK      PipeWire sink for the session's audio (default:
                           the desktop's default output)
  INNKEEPER_IDLE_GRACE      seconds without windows, foreground services,
                           background-allowed apps, a near alarm or a running
                           exec before the session stops itself, leaving a
                           wakeup for its next alarm (default 120; 0: never;
                           headless sessions never unless it is set)
  INNKEEPER_PROFILE         the profile to run (required; `innkeeper` and the
                           manager set it)
Tests only:
  INNKEEPER_TEST=1          isolate clipboard, portals, links, notifications,
                           media players, AT-SPI and location from the desktop
  INNKEEPER_DATA            Android's /data (default
                           $XDG_DATA_HOME/innkeeper/data[-<profile>])
  INNKEEPER_IMAGES, INNKEEPER_PIN  the image cache and the pin
Debugging:
  INNKEEPER_BRIDGE_VERBOSE=1  per-stream stats in bridge.log
  INNKEEPER_BROKER_DEBUG=1    broker tracing, and single-mode syscall counters
                             every 10 s, in session.log; another value is a
                             log file path inside the session root
";

/// The oldest kernel the supervisor works on: seccomp ADDFD with SEND (5.14)
/// for binder's injected fds, openat2 (5.6).
const KERNEL_FLOOR: (u32, u32) = (5, 14);
/// Unprivileged PSI triggers (6.5), without which lmkd exits and every
/// ActivityManager write to it stalls 3 s.
const FLATPAK_KERNEL_FLOOR: (u32, u32) = (6, 5);

pub fn main() {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    match run(&args) {
        Ok(code) => exit(code),
        Err(e) => {
            eprintln!("innkeeper-session: {e:#}");
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
    if matches!(cmd, "-h" | "--help" | "help") {
        print!("{USAGE}");
        return Ok(0);
    }
    if cmd != "image" && !Path::new("/.flatpak-info").exists() {
        bail!(
            "run innkeeper-session inside the Innkeeper Flatpak (in a checkout, use `mise run session`)"
        );
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
            image::prepare(&paths, &paths.image()?)?;
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
            control::stop(&paths.control())?;
            session::remove_pids(&paths);
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
            control::exec(&paths.control(), &argv)
        }
        "shell" => control::exec(
            &paths.control(),
            &[OsStr::new("/system/bin/sh"), OsStr::new("-l")],
        ),
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

fn check_kernel() -> Result<()> {
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
    if v < FLATPAK_KERNEL_FLOOR {
        eprintln!(
            "innkeeper-session: Linux {rel} has no unprivileged PSI triggers ({}.{}+): \
             Android's low-memory killer won't run, and app starts stall",
            FLATPAK_KERNEL_FLOOR.0, FLATPAK_KERNEL_FLOOR.1
        );
    }
    Ok(())
}

/// The host compositor's socket.
fn wayland_socket() -> Result<PathBuf> {
    let mut wd = env::var("WAYLAND_DISPLAY").unwrap_or_default();
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

/// `start`, and `_serve` (`serve`): the session in a sandbox of its own,
/// which `start` spawns.
fn start(paths: &Paths, opts: &[&str], serve: bool) -> Result<()> {
    let trace = session::BootTrace::new();
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
    // A wakeup left for an alarm isn't needed once the session runs.
    if !serve {
        let _ = control::stop(&paths.control());
    }
    check_kernel()?;
    let exe = exe()?;
    // Windows go through the bridge's display service, which connects to the
    // host compositor; --headless shows none.
    let wayland = if headless {
        None
    } else {
        Some(wayland_socket()?)
    };
    let image = paths.image()?;
    let image_lease = image::prepare(paths, &image)?;
    image::check_data(&paths.data, &image)?;
    if !follow && !serve {
        return flatpak::spawn(paths, &exe, headless);
    }
    session::fresh_dirs(paths)?;
    trace.mark(paths, "image prepared, runtime ready");
    let cfg = Config::new(paths.clone(), image, headless, wayland);
    layer::write(&cfg)?;
    trace.mark(paths, "start-time layer written");
    cfg.save()?;
    flatpak::run(&cfg, follow, image_lease, &trace)
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
    flatpak::remove_tree(&paths.data)?;
    println!("removed {}", paths.data.display());
    Ok(())
}
