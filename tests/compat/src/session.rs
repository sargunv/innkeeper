//! Driving the live Android session through the staged `linuxapk-session`.

use std::cell::Cell;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// The session the harness drives. A session it started itself is stopped
/// again when this is dropped, so a run leaves nothing running that it started.
pub struct Session {
    launcher: PathBuf,
    started: Cell<bool>,
}

pub struct Output {
    pub ok: bool,
    pub text: String,
}

impl Session {
    pub fn new(launcher: PathBuf) -> Self {
        Self {
            launcher,
            started: Cell::new(false),
        }
    }

    /// Host pid of the session's Android init, if running.
    pub fn pid(&self) -> Option<u32> {
        let out = Command::new(&self.launcher).arg("status").output().ok()?;
        let s = String::from_utf8_lossy(&out.stdout);
        s.strip_prefix("running: host pid ")?.trim().parse().ok()
    }

    /// Runs `cmd` inside the session as root, killed after `secs`.
    pub fn run(&self, secs: u64, cmd: &[&str]) -> Output {
        self.run_with(secs, cmd, Stdio::null())
    }

    fn run_with(&self, secs: u64, cmd: &[&str], stdin: Stdio) -> Output {
        let res = Command::new("timeout")
            .args(["-k", "5", &secs.to_string()])
            .arg(&self.launcher)
            .arg("exec")
            .args(cmd)
            .stdin(stdin)
            .output();
        match res {
            Ok(o) => {
                let mut text = String::from_utf8_lossy(&o.stdout).into_owned();
                let err = String::from_utf8_lossy(&o.stderr);
                // The session's linker warns on stderr for some binaries; keep the
                // rest, it carries pm/am failure messages.
                for l in err.lines().filter(|l| !l.contains("linker")) {
                    text.push_str(l);
                    text.push('\n');
                }
                Output {
                    ok: o.status.success(),
                    text,
                }
            }
            Err(e) => Output {
                ok: false,
                text: format!("spawn failed: {e}"),
            },
        }
    }

    /// Shell snippet inside the session.
    pub fn sh(&self, secs: u64, script: &str) -> Output {
        self.run(secs, &["sh", "-c", script])
    }

    /// The host bridge's log, in the session's state directory.
    fn bridge_log_path(&self) -> PathBuf {
        linuxapk_core::paths::state_dir()
            .unwrap_or_default()
            .join("bridge.log")
    }

    /// The bridge log's current size, so a step can look only at what comes
    /// after.
    pub fn bridge_log_len(&self) -> u64 {
        std::fs::metadata(self.bridge_log_path()).map_or(0, |m| m.len())
    }

    /// The bridge log from byte `from` on (at most its last MiB).
    pub fn bridge_log(&self, from: u64) -> String {
        use std::io::{Read, Seek, SeekFrom};
        let Ok(mut f) = File::open(self.bridge_log_path()) else {
            return String::new();
        };
        let len = f.metadata().map_or(0, |m| m.len());
        let start = from.min(len).max(len.saturating_sub(1 << 20));
        let mut b = Vec::new();
        if f.seek(SeekFrom::Start(start)).is_ok() {
            let _ = f.read_to_end(&mut b);
        }
        String::from_utf8_lossy(&b).into_owned()
    }

    /// Copies a host file into the session.
    pub fn push(&self, src: &Path, dst: &str) -> Result<()> {
        let f = File::open(src).with_context(|| format!("open {}", src.display()))?;
        let o = self.run_with(300, &["sh", "-c", &format!("cat > {dst}")], f.into());
        if !o.ok {
            bail!("push {} failed: {}", src.display(), o.text.trim());
        }
        Ok(())
    }

    /// Copies a file out of the session.
    pub fn pull(&self, src: &str, dst: &Path) -> Result<()> {
        let f = File::create(dst)?;
        let st = Command::new("timeout")
            .args(["-k", "5", "60"])
            .arg(&self.launcher)
            .args(["exec", "cat", src])
            .stdin(Stdio::null())
            .stdout(f)
            .stderr(Stdio::null())
            .status()?;
        if !st.success() {
            bail!("pull {src} failed");
        }
        Ok(())
    }

