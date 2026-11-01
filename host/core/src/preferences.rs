//! Innkeeper's preferences, shared by every profile
//! ([`innkeeper_profile::preferences`]). Saving Keep Running also sets the
//! Background portal's login command. Running sessions read the preferences
//! when they poll.
use std::collections::HashMap;

use anyhow::{Context, Result, ensure};
use zbus::zvariant::{OwnedValue, Value};

use innkeeper_profile::preferences::Preferences;

/// Sets or clears the Background portal's login command. Innkeeper doesn't
/// write an autostart file.
fn autostart(enabled: bool) -> Result<()> {
    let connection = zbus::blocking::Connection::session()?;
    let token = format!("preferences{}", std::process::id());
    let sender = connection
        .unique_name()
        .context("No bus name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request = zbus::blocking::Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        request_path.as_str(),
        "org.freedesktop.portal.Request",
    )?;
    let mut responses = request.receive_signal("Response")?;
    // Keep Running applies to every profile, so start them all.
    let command: Vec<String> = [crate::xdg::INNKEEPER, "profile", "start", "--all"]
        .map(String::from)
        .into();
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert(
        "reason",
        Value::from("Keep Android ready for apps and background work"),
    );
    options.insert("autostart", Value::from(enabled));
    options.insert("commandline", Value::from(command));
    connection.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.Background"),
        "RequestBackground",
        &("", options),
    )?;
    let response = responses
        .next()
        .context("Background portal did not respond")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = response.body().deserialize()?;
    ensure!(code == 0, "Background permission was declined");
    let autostart = results
        .get("autostart")
        .and_then(|v| bool::try_from(v).ok())
        .unwrap_or(false);
    if enabled {
        ensure!(
            autostart
                && results
                    .get("background")
                    .and_then(|v| bool::try_from(v).ok())
                    == Some(true),
            "The desktop did not allow starting Android at login"
        );
    } else {
        ensure!(
            !autostart,
            "The desktop did not disable starting Android at login"
        );
    }
    Ok(())
}

pub fn save(prefs: Preferences) -> Result<()> {
    let old = Preferences::load()?;
    let login_changed = prefs.keep_running != old.keep_running;
    if login_changed && let Err(e) = autostart(prefs.keep_running) {
        // A partial grant must not leave login startup enabled while the
        // saved preference still says to stop when idle.
        if prefs.keep_running {
            let _ = autostart(false);
        }
        return Err(e);
    }
    if let Err(e) = prefs.save() {
        if login_changed {
            let _ = autostart(old.keep_running);
        }
        return Err(e);
    }
    Ok(())
}
