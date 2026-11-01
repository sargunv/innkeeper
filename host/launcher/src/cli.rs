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
use crate::profile::{Settings, System};
use crate::{control, image, layer, sandbox, selftest, session, sys};

const USAGE: &str = "\
innkeeper-session: Innkeeper's internal session launcher for the profile in
INNKEEPER_PROFILE (docs/session.md). Use `innkeeper profile` instead; in a
checkout, `mise run session -- CMD` runs it in the development Flatpak.

  start [-f] [--headless] | _serve | prepare | image | stop | status
  reset | log [-f] | exec CMD... | shell | selftest

Its environment variables are in host/launcher/README.md.
";

/// The oldest kernel the supervisor works on. It needs seccomp ADDFD with SEND
/// (5.14) for binder's injected fds, and openat2 (5.6).
const KERNEL_FLOOR: (u32, u32) = (5, 14);
/// The kernel that added unprivileged PSI triggers. Without them, lmkd exits
/// and every ActivityManager write to it stalls for 3 s.
const PSI_KERNEL_FLOOR: (u32, u32) = (6, 5);

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
            prepare(&paths, system(&paths)?)?;
            Ok(0)
        }
        "image" => {
            let system = system(&paths)?;
            let i = paths.image_of(system)?;
            println!("image={}\nimg={}", i.name, i.img.display());
            if system == System::Android {
                println!("apex={}", i.apexdir.display());
            }
            Ok(0)
        }
        "stop" => {
            control::stop(&paths.control())?;
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

/// The profile's guest system, from its settings.
fn system(paths: &Paths) -> Result<System> {
    Ok(Settings::load(&paths.data)?.system)
}

/// Fetches and extracts `system`'s image, and for Android also flattens and
/// derives it. The returned lease keeps the image from being pruned.
fn prepare(paths: &Paths, system: System) -> Result<crate::prune::Lease> {
    let image = paths.image_of(system)?;
    match system {
        System::Android => image::prepare(paths, &image),
        System::Ohos => crate::ohos::prepare(paths, &image),
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
    if v < PSI_KERNEL_FLOOR {
        eprintln!(
            "innkeeper-session: Linux {rel} has no unprivileged PSI triggers ({}.{}+): \
             Android's low-memory killer won't run, and app starts stall",
            PSI_KERNEL_FLOOR.0, PSI_KERNEL_FLOOR.1
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

/// Runs `start`, or `_serve` when `serve` is set. Without `-f`, `start`
/// spawns a sandbox that runs the session through `_serve`.
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
    // host compositor. With --headless there are no windows.
    let wayland = if headless {
        None
    } else {
        Some(wayland_socket()?)
    };
    let system = system(paths)?;
    let image = paths.image_of(system)?;
    let image_lease = prepare(paths, system)?;
    if system == System::Android {
        image::check_data(&paths.data, &image)?;
    }
    if !follow && !serve {
        return sandbox::spawn(paths, &exe, headless);
    }
    session::fresh_dirs(paths)?;
    trace.mark(paths, "image prepared, runtime ready");
    let cfg = Config::new(system, paths.clone(), image, headless, wayland);
    if system == System::Android {
        layer::write(&cfg)?;
        trace.mark(paths, "start-time layer written");
    }
    cfg.save()?;
    sandbox::run(&cfg, follow, image_lease, &trace)
}

/// Deletes the guest's `/data`, keeping the profile's settings in it.
fn reset(paths: &Paths) -> Result<()> {
    if let Some(p) = crate::status(paths).pid {
        bail!("running (pid {p}); stop it first");
    }
    crate::profile::reset(&paths.data)?;
    println!("reset {}", paths.data.display());
    Ok(())
}
