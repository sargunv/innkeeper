//! Driving the Android session through the prefix's `linuxapk-session`
//! (modelled on `tests/compat/src/session.rs`). Commands run inside the
//! session as argv through `linuxapk-session exec`, which ends in
//! `exec "$@"`: no string is ever interpreted by a shell, host or Android.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::paths::{self, Prefix};
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

/// What [`Session::status`] reports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Status {
    pub running: bool,
    pub booted: bool,
    /// The session unit's memory use (cgroup `memory.current`), in bytes.
    pub memory: Option<u64>,
}

fn unit_memory(unit: &str) -> Option<u64> {
    let o = proc::run(
        Command::new("systemctl").args(["--user", "show", "-P", "MemoryCurrent", unit]),
        Stdio::null(),
        Duration::from_secs(10),
    )
    .ok()?;
    o.text().trim().parse().ok()
}

impl Session {
    /// The session of this program's prefix (and `LINUXAPK_INSTANCE`).
    pub fn find() -> Result<Self> {
        Ok(Self {
            launcher: Prefix::locate()?.launcher(),
        })
    }

    /// Runs a launcher subcommand with inherited stdio (for `session ...`).
    pub fn passthrough(&self, args: &[&str]) -> Result<i32> {
        let st = Command::new(&self.launcher).args(args).status()?;
        Ok(st.code().unwrap_or(1))
    }

    pub fn running(&self) -> bool {
        Command::new(&self.launcher)
            .arg("status")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
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

    /// Running state, boot state and memory use, for status displays.
    pub fn status(&self) -> Status {
        if !self.running() {
            return Status::default();
        }
        Status {
            running: true,
            booted: self.booted(),
            memory: paths::unit_name().ok().and_then(|u| unit_memory(&u)),
        }
    }

    pub fn booted(&self) -> bool {
        self.getprop("sys.boot_completed") == "1"
    }

    /// Makes sure a session is running and booted, starting one if needed.
    /// Progress goes to `status`.
    pub fn ensure(&self, status: &dyn Fn(&str)) -> Result<()> {
        if !self.running() {
            status("starting the Android session");
            let st = Command::new(&self.launcher)
                .arg("start")
                .stdout(Stdio::null())
                .status()?;
            if !st.success() {
                bail!("session start failed ({} start)", self.launcher.display());
            }
        }
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
}
