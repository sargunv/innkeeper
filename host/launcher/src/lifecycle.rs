//! The session's lifecycle (docs/session.md, "Lifecycle"): it stops itself
//! after a grace period with nothing to do, and a wakeup restarts it for the
//! next alarm.
//!
//! - **Activity** comes from the system app, which reports its windows,
//!   foreground services, background-allowed apps and next wakeup to the
//!   bridge's system service; the bridge writes it to `<run>/lifecycle`
//!   ([`Activity`]). No report (booting, or the system app is down) counts
//!   as busy.
//! - **Commands** keep it up while they run: `exec` holds a shared lock on
//!   `<run>/exec.lock`.
//! - **Wakeups:** host mode leaves a transient systemd user timer that runs
//!   `start` shortly before the alarm. In Flatpak mode the session's process
//!   stays as a waiter in its sandbox, serving `status` and `stop` on the
//!   control socket, and starts the session again at that time.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::conf::Config;
use crate::paths::Paths;
use crate::{host, session};

/// The grace period of a session with windows, unless `LINUXAPK_IDLE_GRACE`
/// says otherwise.
pub const DEFAULT_GRACE_SECS: u64 = 120;
/// How often activity is checked.
const POLL: Duration = Duration::from_secs(5);
/// A wakeup sooner than this keeps the session up: stopping would gain
/// little, and the alarm's app might not have rescheduled it in time.
const IMMINENT: Duration = Duration::from_secs(180);
/// How long before a wakeup the session starts again: boot, then the
/// alarm's app reschedules it from `BOOT_COMPLETED` (Android forgets alarms
/// when it stops).
pub const WAKE_LEAD: Duration = Duration::from_secs(60);

/// The grace period for a new session, in seconds; 0: it never stops by
/// itself. Headless sessions (tests) never do unless `LINUXAPK_IDLE_GRACE`
/// is set.
pub fn grace_secs(headless: bool) -> u64 {
    match std::env::var("LINUXAPK_IDLE_GRACE")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        Some(s) => s,
        None if headless => 0,
        None => DEFAULT_GRACE_SECS,
    }
}

/// What the system app last reported (`<run>/lifecycle`, `key=value` lines).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Activity {
    /// App windows, shown or starting.
    pub windows: u32,
    /// Running foreground services.
    pub services: u32,
    /// Installed apps the user lets run in the background.
    pub background: Vec<String>,
    /// The next alarm clock or exact wakeup alarm, in ms since the epoch.
    pub wakeup_ms: Option<u64>,
}

impl Activity {
    pub fn parse(text: &str) -> Activity {
        let mut a = Activity::default();
        for l in text.lines() {
            let Some((k, v)) = l.split_once('=') else {
                continue;
            };
            match k {
                "windows" => a.windows = v.parse().unwrap_or(0),
                "services" => a.services = v.parse().unwrap_or(0),
                "background" => {
                    a.background = v
                        .split(',')
                        .filter(|p| !p.is_empty())
                        .map(str::to_string)
                        .collect()
                }
                "wakeup" => a.wakeup_ms = v.parse().ok().filter(|&t| t > 0),
                _ => {}
            }
        }
        a
    }

    pub fn read(paths: &Paths) -> Option<Activity> {
        fs::read_to_string(paths.lifecycle())
            .ok()
            .map(|t| Activity::parse(&t))
    }

    /// Why this keeps the session up at `now` (ms since the epoch), if it
    /// does.
    pub fn busy(&self, now_ms: u64) -> Option<String> {
        if self.windows > 0 {
            return Some(format!("{} window(s)", self.windows));
        }
        if self.services > 0 {
            return Some(format!("{} foreground service(s)", self.services));
        }
        if !self.background.is_empty() {
            return Some(format!(
                "running in the background: {}",
                self.background.join(", ")
            ));
        }
        // A wakeup that has just passed is likely ringing now; one long past
        // is stale.
        let soon = |t: u64| {
            t.saturating_sub(now_ms) < IMMINENT.as_millis() as u64
                && now_ms.saturating_sub(t) < IMMINENT.as_millis() as u64
        };
        match self.wakeup_ms {
            Some(t) if soon(t) => Some(format!("the alarm at {}", calendar(t / 1000))),
            _ => None,
        }
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// Opens the lock that `exec` holds shared while a command runs.
fn exec_lock_file(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)
}

/// Takes the shared exec lock at `path` ([`Paths::exec_lock`]) for as long
/// as the returned file is open. `None` if the session's runtime dir is
/// gone.
pub fn hold_exec_lock(path: &Path) -> Option<File> {
    let f = exec_lock_file(path).ok()?;
    (unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_SH) } == 0).then_some(f)
}

/// Whether some `exec` holds the lock.
fn exec_running(paths: &Paths) -> bool {
    let Ok(f) = exec_lock_file(&paths.exec_lock()) else {
        return false;
    };
    // Dropping the file releases the exclusive lock at once.
    unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) != 0 }
}

/// Why the session must stay up now, if it must.
fn busy(paths: &Paths) -> Option<String> {
    if exec_running(paths) {
        return Some("a command is running".into());
    }
    match Activity::read(paths) {
        None => Some("no report from the system app yet".into()),
        Some(a) => a.busy(now_ms()),
    }
}

