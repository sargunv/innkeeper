//! Profiles: separate Android setups, each with its own apps, data and
//! settings, that can run side by side. A running profile is a session. Each
//! is named, by `LINUXAPK_PROFILE` or [`crate::paths::select`], and suffixes
//! every path of its own ([`crate::paths`]). None is special: the first is
//! made on demand as [`FIRST`], and can be renamed or deleted like any other.
//!
//! A profile's settings live in its data directory, beside Android's own data
//! and the data version record (docs/session.md), so they move, back up and
//! go with the data. Nothing here is specific to Android: the OpenHarmony
//! guest can keep its profiles the same way.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

/// The settings file in a profile's data directory.
const FILE: &str = ".linuxapk-profile.toml";

/// The settings format this build reads and writes. A newer one is refused
/// rather than rewritten without the settings it doesn't know.
pub const VERSION: u32 = 1;

/// The name of the profile made when something needs one and there is none.
pub const FIRST: &str = "default";

/// The longest profile name.
const MAX_NAME: usize = 32;

/// Checks a profile name: lowercase ASCII letters, digits and inner dashes,
/// as every per-profile path carries it.
pub fn check_name(name: &str) -> Result<()> {
    ensure!(!name.is_empty(), "no profile selected");
    ensure!(
        name.len() <= MAX_NAME,
        "profile names have at most {MAX_NAME} characters"
    );
    ensure!(
        name.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && !name.starts_with('-')
            && !name.ends_with('-'),
        "profile names use lowercase letters, digits and dashes: {name:?}"
    );
    Ok(())
}

/// The profiles under `data_home` (`$XDG_DATA_HOME/linuxapk`): its
/// `data-<name>` directories, by name.
pub fn list(data_home: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(data_home)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| {
                    let n = e.file_name().into_string().ok()?;
                    let name = n.strip_prefix("data-")?.to_string();
                    check_name(&name).is_ok().then_some(name)
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Where the profile last chosen is remembered (shared by every profile).
pub fn chosen_file() -> PathBuf {
    crate::paths::xdg_home("XDG_STATE_HOME", ".local/state").join("linuxapk/profile")
}

/// The profile to use when none is named, of `profiles`: the one last
/// chosen, else the first.
pub fn pick<'a>(profiles: &'a [String], chosen: Option<&str>) -> Option<&'a String> {
    profiles
        .iter()
        .find(|p| Some(p.as_str()) == chosen)
        .or(profiles.first())
}

/// A profile's settings. Every default is what a profile did before it had
/// settings, so existing data keeps working unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Settings {
    pub version: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { version: VERSION }
    }
}

/// The settings file of the profile whose data directory is `data`.
pub fn settings_path(data: &Path) -> PathBuf {
    data.join(FILE)
}

