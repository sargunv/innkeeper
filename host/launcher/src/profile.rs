//! Profiles: separate Android setups, each with its own apps, data and
//! settings, that can run side by side. A running profile is a session. Each
//! is named, by `INNKEEPER_PROFILE` or [`crate::paths::select`], and suffixes
//! every path of its own ([`crate::paths`]). None is special: the first is
//! made on demand as [`FIRST`], and can be renamed or deleted like any other.
//!
//! A profile's settings live in one file in its data directory, beside
//! Android's own data and the data version record (docs/session.md), so they
//! move, back up and go with the data. Nothing here is specific to Android:
//! the OpenHarmony guest can keep its profiles the same way.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

/// The settings file in a profile's data directory.
const FILE: &str = ".innkeeper-profile.toml";

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

/// A profile's name, checked by [`check_name`]: never empty, so every
/// per-profile path, bus name and desktop entry carries one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProfileName(String);

impl ProfileName {
    pub fn new(name: &str) -> Result<Self> {
        check_name(name)?;
        Ok(Self(name.into()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for ProfileName {
    type Error = anyhow::Error;
    fn try_from(name: String) -> Result<Self> {
        check_name(&name)?;
        Ok(Self(name))
    }
}

impl From<ProfileName> for String {
    fn from(p: ProfileName) -> String {
        p.0
    }
}

impl PartialEq<str> for ProfileName {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for ProfileName {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl std::ops::Deref for ProfileName {
    type Target = str;
    fn deref(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ProfileName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// The profiles under `data_home` (`$XDG_DATA_HOME/innkeeper`): its
/// `data-<name>` directories, by name.
pub fn list(data_home: &Path) -> Vec<ProfileName> {
    let mut v: Vec<ProfileName> = fs::read_dir(data_home)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                .filter_map(|e| {
                    let n = e.file_name().into_string().ok()?;
                    ProfileName::new(n.strip_prefix("data-")?).ok()
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// Where the profile last chosen is remembered (shared by every profile).
pub fn chosen_file() -> PathBuf {
    crate::paths::xdg_home("XDG_STATE_HOME", ".local/state").join("innkeeper/profile")
}

/// The profile to use when none is named, of `profiles`: the one last
/// chosen, else the first.
pub fn pick<'a>(profiles: &'a [ProfileName], chosen: Option<&str>) -> Option<&'a ProfileName> {
    profiles
        .iter()
        .find(|p| Some(p.as_str()) == chosen)
        .or(profiles.first())
}

/// A profile's settings; a missing key takes its default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Settings {
    /// How the guest shows. Applies at the next start.
    pub display: Display,
    /// Android's adb on loopback TCP. Off by default; it applies live.
    pub adb: bool,
    /// adb's port on 127.0.0.1, the profile's own once it has been on (0:
    /// none yet; any port may be set). It stays, so tools keep finding the
    /// profile there.
    pub adb_port: u16,
    /// Minutes with nothing to do before the session stops, 1 to 120
    /// (unless Keep Running, [`crate::preferences`]). Applies live.
    pub idle_minutes: u32,
    /// Apps installed inside Android get app-menu entries. Applies live.
    pub mirror_apps: bool,
    /// Android follows the desktop's dark mode, accent colour, languages and
    /// formats, and clock. Applies live.
    pub follow_dark: bool,
    pub follow_accent: bool,
    pub follow_locale: bool,
    pub follow_clock: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            display: Display::Windowed,
            adb: false,
            adb_port: 0,
            idle_minutes: 2,
            mirror_apps: true,
            follow_dark: true,
            follow_accent: true,
            follow_locale: true,
            follow_clock: true,
        }
    }
}

/// How a profile shows on the desktop: each app task in a window of its own,
/// or the whole device, its launcher and system bars, in one window the size
/// of a device preset (Android Studio's virtual devices of the same names).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Display {
    #[default]
    Windowed,
    SmallPhone,
    MediumPhone,
    MediumTablet,
}

impl Display {
    pub const ALL: [Display; 4] = [
        Display::Windowed,
        Display::SmallPhone,
        Display::MediumPhone,
        Display::MediumTablet,
    ];

    /// The device window's width and height in pixels, in its natural
    /// orientation, and density; None when windowed.
    pub fn screen(self) -> Option<(u32, u32, u32)> {
        match self {
            Display::Windowed => None,
            Display::SmallPhone => Some((720, 1280, 320)),
            Display::MediumPhone => Some((1080, 2400, 420)),
            Display::MediumTablet => Some((2560, 1600, 320)),
        }
    }

    /// Its name in settings and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Display::Windowed => "windowed",
            Display::SmallPhone => "small-phone",
            Display::MediumPhone => "medium-phone",
            Display::MediumTablet => "medium-tablet",
        }
    }

    pub fn parse(name: &str) -> Option<Display> {
        Self::ALL.into_iter().find(|s| s.name() == name)
    }

    /// Whether Android lays the device out as a large screen (600 dp or more
    /// on its short side), with a taskbar.
    pub fn large(self) -> bool {
        self.screen()
            .is_some_and(|(w, h, dpi)| w.min(h) * 160 / dpi >= 600)
    }
}

/// Where adb ports come from: the Android Emulator's adb ports, the
/// odd ones from 5555 to 5585, each with its console port one below. An adb
/// server looks for devices there, so it finds a profile as
/// `emulator-<port - 1>` with no `adb connect`, and so do Android Studio and
/// Gradle (whose test engine fails for `host:port` serials).
pub fn ports() -> impl Iterator<Item = u16> {
    (5555..=5585).step_by(2)
}

/// The first adb port in [`ports`] that no other profile has (`taken`) and
/// whose pair `free` accepts (a running emulator holds both), as the
/// emulator picks its own.
pub fn pick_port(taken: &[u16], free: impl Fn(u16) -> bool) -> Option<u16> {
    ports().find(|p| !taken.contains(p) && free(*p) && free(*p - 1))
}

/// Whether nothing listens on 127.0.0.1:`port`.
pub fn port_free(port: u16) -> bool {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
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
        let s: Self = toml::from_str(text)?;
        s.validate()?;
        Ok(s)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=120).contains(&self.idle_minutes),
            "the idle timeout is 1 to 120 minutes"
        );
        Ok(())
    }

