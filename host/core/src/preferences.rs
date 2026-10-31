//! Desktop preferences, saved without booting Android; live changes use Session1.
use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use zbus::zvariant::{OwnedValue, Value};

pub use linuxapk_session::preferences::Preferences;

pub fn encode(prefs: &Preferences) -> Result<String> {
    prefs.validate()?;
    Ok(serde_json::to_string(prefs)?)
}

pub fn decode(json: &str) -> Result<Preferences> {
    ensure!(json.len() <= 4096, "Preferences too large");
    let p: Preferences = serde_json::from_str(json)?;
    p.validate()?;
    Ok(p)
}

/// The addon is optional and prepared separately from the application build.
pub fn microg_available() -> bool {
    crate::paths::xdg_home("XDG_DATA_HOME", ".local/share")
        .join("linuxapk/addons/microg/system_ext/priv-app/GmsCore/GmsCore.apk")
        .is_file()
}

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
    let command = vec![
        crate::paths::Prefix::locate()?
            .launcher()
            .to_string_lossy()
            .into_owned(),
        "start".into(),
    ];
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
    if login_changed {
        ensure!(
            crate::paths::profile()?.is_empty(),
            "Start at login is only available for the main session"
        );
        if let Err(e) = autostart(prefs.keep_running) {
            // A partial grant must not leave login startup enabled while the
            // saved preference still says to stop when idle.
            if prefs.keep_running {
                let _ = autostart(false);
            }
            return Err(e);
        }
    }
    let result = (|| {
        let connection = zbus::blocking::connection::Builder::session()?
            .method_timeout(Duration::from_secs(5))
            .build()?;
        let name = crate::app_settings::bus_name(&crate::paths::profile()?);
        let bus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
        if bus.name_has_owner(name.as_str().try_into()?)? {
            let proxy = zbus::blocking::Proxy::new(
                &connection,
                name.as_str(),
                crate::app_settings::PATH,
                crate::app_settings::INTERFACE,
            )?;
            let _: () = proxy.call("SetPreferences", &(encode(&prefs)?,))?;
            Ok(())
        } else {
            prefs.save()
        }
    })();
    if result.is_err() && login_changed {
        let _ = autostart(old.keep_running);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_json_and_bounds() {
        let p = Preferences {
            follow_dark: false,
            idle_minutes: 13,
            ..Preferences::default()
        };
        let json = encode(&p).unwrap();
        assert_eq!(decode(&json).unwrap(), p);
        assert!(decode(r#"{"idle_minutes":0}"#).is_err());
        assert!(decode(r#"{"follow_dark":"yes"}"#).is_err());
        assert!(decode(r#"{"unknown":true}"#).is_err());
        assert!(decode(&" ".repeat(4097)).is_err());
    }
}