/// Watches the session's activity on a thread of its own. After
/// `cfg.idle_grace` seconds of idleness it calls `end` with the next wakeup
/// (ms since the epoch), once. Does nothing if the grace period is 0.
pub fn watch(cfg: &Config, end: impl FnOnce(Option<u64>) + Send + 'static) {
    if cfg.idle_grace == 0 {
        return;
    }
    let grace = Duration::from_secs(cfg.idle_grace);
    let paths = cfg.paths.clone();
    let flatpak = cfg.flatpak();
    thread::spawn(move || {
        let mut idle_since: Option<Instant> = None;
        let mut last = String::new();
        let mut asked_background = false;
        loop {
            thread::sleep(POLL);
            let why = busy(&paths);
            // In Flatpak, running with no window is the Background portal's
            // to allow.
            if flatpak
                && !asked_background
                && Activity::read(&paths)
                    .is_some_and(|a| a.windows == 0 && (a.services > 0 || !a.background.is_empty()))
            {
                asked_background = true;
                crate::flatpak::request_background(&paths);
            }
            let state = why.clone().unwrap_or_else(|| "idle".into());
            if state != last {
                session::log(&paths, &format!("activity: {state}"));
                last = state;
            }
            if why.is_some() {
                idle_since = None;
                continue;
            }
            let since = *idle_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= grace {
                let wakeup = Activity::read(&paths).and_then(|a| a.wakeup_ms);
                session::log(
                    &paths,
                    &format!(
                        "idle for {} s: stopping{}",
                        grace.as_secs(),
                        wakeup.map_or(String::new(), |t| format!(
                            " (next wakeup {})",
                            calendar(t / 1000)
                        ))
                    ),
                );
                end(wakeup);
                return;
            }
        }
    });
}

/// What a woken session starts with of `start`'s environment: what shapes
/// the session (the desktop's sockets, the XDG dirs, Linux.apk's own knobs).
const START_ENV: &[&str] = &[
    "WAYLAND_DISPLAY",
    "DBUS_SESSION_BUS_ADDRESS",
    "HOME",
    "PATH",
    "XDG_RUNTIME_DIR",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "XDG_STATE_HOME",
    "XDG_CONFIG_DIRS",
    "XDG_DATA_DIRS",
    "XDG_CURRENT_DESKTOP",
];

pub fn start_env() -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| k.starts_with("LINUXAPK_") || START_ENV.contains(&k.as_str()))
        .collect()
}

/// `secs` since the epoch as a systemd calendar event in UTC.
pub fn calendar(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Howard Hinnant's civil_from_days.
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} UTC",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn systemctl(args: &[&str]) -> Command {
    let mut c = host::user_manager("systemctl");
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    c
}

/// Host mode: a transient systemd user timer that starts the session
/// [`WAKE_LEAD`] before `at_ms`.
pub fn schedule_host(cfg: &Config, exe: &Path, at_ms: u64) -> Result<()> {
    let p = &cfg.paths;
    let at = (at_ms / 1000).saturating_sub(WAKE_LEAD.as_secs());
    cancel_host(p);
    let mut c = host::user_manager("systemd-run");
    c.args(["--quiet", "--collect"])
        .arg(format!("--unit={}", p.wake_unit()))
        .arg(format!("--on-calendar={}", calendar(at)))
        .args([
            "--timer-property=AccuracySec=1s",
            "--timer-property=RemainAfterElapse=no",
        ]);
    for (k, v) in &cfg.start_env {
        c.arg(format!("--setenv={k}={v}"));
    }
    c.arg("--").arg(exe).arg("start");
    if cfg.headless {
        c.arg("--headless");
    }
    let st = c.stdin(Stdio::null()).status().context("run systemd-run")?;
    if !st.success() {
        bail!("systemd-run for the wakeup timer failed ({st})");
    }
    session::log(p, &format!("wakeup timer: start at {}", calendar(at)));
    Ok(())
}

/// Host mode: removes a pending wakeup timer.
pub fn cancel_host(p: &Paths) {
    let t = format!("{}.timer", p.wake_unit());
    let _ = systemctl(&["stop", &t]).status();
    let _ = systemctl(&["reset-failed", &t]).status();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity() {
        let a = Activity::parse(
            "windows=0\nservices=2\nbackground=me.ash.reader,org.x\nwakeup=1700000000000\njunk\n",
        );
        assert_eq!(
            a,
            Activity {
                windows: 0,
                services: 2,
                background: vec!["me.ash.reader".into(), "org.x".into()],
                wakeup_ms: Some(1_700_000_000_000),
            }
        );
        let idle = Activity::parse("windows=0\nservices=0\nbackground=\nwakeup=0\n");
        assert_eq!(idle, Activity::default());
        assert_eq!(idle.busy(1_000), None);
        assert!(a.busy(0).unwrap().contains("foreground"));
        let bg = Activity {
            background: vec!["me.ash.reader".into()],
            ..Activity::default()
        };
        assert!(bg.busy(0).unwrap().contains("me.ash.reader"));
        assert!(
            Activity {
                windows: 1,
                ..Activity::default()
            }
            .busy(0)
            .is_some()
        );
    }

    #[test]
    fn imminent_wakeups_keep_it_up() {
        let at = |s: u64| Activity {
            wakeup_ms: Some(s * 1000),
            ..Activity::default()
        };
        let now = 1_000_000_000;
        assert!(at(1_000_000 + 60).busy(now).is_some());
        // Just past: ringing now. Long past: stale.
        assert!(at(999_990).busy(now).is_some());
        assert_eq!(at(990_000).busy(now), None);
        assert_eq!(at(1_000_000 + 600).busy(now), None);
    }

    #[test]
    fn calendar_events() {
        assert_eq!(calendar(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(calendar(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(calendar(1_793_222_130), "2026-10-28 21:15:30 UTC");
    }

    #[test]
    fn grace() {
        // SAFETY: tests in this module don't read it concurrently.
        unsafe { std::env::remove_var("LINUXAPK_IDLE_GRACE") };
        assert_eq!(grace_secs(true), 0);
        assert_eq!(grace_secs(false), DEFAULT_GRACE_SECS);
    }
}