impl Settings {
    /// The settings in data directory `data`; the defaults if it has none.
    pub fn load(data: &Path) -> Result<Self> {
        let path = settings_path(data);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).with_context(|| format!("read {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("profile settings {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        // The version first: a newer file may have settings this build
        // doesn't know.
        let table: toml::Table = toml::from_str(text)?;
        let version = match table.get("version") {
            None => VERSION,
            Some(v) => v
                .as_integer()
                .and_then(|v| u32::try_from(v).ok())
                .context("version is not a number")?,
        };
        if version > VERSION {
            bail!("made by a newer Linux.apk (version {version}; this one reads {VERSION})");
        }
        let mut s: Self = toml::from_str(text)?;
        s.version = VERSION;
        Ok(s)
    }

    /// Saves the settings into data directory `data`, creating it for a new
    /// profile.
    pub fn save(&self, data: &Path) -> Result<()> {
        fs::create_dir_all(data).with_context(|| format!("create {}", data.display()))?;
        let path = settings_path(data);
        let temp = path.with_extension("tmp");
        fs::write(&temp, toml::to_string(self)?)?;
        fs::rename(&temp, &path).with_context(|| format!("save {}", path.display()))
    }
}

/// Profile `name`'s desktop preferences directory.
fn config_dir(name: &str) -> PathBuf {
    crate::paths::xdg_home("XDG_CONFIG_HOME", ".config")
        .join(crate::paths::per_profile("linuxapk", name))
}

fn ensure_stopped(paths: &crate::Paths) -> Result<()> {
    ensure!(
        crate::status(paths).pid.is_none(),
        "profile {} is running; stop it first",
        paths.profile
    );
    Ok(())
}

/// Renames profile `from.profile` to `to.profile`: its data, settings, logs
/// and desktop preferences move; its composed root, a cache, is dropped. It
/// must be stopped.
pub fn rename(from: &crate::Paths, to: &crate::Paths) -> Result<()> {
    ensure_stopped(from)?;
    ensure!(from.data.is_dir(), "no profile {}", from.profile);
    ensure!(!to.data.exists(), "profile {} exists already", to.profile);
    for (a, b) in [
        (&from.data, &to.data),
        (&from.state, &to.state),
        (&config_dir(&from.profile), &config_dir(&to.profile)),
    ] {
        if a.exists() {
            crate::flatpak::remove_tree(b)?;
            fs::rename(a, b).with_context(|| format!("move {} to {}", a.display(), b.display()))?;
        }
    }
    crate::flatpak::remove_tree(&from.root)?;
    crate::flatpak::remove_tree(&from.run)
}

/// Deletes profile `paths.profile`'s files: its data, settings, composed
/// root, logs, runtime files and desktop preferences. It must be stopped.
pub fn delete(paths: &crate::Paths) -> Result<()> {
    ensure_stopped(paths)?;
    for dir in [
        &paths.data,
        &paths.root,
        &paths.state,
        &paths.run,
        &config_dir(&paths.profile),
    ] {
        crate::flatpak::remove_tree(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn dir(name: &str) -> PathBuf {
        let d = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../out/test/profile")
            .join(name);
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn names() {
        for ok in ["work", "a-1", "x9"] {
            check_name(ok).unwrap();
        }
        for bad in ["", "Work", "a_b", "-a", "a-", "a/b", "..", &"a".repeat(33)] {
            assert!(check_name(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn picks_the_profile_last_chosen_else_the_first() {
        let v = |s: &[&str]| s.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(pick(&[], Some("a")), None);
        assert_eq!(pick(&v(&["a", "b"]), Some("b")).unwrap(), "b");
        assert_eq!(pick(&v(&["a", "b"]), Some("gone")).unwrap(), "a");
        assert_eq!(pick(&v(&["a", "b"]), None).unwrap(), "a");
    }

    #[test]
    fn lists_named_data_dirs() {
        let d = dir("list");
        assert!(list(&d.join("missing")).is_empty());
        for n in [
            "data",
            "data-work",
            "data-a",
            "data-Bad",
            "data-",
            "root-x",
            "images",
        ] {
            fs::create_dir(d.join(n)).unwrap();
        }
        fs::write(d.join("data-file"), "").unwrap();
        assert_eq!(list(&d), ["a", "work"]);
    }

    #[test]
    fn settings_default_round_trip_and_versions() {
        let d = dir("settings");
        // No file: today's behaviour.
        assert_eq!(Settings::load(&d).unwrap(), Settings::default());
        let data = d.join("data-new");
        Settings::default().save(&data).unwrap();
        assert_eq!(Settings::load(&data).unwrap(), Settings::default());
        assert!(
            fs::read_to_string(settings_path(&data))
                .unwrap()
                .contains("version = 1")
        );
        // No version: the first one.
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
        assert!(
            Settings::parse("version = 2\nnew-thing = 1\n")
                .unwrap_err()
                .to_string()
                .contains("newer")
        );
        assert!(Settings::parse("version = \"one\"").is_err());
        assert!(Settings::parse("version = 1\ntypo = true\n").is_err());
        fs::write(settings_path(&data), "version = [").unwrap();
        assert!(Settings::load(&data).is_err());
    }

    #[test]
    fn rename_moves_and_delete_removes_a_stopped_profiles_files() {
        let d = dir("delete");
        let p = |name: &str| crate::Paths {
            profile: name.into(),
            libexec: d.join("libexec"),
            share: d.join("share"),
            run: d.join(format!("run-{name}")),
            state: d.join(format!("state-{name}")),
            data_home: d.clone(),
            data: d.join(format!("data-{name}")),
            images: d.join("images"),
            pin: d.join("pin.toml"),
            root: d.join(format!("root-{name}")),
        };
        let (old, gone) = (p("unit-test-old"), p("unit-test-gone"));
        Settings::default().save(&old.data).unwrap();
        fs::create_dir_all(old.root.join("system")).unwrap();
        fs::write(old.root.join("system/f"), "").unwrap();
        // The composed root is read-only.
        fs::set_permissions(old.root.join("system"), fs::Permissions::from_mode(0o555)).unwrap();
        fs::create_dir_all(&old.state).unwrap();
        fs::create_dir_all(d.join("images")).unwrap();
        rename(&old, &gone).unwrap();
        assert!(!old.data.exists() && !old.root.exists() && !old.state.exists());
        assert!(settings_path(&gone.data).is_file() && gone.state.is_dir());
        assert!(rename(&old, &gone).is_err());
        Settings::default().save(&old.data).unwrap();
        assert!(
            rename(&old, &gone)
                .unwrap_err()
                .to_string()
                .contains("exists")
        );
        fs::create_dir_all(gone.root.join("system")).unwrap();
        fs::set_permissions(gone.root.join("system"), fs::Permissions::from_mode(0o555)).unwrap();
        delete(&gone).unwrap();
        for dir in [&gone.data, &gone.root, &gone.state] {
            assert!(!dir.exists(), "{}", dir.display());
        }
        assert!(d.join("images").is_dir());
    }
}
