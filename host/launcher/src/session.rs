//! What runs beside Android in both modes: the kmsg log reader and the
//! desktop bridge, plus the session's pid files.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::conf::Config;
use crate::paths::Paths;
use crate::sys;

/// `path` opened for appending (created if missing).
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
        let _ = writeln!(f, "linuxapk-session: {msg}");
    }
}

fn read_pid(p: &Path) -> Option<i32> {
    fs::read_to_string(p).ok()?.trim().parse().ok()
}

/// Android init's pid (in this process's pid namespace), if it is alive.
pub fn ns_pid(paths: &Paths) -> Option<i32> {
    read_pid(&paths.pid_file()).filter(|&p| sys::alive(p))
}

/// The process init is a child of, if it is alive.
pub fn outer_pid(paths: &Paths) -> Option<i32> {
    read_pid(&paths.outer_pid_file()).filter(|&p| sys::alive(p))
}

pub fn write_pids(paths: &Paths, outer: i32, init: i32) -> Result<()> {
    fs::write(paths.outer_pid_file(), format!("{outer}\n"))?;
    fs::write(paths.pid_file(), format!("{init}\n"))?;
    Ok(())
}

pub fn remove_pids(paths: &Paths) {
    let _ = fs::remove_file(paths.pid_file());
    let _ = fs::remove_file(paths.outer_pid_file());
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
    let bridge = p.libexec.join("linuxapk-bridge");
    let (args, env, log) = (
        cfg.bridge_args.clone(),
        cfg.bridge_env.clone(),
        p.bridge_log(),
    );
    thread::spawn(move || {
        loop {
            let st = (|| -> Result<String> {
                let out = append(&log)?;
                let st = Command::new(&bridge)
                    .args(&args)
                    .envs(&env)
                    .stdin(Stdio::null())
                    .stdout(out.try_clone()?)
                    .stderr(out)
                    .status()?;
                Ok(st.to_string())
            })()
            .unwrap_or_else(|e| format!("{e:#}"));
            if let Ok(mut f) = append(&log) {
                let _ = writeln!(f, "linuxapk-session: bridge exited ({st}); restarting");
            }
            thread::sleep(Duration::from_secs(1));
            if ENDING.load(Ordering::SeqCst) {
                return;
            }
        }
    });
    let sock = p.run.join("bridge/bridge.sock");
    for _ in 0..50 {
        if sock.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(20));
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
        // Truncated (a new session): start over.
        if f.stream_position()? > f.metadata()?.len() {
            f.seek(SeekFrom::Start(0))?;
        }
        thread::sleep(Duration::from_millis(200));
    }
}
