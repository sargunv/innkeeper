//! Persistent preferences shared by the launcher and desktop clients.
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Preferences {
    pub follow_dark: bool,
    pub follow_accent: bool,
    pub follow_locale: bool,
    pub follow_clock: bool,
    pub keep_running: bool,
    pub idle_minutes: u32,
    pub mirror_apps: bool,
    pub microg: bool,
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            follow_dark: true,
            follow_accent: true,
            follow_locale: true,
            follow_clock: true,
            keep_running: false,
            idle_minutes: 2,
            mirror_apps: true,
            microg: true,
        }
    }
}

pub fn path() -> Result<PathBuf> {
    Ok(crate::paths::xdg_home("XDG_CONFIG_HOME", ".config")
        .join(crate::paths::per_instance(
            "linuxapk",
            &crate::paths::instance()?,
        ))
        .join("preferences.toml"))
}

impl Preferences {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=120).contains(&self.idle_minutes),
            "Idle timeout must be 1–120 minutes"
        );
        Ok(())
    }

    pub fn load() -> Result<Self> {
        Self::read(&path()?)
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).context("Read preferences"),
        };
        let prefs: Self = toml::from_str(&text).context("Read preferences")?;
        prefs.validate()?;
        Ok(prefs)
    }

    pub fn save(&self) -> Result<()> {
        self.write(&path()?)
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        self.validate()?;
        std::fs::create_dir_all(path.parent().context("Preferences directory")?)?;
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, toml::to_string(self)?)?;
        std::fs::rename(temp, path).context("Save preferences")
    }

    pub fn idle_seconds(&self) -> u64 {
        if self.keep_running {
            0
        } else {
            u64::from(self.idle_minutes) * 60
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_persistence_and_bounds() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/test/preferences");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("preferences.toml");
        let _ = std::fs::remove_file(&path);
        let mut p = Preferences::read(&path).unwrap();
        assert!(
            p.follow_dark && p.follow_accent && p.follow_locale && p.follow_clock && p.mirror_apps
        );
        assert_eq!(p.idle_seconds(), 120);
        p.keep_running = true;
        p.follow_accent = false;
        p.microg = false;
        p.idle_minutes = 7;
        p.write(&path).unwrap();
        assert_eq!(Preferences::read(&path).unwrap(), p);
        assert_eq!(p.idle_seconds(), 0);
        p.idle_minutes = 0;
        assert!(p.write(&path).is_err());
        p.idle_minutes = 121;
        assert!(p.validate().is_err());
        std::fs::write(&path, "follow_dark = false\n").unwrap();
        assert!(!Preferences::read(&path).unwrap().follow_dark);
        std::fs::write(&path, "idle_minutes = 0\n").unwrap();
        assert!(Preferences::read(&path).is_err());
    }
}
