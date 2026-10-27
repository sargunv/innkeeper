//! Driving the live Android session through `session/linuxapk-session`.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub struct Session {
    launcher: PathBuf,
}

pub struct Output {
    pub ok: bool,
    pub text: String,
}

impl Session {
    pub fn new(launcher: PathBuf) -> Self {
        Self { launcher }
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
    /// `window`) if none is running. Returns true if this call started it.
    pub fn ensure(&self, window: bool) -> Result<bool> {
        let mut started = false;
        if self.pid().is_none() {
            self.start(window)?;
            started = true;
        }
        self.wait_boot(Duration::from_secs(300))?;
        self.prepare();
        Ok(started)
    }

    pub fn start(&self, window: bool) -> Result<()> {
        eprintln!(
            "compat: starting a session ({})",
            if window { "window" } else { "headless" }
        );
        let mut c = Command::new(&self.launcher);
        c.arg("start");
        if !window {
            c.arg("--headless");
        }
        let st = c.status()?;
        if !st.success() {
            bail!("session start failed");
        }
        Ok(())
    }

    pub fn stop(&self) {
        let _ = Command::new(&self.launcher)
            .arg("stop")
            .stdout(Stdio::null())
            .status();
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

    /// "wayland" or "headless".
    pub fn backend(&self) -> String {
        self.getprop("vendor.linuxapk.hwc.backend")
    }
}
