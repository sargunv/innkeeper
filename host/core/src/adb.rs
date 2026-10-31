//! adb into a profile ([`innkeeper_profile::adb`]): turning it on and off,
//! live if the profile runs, and telling the user's adb server about it.

use std::time::Duration;

use anyhow::Result;
use innkeeper_profile::adb;
use innkeeper_profile::profile::Settings;

use crate::launcher::Session;
use crate::ops::{Event, Report};
use crate::paths::Prefix;

/// The serial adb knows profile `settings` by: `emulator-<port - 1>`.
pub fn serial(settings: &Settings) -> String {
    adb::serial(settings.adb_port)
}

/// Where adbd listens.
pub fn address(settings: &Settings) -> String {
    format!("127.0.0.1:{}", settings.adb_port)
}

/// Turns adb on or off for profile `name`, at once if it runs. Turning it on
/// gives it a port, authorizes the user's key and tells a running adb server.
/// Returns the new settings.
pub fn set(name: &str, on: bool, report: Report) -> Result<Settings> {
    let paths = Prefix::locate()?.profile_paths(name)?;
    let mut s = Settings::load(&paths.data)?;
    s.adb = on;
    if on {
        s.assign_port(&paths.data_home, &paths.data)?;
        authorize(&paths.data, report)?;
    }
    s.save(&paths.data)?;
    let session = Session::of_profile(name)?;
    if session.running() {
        session.wait_booted(&|m| report(Event::Status(m.to_string())))?;
        apply(&session, &s)?;
        if on {
            let r = adb::announce_when_listening(
                adb::server_port(),
                s.adb_port,
                Duration::from_secs(15),
            );
            if let Err(e) = r {
                report(Event::Warning(format!(
                    "{e:#}; `adb devices` finds {} once a server runs",
                    serial(&s)
                )));
            }
        }
    }
    Ok(s)
}

/// Starts or stops adbd in running `session` as `s` says.
fn apply(session: &Session, s: &Settings) -> Result<()> {
    let on = s.adb;
    if on {
        let addrs = adb::listen_addrs(s.adb_port);
        session.exec_ok(15, &["setprop", "service.adb.listen_addrs", &addrs])?;
    }
    session.exec_ok(
        15,
        &[
            "setprop",
            "vendor.innkeeper.adb",
            if on { "1" } else { "0" },
        ],
    )?;
    Ok(())
}

/// Why adb will refuse the user, if it will: they have no adb key.
pub fn missing_key() -> Option<String> {
    adb::user_key().is_none().then(|| {
        format!(
            "no adb key at {} (`adb start-server` creates one), so adb will be refused",
            adb::user_key_path().display()
        )
    })
}

/// Authorizes the user's adb key in data directory `data`, warning if there
/// is none.
fn authorize(data: &std::path::Path, report: Report) -> Result<()> {
    match adb::user_key() {
        Some(key) => {
            adb::authorize(data, &key)?;
        }
        None => report(Event::Warning(missing_key().unwrap_or_default())),
    }
    Ok(())
}
