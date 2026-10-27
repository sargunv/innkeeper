//! Driving the Android session through `session/linuxapk-session` (modelled
//! on `compat/src/session.rs`). Commands run inside the session as argv
//! through `linuxapk-session exec`, which ends in `exec "$@"`: no string is
//! ever interpreted by a shell, host or Android.

use std::env;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::proc;

pub struct Session {
    launcher: PathBuf,
}

/// Finds `linuxapk-session`: `$LINUXAPK_SESSION`; the checkout this binary
/// runs from (`<repo>/target/*/linuxapk`) or was built in, or its main checkout
/// if that is a git worktree whose image isn't prepared; or `linuxapk-session`
/// on `PATH`.
pub fn find_launcher() -> Result<PathBuf> {
    if let Some(p) = env::var_os("LINUXAPK_SESSION") {
        return Ok(PathBuf::from(p));
    }
    // The checkout this binary runs from, then the one it was built in.
    let mut roots: Vec<PathBuf> = vec![];
    if let Ok(exe) = env::current_exe().and_then(|p| p.canonicalize()) {
        roots.extend(exe.ancestors().skip(1).map(Path::to_path_buf));
    }
    roots.extend(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .map(Path::to_path_buf),
    );
    for dir in roots {
        let l = dir.join("session/linuxapk-session");
        if !l.is_file() {
            continue;
        }
        if image_ready(&l) {
            return Ok(l);
        }
        if let Some(main) = main_checkout(&dir) {
            let m = main.join("session/linuxapk-session");
            if m.is_file() {
                return Ok(m);
            }
        }
        return Ok(l);
    }
    proc::which("linuxapk-session").context("linuxapk-session not found: set LINUXAPK_SESSION")
}

/// Whether the image `launcher` selects is extracted and flattened, at the
/// paths `linuxapk-session image` prints.
fn image_ready(launcher: &Path) -> bool {
    let Ok(o) = proc::run(
        Command::new(launcher).arg("image"),
        Stdio::null(),
        Duration::from_secs(10),
    ) else {
        return false;
    };
    let text = o.text();
    let path = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(PathBuf::from)
    };
    o.ok()
        && path("img=").is_some_and(|p| p.join("system/system").is_dir())
        && path("apex=").is_some_and(|p| p.join("apex-info-list.xml").is_file())
}

/// The main checkout of a git worktree (`.git` is a file naming
/// `<main>/.git/worktrees/<name>`).
fn main_checkout(dir: &Path) -> Option<PathBuf> {
    let t = std::fs::read_to_string(dir.join(".git")).ok()?;
    let gitdir = PathBuf::from(t.strip_prefix("gitdir:")?.trim());
    let dotgit = gitdir.parent()?.parent()?;
    (dotgit.file_name()? == ".git").then(|| dotgit.parent().map(Path::to_path_buf))?
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

/// `LINUXAPK_INSTANCE` if it is a valid instance name (as the launcher checks
/// it), else an error.
pub fn instance() -> Result<String> {
    let inst = env::var("LINUXAPK_INSTANCE").unwrap_or_default();
    if !inst
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("bad LINUXAPK_INSTANCE");
    }
    Ok(inst)
}

/// The systemd user unit the launcher runs the session in.
pub fn unit_name() -> String {
    match instance() {
        Ok(i) if !i.is_empty() => format!("linuxapk-session-{i}"),
        _ => "linuxapk-session".into(),
    }
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
    pub fn find() -> Result<Self> {
        Ok(Self {
            launcher: find_launcher()?,
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
            memory: unit_memory(&unit_name()),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_main_checkout() {
        let d = env::temp_dir().join(format!("linuxapk-wt-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join(".git"),
            "gitdir: /home/u/Projects/linux.apk/.git/worktrees/cli\n",
        )
        .unwrap();
        assert_eq!(
            main_checkout(&d),
            Some(PathBuf::from("/home/u/Projects/linux.apk"))
        );
        std::fs::write(d.join(".git"), "gitdir: /x/y\n").unwrap();
        assert_eq!(main_checkout(&d), None);
        std::fs::remove_dir_all(&d).unwrap();
    }
}
