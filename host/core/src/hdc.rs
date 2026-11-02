//! hdc access to an OpenHarmony profile, built on
//! [`innkeeper_profile::hdc`]. This module turns hdc on and off and applies
//! the change at once if the profile is running.

use std::time::Duration;

use anyhow::Result;
use innkeeper_profile::hdc;
use innkeeper_profile::profile::Settings;

use crate::launcher::Session;
use crate::ops::{Event, Report};
use crate::paths::Prefix;

/// The command that connects the user's hdc server to the profile.
pub fn connect_command(settings: &Settings) -> String {
    hdc::connect_command(settings.hdc_port)
}

/// The address hdcd listens on.
pub fn address(settings: &Settings) -> String {
    hdc::address(settings.hdc_port)
}

/// What the CLI and the manager tell the user when a profile with hdc on
/// starts or turns hdc on.
pub fn announcement(settings: &Settings) -> String {
    format!(
        "hdc listens on {}. Connect with {}.",
        address(settings),
        connect_command(settings)
    )
}

/// Waits for hdcd to listen in a profile that has just started with hdc on.
pub fn wait_started(settings: &Settings) -> Result<()> {
    hdc::wait_listening(settings.hdc_port, Duration::from_secs(30))
}

/// Turns hdc on or off for OpenHarmony profile `name` and applies the change
/// at once if the profile is running. Turning hdc on assigns it a port and
/// authorizes the user's key. Returns the new settings.
pub fn set(name: &str, on: bool, report: Report) -> Result<Settings> {
    let paths = Prefix::locate()?.profile_paths(name)?;
    let mut s = Settings::load(&paths.data)?;
    s.system.need_ohos("hdc")?;
    s.hdc = on;
    if on {
        s.assign_hdc_port(&paths.data_home, &paths.data)?;
        match hdc::user_key() {
            Some(key) => {
                hdc::authorize(&paths.data, &key)?;
            }
            None => report(Event::Warning(missing_key().unwrap_or_default())),
        }
    }
    s.save(&paths.data)?;
    let session = Session::of_profile(name)?;
    if session.running() {
        session.wait_booted(&|m| report(Event::Status(m.to_string())))?;
        for argv in hdc::commands(s.hdc_port, on) {
            let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
            session.exec_ok(15, &argv)?;
        }
        if on {
            hdc::wait_listening(s.hdc_port, Duration::from_secs(15))?;
        }
    }
    Ok(s)
}

/// Returns a warning if the user has no hdc key, because OpenHarmony will
/// then ask whether to allow the connection.
pub fn missing_key() -> Option<String> {
    hdc::user_key().is_none().then(|| {
        format!(
            "there is no hdc key at {}. hdc creates one when it first connects, and \
             OpenHarmony then asks whether to allow it. Turn hdc on again to allow the \
             key without asking",
            hdc::user_key_path().display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use innkeeper_profile::profile::System;

    #[test]
    fn the_announcement_names_the_port_and_the_command() {
        let s = Settings {
            hdc: true,
            hdc_port: 55557,
            ..Settings::new(System::Ohos)
        };
        assert_eq!(
            announcement(&s),
            "hdc listens on 127.0.0.1:55557. Connect with hdc tconn 127.0.0.1:55557."
        );
    }
}
