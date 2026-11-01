//! adb setup at session start. It writes the properties that turn on
//! Android's adbd and tells the user's adb server about the profile. The
//! shared adb code is in [`innkeeper_profile::adb`].

use std::time::Duration;

use anyhow::Result;
pub use innkeeper_profile::adb::*;

use crate::Paths;
use crate::profile::Settings;

/// Returns the adb properties for `settings`. adb authentication is always
/// on, and adbd listens on the profile's port when the profile has adb on.
pub fn props(settings: &Settings) -> String {
    let mut s = String::from("ro.adb.secure=1\n");
    if settings.adb && settings.adb_port != 0 {
        s += &format!(
            "service.adb.listen_addrs={}\nvendor.innkeeper.adb=1\n",
            listen_addrs(settings.adb_port)
        );
    } else {
        s += "vendor.innkeeper.adb=0\n";
    }
    s
}

/// Returns the profile's adb properties at start. When adb is on, it first
/// assigns a port and authorizes the user's key.
pub fn prepare(paths: &Paths) -> Result<String> {
    let mut settings = Settings::load(&paths.data)?;
    if settings.adb {
        if settings.assign_port(&paths.data_home, &paths.data)? {
            settings.save(&paths.data)?;
        }
        match user_key() {
            Some(key) => {
                authorize(&paths.data, &key)?;
            }
            None => crate::session::log(
                paths,
                &format!(
                    "adb: no key at {}; `adb start-server` creates one",
                    user_key_path().display()
                ),
            ),
        }
    }
    Ok(props(&settings))
}

/// If the profile has adb on, tells the user's adb server about it once adbd
/// listens. This runs in a background thread.
pub fn announce_at_start(paths: &Paths) {
    let Ok(s) = Settings::load(&paths.data) else {
        return;
    };
    if !s.adb || s.adb_port == 0 {
        return;
    }
    let paths = paths.clone();
    std::thread::spawn(move || {
        let r = announce_when_listening(server_port(), s.adb_port, Duration::from_secs(180));
        crate::session::log(
            &paths,
            &match r {
                Ok(()) => format!("adb: the adb server lists {}", serial(s.adb_port)),
                Err(e) => format!("adb: {e:#}"),
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_follow_the_setting() {
        let mut s = Settings::default();
        assert_eq!(props(&s), "ro.adb.secure=1\nvendor.innkeeper.adb=0\n");
        s.adb = true;
        s.adb_port = 5557;
        let p = props(&s);
        assert!(p.starts_with("ro.adb.secure=1\n"));
        assert!(p.contains("service.adb.listen_addrs=tcp:localhost:5557\n"));
        assert!(p.contains("vendor.innkeeper.adb=1\n"));
    }
}
