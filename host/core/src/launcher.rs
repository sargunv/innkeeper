//! Driving the Android session through `linuxapk-session` (the prefix's, or
//! any launcher for test harnesses). Its state (running, uid mode, memory,
//! the image) comes from the launcher's library; starting, stopping and
//! commands go through the launcher itself. Commands run inside the session
//! as argv through `linuxapk-session exec`, which hands them to the exec
//! relay as argv: no string is ever interpreted by a shell, host or Android.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use linuxapk_session::Paths;

use crate::paths::Prefix;
use crate::proc;

pub struct Session {
    launcher: PathBuf,
}

/// Lines of session stderr worth keeping (the session's linker warns there).
fn clean_stderr(s: &str) -> String {
    s.lines()
        .filter(|l| !l.contains("linker"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// How Android's uids map to host ids: each Android uid one of the user's
/// subordinate ids (apps isolated from each other as on Android), or every
/// Android uid the user (no isolation between apps).
pub use linuxapk_session::UidMode;

/// What [`Session::status`] reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    pub running: bool,
    pub booted: bool,
    /// The session unit's memory use (cgroup `memory.current`), in bytes.
    pub memory: Option<u64>,
    pub uid_mode: Option<UidMode>,
}

impl Session {
    /// The session of this program's prefix and instance.
    pub fn find() -> Result<Self> {
        Ok(Self::at(Prefix::locate()?.launcher()))
    }

    /// The session `launcher` runs (for harnesses outside a prefix).
    pub fn at(launcher: PathBuf) -> Self {
        Self { launcher }
    }

    /// The launcher library's view of this launcher's session.
    fn paths(&self) -> Option<Paths> {
        let prefix = self.launcher.parent()?.parent()?.parent()?;
        Paths::new(prefix).ok()
    }

    fn state(&self) -> Option<linuxapk_session::Status> {
        Some(linuxapk_session::status(&self.paths()?))
    }

    pub fn running(&self) -> bool {
        self.pid().is_some()
    }

    /// Pid of the session's Android init (it changes when the session
    /// restarts).
    pub fn pid(&self) -> Option<u32> {
        self.state()?.pid.map(|p| p as u32)
    }

    /// The running session's uid mode.
    pub fn uid_mode(&self) -> Option<UidMode> {
        let s = self.state()?;
        s.pid.map(|_| s.uid_mode)
    }

    /// The session's cgroup dir (host mode), for memory accounting.
    pub fn cgroup(&self) -> Option<PathBuf> {
        linuxapk_session::cgroup(&self.paths()?)
    }

    /// Runs argv inside the session as root; returns (success, stdout, stderr).
    pub fn exec_with(&self, secs: u64, argv: &[&str], stdin: Stdio) -> Result<proc::Output> {
        let mut c = Command::new(&self.launcher);
        c.arg("exec").args(argv);
        let mut o = proc::run(&mut c, stdin, Duration::from_secs(secs))?;
        o.stderr = clean_stderr(&o.stderr);
        Ok(o)
    }

    pub fn exec(&self, secs: u64, argv: &[&str]) -> Result<proc::Output> {
        self.exec_with(secs, argv, Stdio::null())
    }

    /// Runs argv, failing with its output unless it exits 0.
    pub fn exec_ok(&self, secs: u64, argv: &[&str]) -> Result<String> {
        let o = self.exec(secs, argv)?;
        let text = o.text();
        if !o.ok() {
            bail!(
                "{} failed: {} {}",
                argv.join(" "),
                text.trim(),
                o.stderr.trim()
            );
        }
        Ok(text)
    }

    /// Writes a host file to a fixed path inside the session (`dst` must be a
    /// constant, not derived from input).
    pub fn push_fixed(&self, src: &Path, dst: &'static str) -> Result<()> {
        let f = File::open(src).with_context(|| format!("open {}", src.display()))?;
        // `$1` is the destination; the script itself is a constant.
        let o = self.exec_with(
            120,
            &[
                "sh",
                "-c",
                "cat > \"$1.tmp\" && chmod 0444 \"$1.tmp\" && mv -f \"$1.tmp\" \"$1\"",
                "sh",
                dst,
            ],
            f.into(),
        )?;
        if !o.ok() {
            bail!("copy to {dst} failed: {}", o.stderr.trim());
        }
        Ok(())
    }

    pub fn getprop(&self, name: &str) -> String {
        self.exec(15, &["getprop", name])
            .map(|o| o.text().trim().to_string())
            .unwrap_or_default()
    }

    /// Stops the session (no-op if it isn't running).
    pub fn stop(&self) -> Result<()> {
        let o = proc::run(
            Command::new(&self.launcher).arg("stop"),
            Stdio::null(),
            Duration::from_secs(120),
        )?;
        if !o.ok() {
            bail!("session stop failed: {}", o.stderr.trim());
        }
        Ok(())
    }

    /// Running state, boot state, memory use and uid mode, for status
    /// displays.
    pub fn status(&self) -> Status {
        let Some(paths) = self.paths() else {
            return Status::default();
        };
        let s = linuxapk_session::status(&paths);
        if s.pid.is_none() {
            return Status::default();
        }
        Status {
            running: true,
            booted: self.booted(),
            memory: linuxapk_session::memory(&paths),
            uid_mode: Some(s.uid_mode),
        }
    }

    pub fn booted(&self) -> bool {
        self.getprop("sys.boot_completed") == "1"
    }

    /// Whether the image is fetched, extracted and flattened (so starting
    /// doesn't download it first).
    pub fn image_ready(&self) -> bool {
        self.paths().and_then(|p| p.image().ok()).is_some_and(|i| {
            i.img.join("system/system").is_dir() && i.apexdir.join("apex-info-list.xml").is_file()
        })
    }

    /// Fetches and unpacks the image (about 1.2 GB to download). Its
    /// progress goes to stderr.
    pub fn prepare(&self) -> Result<()> {
        let st = Command::new(&self.launcher)
            .arg("prepare")
            .stdin(Stdio::null())
            .stdout(io::stderr())
            .status()?;
        if !st.success() {
            bail!(
                "preparing the Android image failed ({} prepare)",
                self.launcher.display()
            );
        }
        Ok(())
    }

    /// Starts the session (headless: with no windows, for tests), preparing
    /// the image first if this is the first start. Progress goes to `status`.
    pub fn start(&self, headless: bool, status: &dyn Fn(&str)) -> Result<()> {
        if !self.image_ready() {
            status(PREPARING);
            self.prepare()?;
        }
        status("starting the Android session");
        let mut c = Command::new(&self.launcher);
        c.arg("start").stdin(Stdio::null()).stdout(Stdio::null());
        if headless {
            c.arg("--headless");
        }
        if !c.status()?.success() {
            bail!("session start failed ({} start)", self.launcher.display());
        }
        Ok(())
    }

    /// Waits until Android has booted. Progress goes to `status`.
    pub fn wait_booted(&self, status: &dyn Fn(&str)) -> Result<()> {
        let t = Instant::now();
        let limit = Duration::from_secs(300);
        let mut said = false;
        while !self.booted() {
            if !self.running() {
                bail!("the session stopped while booting (see `linuxapk-session log`)");
            }
            if t.elapsed() > limit {
                bail!("the session did not finish booting in {limit:?}");
            }
            if !said && t.elapsed() > Duration::from_secs(3) {
                status("waiting for Android to boot");
                said = true;
            }
            sleep(Duration::from_secs(1));
        }
        if said {
            // Freshly booted: wake the display and dismiss the keyguard.
            let _ = self.exec(15, &["input", "keyevent", "WAKEUP"]);
            let _ = self.exec(15, &["wm", "dismiss-keyguard"]);
        }
        Ok(())
    }

    /// Makes sure a session is running and booted, starting one (with
    /// windows) if needed. Progress goes to `status`.
    pub fn ensure(&self, status: &dyn Fn(&str)) -> Result<()> {
        if !self.running() {
            self.start(false, status)?;
        }
        self.wait_booted(status)
    }
}

/// The status line for a first start's image download.
pub const PREPARING: &str = "preparing Android (first run: downloading about 1.2 GB)";
