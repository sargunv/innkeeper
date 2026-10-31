//! adb into a profile: Android's own adbd on a loopback TCP port of the
//! profile's ([`crate::profile::Settings::debug_bridge_port`]), with adb
//! authentication on (`ro.adb.secure=1`). The user's adb key
//! (`~/.android/adbkey.pub`, which `adb` creates on its first start) is
//! authorized in the profile's data, so a local `adb` connects without a
//! prompt and other users of the host can't.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::Paths;
use crate::profile::Settings;

/// The user's adb public key file.
pub fn user_key_path() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".android/adbkey.pub")
}

/// The user's adb public key: its first line, if it looks like one.
pub fn user_key() -> Option<String> {
    let text = fs::read_to_string(user_key_path()).ok()?;
    let key = text.lines().next()?.trim();
    // Base64 of the key, then a comment (user@host).
    let (b64, _) = key.split_once(' ').unwrap_or((key, ""));
    (b64.len() > 100
        && b64
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)))
    .then(|| key.to_string())
}

/// The file adbd reads authorized keys from, in data directory `data`.
fn keys_file(data: &Path) -> PathBuf {
    data.join("misc/adb/adb_keys")
}

/// Authorizes `key` in data directory `data`, keeping the keys Android
/// itself added. Returns whether it was new.
pub fn authorize(data: &Path, key: &str) -> Result<bool> {
    let file = keys_file(data);
    let old = fs::read_to_string(&file).unwrap_or_default();
    if old.lines().any(|l| l.trim() == key) {
        return Ok(false);
    }
    fs::create_dir_all(file.parent().unwrap())?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .with_context(|| format!("open {}", file.display()))?;
    let sep = if old.is_empty() || old.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    f.write_all(format!("{sep}{key}\n").as_bytes())?;
    Ok(true)
}

/// adbd's listen address for `port`: loopback only.
pub fn listen_addrs(port: u16) -> String {
    format!("tcp:localhost:{port}")
}

/// The properties a boot gets for `settings`: adb authentication always, and
/// adbd on its port when the profile has adb on.
pub fn props(settings: &Settings) -> String {
    let mut s = String::from("ro.adb.secure=1\n");
    if settings.debug_bridge && settings.debug_bridge_port != 0 {
        s += &format!(
            "service.adb.listen_addrs={}\nvendor.linuxapk.adb=1\n",
            listen_addrs(settings.debug_bridge_port)
        );
    } else {
        s += "vendor.linuxapk.adb=0\n";
    }
    s
}

/// At start: the profile's adb properties, after giving adb a port and
/// authorizing the user's key if it is on.
pub fn prepare(paths: &Paths) -> Result<String> {
    let mut settings = Settings::load(&paths.data)?;
    if settings.debug_bridge {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_appended_once_beside_androids_own() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/test/adb-keys");
        let _ = fs::remove_dir_all(&d);
        let key = format!("{} user@host", "A".repeat(700));
        assert!(authorize(&d, &key).unwrap());
        assert!(!authorize(&d, &key).unwrap());
        // A key Android added, without a final newline.
        fs::write(keys_file(&d), "QUJD android\nQkNE other").unwrap();
        assert!(authorize(&d, &key).unwrap());
        assert_eq!(
            fs::read_to_string(keys_file(&d)).unwrap(),
            format!("QUJD android\nQkNE other\n{key}\n")
        );
    }

    #[test]
    fn props_follow_the_setting() {
        let mut s = Settings::default();
        assert_eq!(props(&s), "ro.adb.secure=1\nvendor.linuxapk.adb=0\n");
        s.debug_bridge = true;
        s.debug_bridge_port = 5557;
        let p = props(&s);
        assert!(p.starts_with("ro.adb.secure=1\n"));
        assert!(p.contains("service.adb.listen_addrs=tcp:localhost:5557\n"));
        assert!(p.contains("vendor.linuxapk.adb=1\n"));
    }
}
