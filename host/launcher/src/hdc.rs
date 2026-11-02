//! hdc setup at the start of an OpenHarmony session. If the profile has hdc
//! on, it authorizes the user's key and starts hdcd on the profile's port
//! once the exec relay runs. The shared hdc code is in
//! [`innkeeper_profile::hdc`].

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
pub use innkeeper_profile::hdc::*;

use crate::Paths;
use crate::profile::{Settings, System};

/// If the profile has hdc on, starts hdcd in a background thread.
pub fn start_at_boot(paths: &Paths) {
    let port = match prepare(paths) {
        Ok(Some(port)) => port,
        Ok(None) => return,
        Err(e) => return crate::session::log(paths, &format!("hdc: {e:#}")),
    };
    let paths = paths.clone();
    std::thread::spawn(move || {
        let r = start(port, Duration::from_secs(180));
        crate::session::log(
            &paths,
            &match r {
                Ok(()) => format!("hdc: hdcd listens on {}", address(port)),
                Err(e) => format!("hdc: {e:#}"),
            },
        );
    });
}

/// Returns the profile's hdc port if it has hdc on. It first assigns a port
/// if the profile has none, and authorizes the user's key.
fn prepare(paths: &Paths) -> Result<Option<u16>> {
    let mut s = Settings::load(&paths.data)?;
    if !s.hdc {
        return Ok(None);
    }
    if s.assign_hdc_port(&paths.data_home, &paths.data)? {
        s.save(&paths.data)?;
    }
    match user_key() {
        Some(key) => {
            authorize(&paths.data, &key)?;
        }
        None => crate::session::log(
            paths,
            &format!(
                "hdc: no key at {}. hdc creates one when it first connects",
                user_key_path().display()
            ),
        ),
    }
    Ok(Some(s.hdc_port))
}

/// Runs the commands that start hdcd on `port` once the exec relay accepts
/// them, then waits for hdcd to listen. It gives up after `wait`.
fn start(port: u16, wait: Duration) -> Result<()> {
    let deadline = Instant::now() + wait;
    let commands = commands(port, true);
    let mut next = 0;
    while next < commands.len() {
        if run(&commands[next]) {
            next += 1;
        } else if Instant::now() >= deadline {
            bail!("`{}` failed", commands[next].join(" "));
        } else {
            std::thread::sleep(Duration::from_millis(250));
        }
    }
    wait_listening(port, deadline.saturating_duration_since(Instant::now()))
}

/// Runs `argv` in OpenHarmony through the exec relay and returns whether it
/// exited 0.
fn run(argv: &[String]) -> bool {
    Command::new(crate::exec::relay(System::Ohos))
        .args(argv)
        .env_clear()
        .envs(crate::exec::env(System::Ohos))
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}
