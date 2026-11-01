//! Runs the kmsg log reader and the desktop bridge beside Android.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::conf::Config;
use crate::paths::Paths;
use crate::sys;

/// Opens `path` for appending and creates it if missing.
pub fn append(path: &Path) -> Result<File> {
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open {}", path.display()))
}

/// Appends a line to session.log.
pub fn log(paths: &Paths, msg: &str) {
    if let Ok(mut f) = append(&paths.session_log()) {
        let _ = writeln!(f, "innkeeper-session: {msg}");
    }
}

/// Logs launcher milestones. Each entry also has a time on Android's
/// CLOCK_BOOTTIME timebase, so init's ro.boottime.* properties and
/// boot_progress events can be compared directly.
pub struct BootTrace(Instant);

impl BootTrace {
    pub fn new() -> Self {
        Self(Instant::now())
    }

    pub fn mark(&self, paths: &Paths, stage: &str) {
        let mut ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        unsafe { libc::clock_gettime(libc::CLOCK_BOOTTIME, &mut ts) };
        log(
            paths,
            &format!(
                "boot +{} ms, boottime {} ms: {stage}",
                self.0.elapsed().as_millis(),
                ts.tv_sec * 1000 + ts.tv_nsec / 1_000_000
            ),
        );
    }
}

/// Empties the runtime dir and the logs, and makes the kmsg FIFO and the
/// bridge's socket dir.
pub fn fresh_dirs(paths: &Paths) -> Result<()> {
    let _ = fs::remove_dir_all(&paths.run);
    let bridge = paths.run.join("bridge");
    fs::create_dir_all(&bridge).with_context(|| format!("create {}", bridge.display()))?;
    fs::create_dir_all(&paths.state)?;
    for l in [paths.session_log(), paths.kmsg_log(), paths.bridge_log()] {
        File::create(&l).with_context(|| format!("create {}", l.display()))?;
    }
    sys::mkfifo(&paths.run.join("kmsg"), 0o600)
}

/// Copies what Android writes to `/dev/kmsg` (the FIFO) into kmsg.log. The
/// reader holds the FIFO open read-write, so writers never block or see EOF.
pub fn start_kmsg_reader(paths: &Paths) -> Result<()> {
    let mut fifo = OpenOptions::new()
        .read(true)
        .write(true)
        .open(paths.run.join("kmsg"))
        .context("open the kmsg FIFO")?;
    let mut log = append(&paths.kmsg_log())?;
    thread::spawn(move || {
        let mut buf = vec![0u8; 64 << 10];
        while let Ok(n) = fifo.read(&mut buf) {
            if n == 0 || log.write_all(&buf[..n]).is_err() {
                break;
            }
        }
    });
    Ok(())
}

/// Set when the session ends, so the bridge isn't restarted.
pub static ENDING: AtomicBool = AtomicBool::new(false);

/// Starts the bridge, restarts it whenever it exits, and waits for its socket.
pub fn start_bridge(cfg: &Config) -> Result<()> {
    let p = &cfg.paths;
    let bridge = p.libexec.join("innkeeper-bridge");
    let (env, log) = (cfg.bridge_env.clone(), p.bridge_log());
    thread::spawn(move || {
        loop {
            let st = (|| -> Result<String> {
                let out = append(&log)?;
                let st = Command::new(&bridge)
                    .envs(&env)
                    .stdin(Stdio::null())
                    .stdout(out.try_clone()?)
                    .stderr(out)
                    .status()?;
                Ok(st.to_string())
            })()
            .unwrap_or_else(|e| format!("{e:#}"));
            if let Ok(mut f) = append(&log) {
                let _ = writeln!(f, "innkeeper-session: bridge exited ({st}); restarting");
            }
            thread::sleep(Duration::from_secs(1));
            if ENDING.load(Ordering::SeqCst) {
                return;
            }
        }
    });
    let sock = p.run.join("bridge/bridge.sock");
    if wait_for_bridge(&sock, BRIDGE_DEADLINE) {
        return Ok(());
    }
    bail!(
        "the bridge did not start (see {})",
        p.bridge_log().display()
    )
}

/// Prints `file` from the start, following it while `alive()` holds.
pub fn tail(file: &Path, alive: &dyn Fn() -> bool) -> Result<()> {
    let mut f = File::open(file)?;
    let mut out = io::stdout();
    let mut buf = vec![0u8; 64 << 10];
    loop {
        let n = f.read(&mut buf)?;
        if n > 0 {
            out.write_all(&buf[..n])?;
            out.flush()?;
            continue;
        }
        if !alive() {
            return Ok(());
        }
        // A new session truncated the file, so start over.
        if f.stream_position()? > f.metadata()?.len() {
            f.seek(SeekFrom::Start(0))?;
        }
        thread::sleep(Duration::from_millis(200));
    }
}

/// How long the bridge may take to bind its socket. Registering its desktop
/// services can take seconds, and a crash costs a restart's backoff.
const BRIDGE_DEADLINE: Duration = Duration::from_secs(15);

/// Waits up to `limit` for the bridge's socket.
fn wait_for_bridge(sock: &Path, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if sock.exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waits_for_the_bridge_socket_until_the_deadline() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../out/test/bridge-ready-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("bridge.sock");
        assert!(!wait_for_bridge(&sock, Duration::from_millis(50)));
        let delayed = sock.clone();
        let bridge = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            fs::write(delayed, "").unwrap();
        });
        let ready = wait_for_bridge(&sock, BRIDGE_DEADLINE);
        bridge.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert!(ready, "the bridge was rejected before binding its socket");
    }
}
