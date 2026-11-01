//! session.conf holds what a session needs. `start` decides it once, and the
//! session and selftest read it.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{Image, Paths};
use crate::profile::System;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// The guest system, from the profile's settings.
    #[serde(default)]
    pub system: System,
    /// Runs without windows, using the composer's headless backend. Tests use
    /// this.
    pub headless: bool,
    /// The only DRM render node Android may open. None selects lavapipe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub render_node: Option<PathBuf>,
    /// The Vulkan HAL matched to render_node at start (radv, anv or lvp).
    pub vulkan_driver: String,
    /// The host compositor's socket, for the bridge's windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wayland: Option<PathBuf>,
    /// `INNKEEPER_BROKER_DEBUG`, passed to the broker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_debug: Option<String>,
    /// Variables for the desktop bridge, set on top of this process's
    /// environment.
    pub bridge_env: BTreeMap<String, String>,
    pub paths: Paths,
    pub image: Image,
}

impl Config {
    pub fn new(
        system: System,
        paths: Paths,
        image: Image,
        headless: bool,
        wayland: Option<PathBuf>,
    ) -> Self {
        let selection = if env::var("INNKEEPER_VULKAN").as_deref() == Ok("lvp") {
            None
        } else {
            crate::graphics::select_node(
                Path::new("/sys/class/drm"),
                Path::new("/dev/dri"),
                &fs::read_to_string(paths.share.join("anv-pci-ids")).unwrap_or_default(),
            )
        };
        let (render_node, vulkan_driver) = selection
            .map(|(node, driver)| (Some(node), driver.to_string()))
            .unwrap_or((None, "lvp".to_string()));
        Config {
            system,
            headless,
            render_node,
            vulkan_driver,
            broker_debug: env::var("INNKEEPER_BROKER_DEBUG")
                .ok()
                .filter(|v| !v.is_empty()),
            bridge_env: bridge_env(wayland.as_deref()),
            wayland,
            paths,
            image,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        toml::from_str(&s).with_context(|| format!("parse {}", path.display()))
    }

    pub fn save(&self) -> Result<()> {
        let p = self.paths.conf();
        fs::write(&p, toml::to_string(self)?).with_context(|| format!("write {}", p.display()))
    }

    /// The start-time layer (see [`crate::layer`]).
    pub fn run_layer(&self) -> PathBuf {
        self.paths.run.join("layer")
    }
}

/// The desktop bridge's environment (host/bridge/src/main.rs), set on top of
/// the session's own, which carries `INNKEEPER_*`. The bridge runs outside
/// Android as the user, and its socket dir is Android's
/// `/dev/socket/innkeeper`. The display service and the clipboard use the host
/// compositor. The system service uses the caller's session bus, and the host
/// state service uses UPower on the system bus. Desktop entries for apps
/// installed inside Android go to the caller's XDG_DATA_HOME. The settings
/// service reads the desktop's locale (plasma-localerc on KDE).
fn bridge_env(wayland: Option<&Path>) -> BTreeMap<String, String> {
    let mut e = BTreeMap::new();
    e.insert(
        "WAYLAND_DISPLAY".into(),
        wayland
            .map(|w| w.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    for v in [
        "DBUS_SESSION_BUS_ADDRESS",
        "DBUS_SYSTEM_BUS_ADDRESS",
        "XDG_DATA_HOME",
        "XDG_CONFIG_HOME",
        "XDG_CURRENT_DESKTOP",
    ] {
        if let Some(d) = env::var(v).ok().filter(|v| !v.is_empty()) {
            e.insert(v.into(), d);
        }
    }
    e
}
