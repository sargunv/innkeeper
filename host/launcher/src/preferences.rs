//! Innkeeper's own preferences, shared by every profile:
//! `$XDG_CONFIG_HOME/innkeeper/preferences.toml`. A profile's settings are
//! its own ([`crate::profile::Settings`]).
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields, rename_all = "kebab-case")]
pub struct Preferences {
    /// No profile stops when idle, and every profile starts at login (the
    /// Background portal holds one login command for the app).
    pub keep_running: bool,
}

pub fn path() -> PathBuf {
    crate::paths::xdg_home("XDG_CONFIG_HOME", ".config").join("innkeeper/preferences.toml")
}

impl Preferences {
    pub fn load() -> Result<Self> {
        Self::read(&path())
    }

    pub fn read(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(e).context("read preferences"),
        };
        toml::from_str(&text).with_context(|| format!("preferences {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        self.write(&path())
    }

    pub fn write(&self, path: &Path) -> Result<()> {
        std::fs::create_dir_all(path.parent().context("preferences directory")?)?;
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, toml::to_string(self)?)?;
        std::fs::rename(temp, path).context("save preferences")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_persistence() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/test/preferences");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("preferences.toml");
        let _ = std::fs::remove_file(&path);
        assert!(!Preferences::read(&path).unwrap().keep_running);
        let p = Preferences { keep_running: true };
        p.write(&path).unwrap();
        assert_eq!(Preferences::read(&path).unwrap(), p);
        std::fs::write(&path, "idle-minutes = 3\n").unwrap();
        assert!(Preferences::read(&path).is_err());
    }
}