    /// Seconds with nothing to do before the session stops; 0: never.
    pub fn idle_seconds(&self, keep_running: bool) -> u64 {
        if keep_running {
            0
        } else {
            u64::from(self.idle_minutes) * 60
        }
    }

    /// Gives adb of the profile with data directory `data` a port of its
    /// own, if it has none: one no other profile under `data_home` has and
    /// nothing listens on. Returns whether it changed.
    pub fn assign_port(&mut self, data_home: &Path, data: &Path) -> Result<bool> {
        if self.adb_port != 0 {
            return Ok(false);
        }
        let taken: Vec<u16> = list(data_home)
            .iter()
            .map(|n| data_home.join(crate::paths::per_profile("data", n)))
            .filter(|d| d != data)
            .filter_map(|d| Self::load(&d).ok())
            .map(|s| s.adb_port)
            .collect();
        self.adb_port = pick_port(&taken, port_free).context("no free adb port")?;
        Ok(true)
    }

    /// Saves the settings into data directory `data`, creating it for a new
    /// profile.
    pub fn save(&self, data: &Path) -> Result<()> {
        self.validate()?;
        fs::create_dir_all(data).with_context(|| format!("create {}", data.display()))?;
        let path = settings_path(data);
        let temp = path.with_extension("tmp");
        fs::write(&temp, toml::to_string(self)?)?;
        fs::rename(&temp, &path).with_context(|| format!("save {}", path.display()))
    }
}

fn ensure_stopped(paths: &crate::Paths) -> Result<()> {
    ensure!(
        crate::status(paths).pid.is_none(),
        "profile {} is running; stop it first",
        paths.profile
    );
    Ok(())
}

/// Renames profile `from.profile` to `to.profile`: its data, settings and
/// logs move; its composed root, a cache, is dropped. It
/// must be stopped.
pub fn rename(from: &crate::Paths, to: &crate::Paths) -> Result<()> {
    ensure_stopped(from)?;
    ensure!(from.data.is_dir(), "no profile {}", from.profile);
    ensure!(!to.data.exists(), "profile {} exists already", to.profile);
    for (a, b) in [(&from.data, &to.data), (&from.state, &to.state)] {
        if a.exists() {
            crate::sandbox::remove_tree(b)?;
            fs::rename(a, b).with_context(|| format!("move {} to {}", a.display(), b.display()))?;
        }
    }
    crate::sandbox::remove_tree(&from.root)?;
    crate::sandbox::remove_tree(&from.run)
}

