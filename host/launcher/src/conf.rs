//! session.conf: everything a session needs, decided once by `start` and read
//! by the session and selftest.

use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths::{Image, Paths};
use crate::sys;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// No windows (tests): the composer's headless backend.
    pub headless: bool,
    /// The host compositor's socket, for the bridge's windows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wayland: Option<PathBuf>,
    /// Seconds without activity before the session stops itself; 0: never
    /// ([`crate::lifecycle`]).
    #[serde(default)]
    pub idle_grace: u64,
    /// The environment a wakeup starts the session again with.
    #[serde(default)]
    pub start_env: BTreeMap<String, String>,
    /// `LINUXAPK_BROKER_DEBUG`, passed to the broker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub broker_debug: Option<String>,
    /// The desktop bridge's arguments and environment.
    pub bridge_args: Vec<String>,
    pub bridge_env: BTreeMap<String, String>,
    pub paths: Paths,
    pub image: Image,
}

impl Config {
    pub fn new(paths: Paths, image: Image, headless: bool, wayland: Option<PathBuf>) -> Self {
        let mut c = Config {
            headless,
            wayland,
            idle_grace: crate::lifecycle::grace_secs(headless),
            start_env: crate::lifecycle::start_env(),
            broker_debug: env::var("LINUXAPK_BROKER_DEBUG")
                .ok()
                .filter(|v| !v.is_empty()),
            bridge_args: vec![],
            bridge_env: BTreeMap::new(),
            paths,
            image,
        };
        c.bridge_setup();
        c
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

    /// The desktop bridge's arguments and environment
    /// (host/bridge/README.md). It runs outside Android, as the user. Its
    /// socket dir is Android's `/dev/socket/linuxapk`, and it only accepts
    /// Android root, system and audioserver (SO_PEERCRED, as host ids), plus
    /// the system app's uid (the owner of its data dir).
    fn bridge_setup(&mut self) {
        let p = &self.paths;
        let a = &mut self.bridge_args;
        for (k, v) in [
            ("--run-dir", p.run.clone()),
            ("--state-dir", p.state.clone()),
            ("--system-app-dir", p.data.join("data/dev.linuxapk.system")),
        ] {
            a.push(k.into());
            a.push(v.to_string_lossy().into_owned());
        }
        a.push("--allow-uid".into());
        a.push(sys::uid().to_string());
        if let Some(sink) = env::var("LINUXAPK_AUDIO_SINK")
            .ok()
            .filter(|v| !v.is_empty())
        {
            a.push("--sink".into());
            a.push(sink);
        }
        if env::var("LINUXAPK_BRIDGE_VERBOSE").is_ok_and(|v| v == "1") {
            a.push("-v".into());
        }
        let bus = env::var("DBUS_SESSION_BUS_ADDRESS")
            .ok()
            .filter(|v| !v.is_empty());
        if !p.instance.is_empty() || env::var("LINUXAPK_TEST").is_ok_and(|v| v == "1") {
            // Persist isolation in session.conf, including across wakeups.
            a.push("--test-session".into());
        } else {
            a.push("--mirror-apps".into());
        }
        // The display service and the clipboard use the host compositor; the
        // system service the caller's session bus; desktop entries for apps
        // installed inside Android go to the caller's XDG_DATA_HOME.
        let e = &mut self.bridge_env;
        e.insert(
            "WAYLAND_DISPLAY".into(),
            self.wayland
                .as_ref()
                .map(|w| w.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        if let Some(b) = bus {
            e.insert("DBUS_SESSION_BUS_ADDRESS".into(), b);
        }
        // The host state service reads UPower on the caller's system bus,
        // and the settings service reads the desktop's locale (plasma-localerc
        // on KDE).
        for v in [
            "XDG_DATA_HOME",
            "XDG_CONFIG_HOME",
            "XDG_CURRENT_DESKTOP",
            "DBUS_SYSTEM_BUS_ADDRESS",
            "LINUXAPK_GAMEPAD_TEST_DIR",
            "LINUXAPK_BENCH",
        ] {
            if let Some(d) = env::var(v).ok().filter(|v| !v.is_empty()) {
                e.insert(v.into(), d);
            }
        }
    }
}