    pub fn getprop(&self, name: &str) -> String {
        self.run(15, &["getprop", name]).text.trim().to_string()
    }

    pub fn booted(&self) -> bool {
        self.getprop("sys.boot_completed") == "1"
    }

    /// Makes sure a session is running and booted. Starts one (headless unless
    /// `window`) if none is running.
    pub fn ensure(&self, window: bool) -> Result<()> {
        if self.pid().is_none() {
            self.start(window)?;
        }
        self.wait_boot(Duration::from_secs(300))?;
        self.prepare();
        Ok(())
    }

    fn start(&self, window: bool) -> Result<()> {
        eprintln!(
            "compat: starting a session ({})",
            if window { "window" } else { "headless" }
        );
        let mut c = Command::new(&self.launcher);
        c.arg("start");
        if !window {
            c.arg("--headless");
        }
        self.started.set(true);
        let st = c.status()?;
        if !st.success() {
            bail!("session start failed");
        }
        Ok(())
    }

    pub fn wait_boot(&self, limit: Duration) -> Result<()> {
        let t = Instant::now();
        while t.elapsed() < limit {
            if self.pid().is_none() {
                bail!("session is not running");
            }
            if self.booted() {
                if t.elapsed() > Duration::from_secs(1) {
                    eprintln!("compat: session booted ({:.0?} wait)", t.elapsed());
                }
                return Ok(());
            }
            sleep(Duration::from_secs(2));
        }
        bail!("session did not reach sys.boot_completed=1 in {limit:?}")
    }

    /// Per-boot setup: a bigger log buffer, screen awake and unlocked.
    pub fn prepare(&self) {
        self.run(30, &["logcat", "-G", "16M"]);
        self.run(15, &["input", "keyevent", "WAKEUP"]);
        self.run(15, &["wm", "dismiss-keyguard"]);
    }

    /// Pid of Android's system_server, if running.
    pub fn system_server_pid(&self) -> Option<String> {
        let o = self.run(15, &["pidof", "system_server"]);
        let p = o.text.trim().to_string();
        (o.ok && !p.is_empty()).then_some(p)
    }

    /// The last native crash in the crash buffer: process and abort message.
    pub fn last_fatal(&self) -> String {
        let log = self
            .run(20, &["logcat", "-d", "-b", "crash", "-t", "300"])
            .text;
        let proc = log
            .lines()
            .rfind(|l| l.contains(">>> "))
            .and_then(|l| l.split(">>> ").nth(1))
            .map(|p| p.trim_end_matches(" <<<").trim().to_string());
        let msg = log
            .lines()
            .rfind(|l| l.contains("Abort message: "))
            .and_then(|l| l.split("Abort message: ").nth(1))
            .map(|m| m.trim().trim_matches('\'').to_string());
        match (proc, msg) {
            (Some(p), Some(m)) => format!("{p}: {m}"),
            (Some(p), None) => format!("{p} crashed"),
            (None, _) => "system_server restarted".into(),
        }
    }

    /// Waits until a restarted Android userspace serves activities again.
    pub fn wait_system(&self, limit: Duration) -> Result<()> {
        let t = Instant::now();
        let mut last = None;
        while t.elapsed() < limit {
            let pid = self.system_server_pid();
            let home = self.run(
                20,
                &[
                    "cmd",
                    "package",
                    "resolve-activity",
                    "--brief",
                    "-a",
                    "android.intent.action.MAIN",
                    "-c",
                    "android.intent.category.HOME",
                ],
            );
            if pid.is_some() && pid == last && home.text.contains('/') {
                sleep(Duration::from_secs(10));
                eprintln!("compat: Android is back ({:.0?})", t.elapsed());
                return Ok(());
            }
            last = pid;
            sleep(Duration::from_secs(5));
        }
        bail!("Android did not come back in {limit:?}")
    }

    /// Whether the composer runs without host windows.
    pub fn headless(&self) -> bool {
        self.getprop("vendor.linuxapk.hwc.backend") == "headless"
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if self.started.get() {
            eprintln!("compat: stopping the session this run started");
            let _ = Command::new(&self.launcher)
                .arg("stop")
                .stdout(Stdio::null())
                .status();
        }
    }
}