/// Deletes profile `paths.profile`'s files: its data, settings, composed
/// root, logs and runtime files. It must be stopped.
pub fn delete(paths: &crate::Paths) -> Result<()> {
    ensure_stopped(paths)?;
    for dir in [&paths.data, &paths.root, &paths.state, &paths.run] {
        crate::sandbox::remove_tree(dir)?;
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
        let v = |s: &[&str]| {
            s.iter()
                .map(|s| ProfileName::new(s).unwrap())
                .collect::<Vec<_>>()
        };
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
    fn settings_default_round_trip_and_bounds() {
        let d = dir("settings");
        assert_eq!(Settings::load(&d).unwrap(), Settings::default());
        let data = d.join("data-new");
        let mut s = Settings {
            display: Display::SmallPhone,
            follow_accent: false,
            idle_minutes: 7,
            ..Settings::default()
        };
        s.save(&data).unwrap();
        assert_eq!(Settings::load(&data).unwrap(), s);
        assert_eq!(s.idle_seconds(false), 420);
        assert_eq!(s.idle_seconds(true), 0);
        s.idle_minutes = 0;
        assert!(s.save(&data).is_err());
        assert_eq!(Settings::parse("").unwrap(), Settings::default());
        let s =
            Settings::parse("adb = true\nadb-port = 16501\ndisplay = \"medium-tablet\"\n").unwrap();
        assert!(s.adb && s.adb_port == 16501 && s.display == Display::MediumTablet);
        for bad in [
            "typo = true",
            "display = \"tv\"",
            "idle-minutes = 121",
            "follow-dark = \"yes\"",
        ] {
            assert!(Settings::parse(bad).is_err(), "{bad}");
        }
        fs::write(settings_path(&data), "display = [").unwrap();
        assert!(Settings::load(&data).is_err());
    }

    #[test]
    fn display_presets() {
        for s in Display::ALL {
            assert_eq!(Display::parse(s.name()), Some(s));
        }
        assert_eq!(Display::parse("huge"), None);
        assert_eq!(Display::Windowed.screen(), None);
        assert!(!Display::SmallPhone.large() && !Display::MediumPhone.large());
        assert!(Display::MediumTablet.large() && !Display::Windowed.large());
    }

    #[test]
    fn ports_are_the_emulators_and_skip_busy_pairs_and_other_profiles() {
        let all_free = |_| true;
        assert_eq!(pick_port(&[], all_free), Some(5555));
        // A running emulator holds 5554 and 5555; one holding only its
        // console port still blocks the pair.
        let emulator = |p: u16| p != 5554 && p != 5555;
        assert_eq!(pick_port(&[], emulator), Some(5557));
        assert_eq!(pick_port(&[], |p| p != 5554), Some(5557));
        assert_eq!(pick_port(&[5557], emulator), Some(5559));
        assert_eq!(pick_port(&ports().collect::<Vec<_>>(), all_free), None);
        assert_eq!(ports().count(), 16);
        let d = dir("ports");
        let other = d.join("data-other");
        Settings {
            adb_port: 5555,
            ..Settings::default()
        }
        .save(&other)
        .unwrap();
        // A profile keeps its own port, an explicit one too; a new one gets
        // a free pair.
        let mut s = Settings::default();
        assert!(s.assign_port(&d, &d.join("data-new")).unwrap());
        assert!(s.adb_port != 5555 && ports().any(|p| p == s.adb_port));
        let mut again = Settings::load(&other).unwrap();
        assert!(!again.assign_port(&d, &other).unwrap());
        assert_eq!(again.adb_port, 5555);
        let mut explicit = Settings {
            adb_port: 16500,
            ..Settings::default()
        };
        assert!(!explicit.assign_port(&d, &d.join("data-x")).unwrap());
        assert_eq!(explicit.adb_port, 16500);
    }

    #[test]
    fn rename_moves_and_delete_removes_a_stopped_profiles_files() {
        let d = dir("delete");
        let p = |name: &str| crate::Paths {
            profile: ProfileName::new(name).unwrap(),
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
