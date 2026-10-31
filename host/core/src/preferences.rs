//! Desktop preferences: saved to the profile's file, then applied live by a
//! running session's bridge (Session1 `ReloadPreferences`).
use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use zbus::zvariant::{OwnedValue, Value};

pub use innkeeper_session::preferences::Preferences;

/// Set the Background portal's login command. Never create an autostart file.
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
    let mut command = crate::paths::command(
        &[crate::paths::Prefix::locate()?
            .launcher()
            .to_string_lossy()
            .into_owned()],
        &crate::paths::profile()?,
    );
    command.push("start".into());
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
    prefs.validate()?;
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
    reload()
}

/// Has a running session's bridge apply the saved preferences.
fn reload() -> Result<()> {
    let connection = zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_secs(5))
        .build()?;
    let name = crate::app_settings::bus_name(&crate::paths::profile()?);
    let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    if !bus.name_has_owner(name.as_str().try_into()?)? {
        return Ok(());
    }
    let proxy = zbus::blocking::Proxy::new(
        &connection,
        name.as_str(),
        crate::app_settings::PATH,
        crate::app_settings::INTERFACE,
    )?;
    let _: () = proxy
        .call("ReloadPreferences", &())
        .context("Saved, but the running session did not apply it")?;
    Ok(())
}
