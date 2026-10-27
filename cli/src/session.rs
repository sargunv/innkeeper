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

/// Finds `linuxapk-session`: `$LINUXAPK_SESSION`; the checkout this binary was
/// runs from (`<repo>/target/*/linuxapk`) or was built in, or its main checkout
/// if that is a git worktree without an extracted image; or `linuxapk-session`
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
        if dir.join("out/img/system").is_dir() {
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

    fn booted(&self) -> bool {
        self.getprop("sys.boot_completed") == "1"
    }

    /// Makes sure a session is running and booted, starting one if needed (with
    /// its host window unless `LINUXAPK_HEADLESS=1`).
    pub fn ensure(&self) -> Result<()> {
        if !self.running() {
            let headless = env::var("LINUXAPK_HEADLESS").is_ok_and(|v| v == "1");
            eprintln!(
                "linuxapk: starting the Android session{}",
                if headless { " (headless)" } else { "" }
            );
            let mut c = Command::new(&self.launcher);
            c.arg("start");
            if headless {
                c.arg("--headless");
            }
            if !c.stdout(Stdio::null()).status()?.success() {
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
                eprintln!("linuxapk: waiting for Android to boot");
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
