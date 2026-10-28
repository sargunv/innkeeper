//! Where a session's files are, from the prefix the launcher runs from and the
//! environment (docs/session.md, "Where things live").

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::sys;

/// The Flatpak app id, when `FLATPAK_ID` doesn't say.
const APP_ID: &str = "dev.linuxapk.Manager";

/// Where the session runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Sandbox {
    /// Rootless namespaces in a systemd user unit.
    Host,
    /// Inside Linux.apk's own Flatpak sandbox, which allows no namespaces or
    /// mounts (docs/session.md, "Flatpak mode").
    Flatpak,
}

impl Sandbox {
    pub fn detect() -> Self {
        if Path::new("/.flatpak-info").exists() {
            Sandbox::Flatpak
        } else {
            Sandbox::Host
        }
    }
}

/// `LINUXAPK_INSTANCE` if it is a valid instance name, else an error. Empty
/// for the main session.
pub fn instance() -> Result<String> {
    let inst = env::var("LINUXAPK_INSTANCE").unwrap_or_default();
    if !inst
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("bad LINUXAPK_INSTANCE");
    }
    Ok(inst)
}

/// `name`, suffixed with `-<instance>` for a named instance: the session's
/// systemd unit and its directories are named this way.
pub fn per_instance(name: &str, instance: &str) -> String {
    if instance.is_empty() {
        name.into()
    } else {
        format!("{name}-{instance}")
    }
}

/// An XDG base directory: `$var` if it is absolute, else `default` under
/// `$HOME`.
pub fn xdg_home(var: &str, default: &str) -> PathBuf {
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            Path::new(&env::var_os("HOME").unwrap_or_else(|| "/".into())).join(default)
        })
}

/// `$var` as a path, if set and not empty.
fn env_path(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// A session's paths. Every per-instance one carries the instance suffix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paths {
    /// Empty for the main session.
    pub instance: String,
    pub sandbox: Sandbox,
    /// `<prefix>/libexec/linuxapk`: pivot-exec, the bridge, the image scripts.
    pub libexec: PathBuf,
    /// `<prefix>/share/linuxapk`: the Android tree, the image config.
    pub share: PathBuf,
    /// `$XDG_RUNTIME_DIR/linuxapk[-<instance>]` (in Flatpak mode, under the
    /// app's dir, which every instance of the app shares): session.conf, pid
    /// files, the kmsg FIFO, the bridge's socket dir, the start-time layer.
    pub run: PathBuf,
    /// `$XDG_STATE_HOME/linuxapk[-<instance>]`: logs, the documents grants.
    pub state: PathBuf,
    /// `$XDG_DATA_HOME/linuxapk`.
    pub data_home: PathBuf,
    /// Android's `/data` (`LINUXAPK_DATA`, default `data[-<instance>]`).
    pub data: PathBuf,
    /// The image cache (`LINUXAPK_IMAGES`, default `images`).
    pub images: PathBuf,
    /// The image pin (`LINUXAPK_PIN`, default the prefix's).
    pub pin: PathBuf,
    /// Flatpak mode's composed root: `root[-<instance>]`.
    pub root: PathBuf,
    /// The systemd user unit (host mode).
    pub unit: String,
}

impl Paths {
    /// The paths for the prefix at `prefix` and this process's environment.
    pub fn new(prefix: &Path) -> Result<Self> {
        let share = prefix.join("share/linuxapk");
        if !share.join("manifest.tsv").is_file() {
            bail!(
                "no staged build at {} (run `mise run build`)",
                prefix.display()
            );
        }
        let instance = instance()?;
        let sandbox = Sandbox::detect();
        let sfx = |n: &str| per_instance(n, &instance);
        let runtime = env_path("XDG_RUNTIME_DIR")
            .unwrap_or_else(|| format!("/run/user/{}", sys::uid()).into());
        let run = match sandbox {
            Sandbox::Host => runtime.join(sfx("linuxapk")),
            // The sandbox's own runtime dir is private to each instance; the
            // app's dir in it is shared by all of them.
            Sandbox::Flatpak => runtime
                .join("app")
                .join(env::var("FLATPAK_ID").unwrap_or_else(|_| APP_ID.into()))
                .join(sfx("linuxapk")),
        };
        let data_home = xdg_home("XDG_DATA_HOME", ".local/share").join("linuxapk");
        Ok(Paths {
            libexec: prefix.join("libexec/linuxapk"),
            run,
            state: xdg_home("XDG_STATE_HOME", ".local/state").join(sfx("linuxapk")),
            data: env_path("LINUXAPK_DATA").unwrap_or_else(|| data_home.join(sfx("data"))),
            images: env_path("LINUXAPK_IMAGES").unwrap_or_else(|| data_home.join("images")),
            pin: env_path("LINUXAPK_PIN").unwrap_or_else(|| share.join("image/pin.toml")),
            root: data_home.join(sfx("root")),
            unit: sfx("linuxapk-session"),
            data_home,
            share,
            instance,
            sandbox,
        })
    }

    /// The paths of the prefix this executable is installed in
    /// (`<prefix>/libexec/linuxapk/linuxapk-session`).
    pub fn of_this_exe() -> Result<Self> {
        let exe = env::current_exe()
            .and_then(|p| p.canonicalize())
            .context("can't find this program's path")?;
        let prefix = exe
            .ancestors()
            .nth(3)
            .context("the launcher is not in <prefix>/libexec/linuxapk")?;
        Self::new(prefix)
    }

    pub fn conf(&self) -> PathBuf {
        self.run.join("session.conf")
    }

    /// Android init's pid.
    pub fn pid_file(&self) -> PathBuf {
        self.run.join("pid")
    }

    /// The process init is a child of: pid 1 of the outer namespaces (host
    /// mode), or pivot-exec (Flatpak mode).
    pub fn outer_pid_file(&self) -> PathBuf {
        self.run.join("outer.pid")
    }

    /// Flatpak mode's control socket, which other sandbox instances (the CLI,
    /// the manager) reach the session through.
    pub fn control(&self) -> PathBuf {
        self.run.join("control.sock")
    }

    pub fn kmsg_log(&self) -> PathBuf {
        self.state.join("kmsg.log")
    }

    pub fn session_log(&self) -> PathBuf {
        self.state.join("session.log")
    }

    pub fn bridge_log(&self) -> PathBuf {
        self.state.join("bridge.log")
    }

    /// The staged Android tree (overlay, HALs, drivers, system app).
    pub fn android(&self) -> PathBuf {
        self.share.join("android")
    }

    /// Addon layers over the partitions, one dir each (e.g. microG).
    pub fn addons(&self) -> PathBuf {
        self.data_home.join("addons")
    }

    /// The addon layers, in name order.
    pub fn addon_layers(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = fs::read_dir(self.addons())
            .map(|d| {
                d.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    /// The extra `androidboot.*` values (image/bootconfig), one token each.
    pub fn image_bootconfig(&self) -> Result<Vec<String>> {
        let p = self.share.join("image/bootconfig");
        let text = fs::read_to_string(&p).with_context(|| format!("read {}", p.display()))?;
        Ok(bootconfig_tokens(&text))
    }

    /// The pinned image and where its files go.
    pub fn image(&self) -> Result<Image> {
        let pin = fs::read_to_string(&self.pin).unwrap_or_default();
        let name =
            pin_name(&pin).with_context(|| format!("no image name in {}", self.pin.display()))?;
        let dir = self.images.join(&name);
        Ok(Image {
            img: dir.join("img"),
            apexdir: dir.join(format!(
                "apex-{}",
                apex_selection_hash(&self.image_bootconfig()?)
            )),
            derived: dir.join("derived"),
            dir,
            name,
        })
    }
}

/// The pinned AOSP CI build's files: its extracted partitions (`img`), its
/// flattened APEXes (`apexdir`, one per vendor APEX selection, shared by
/// every instance and never rewritten under a running session) and the
/// layers derived from them (image/README.md).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Image {
    pub name: String,
    pub dir: PathBuf,
    pub img: PathBuf,
    pub apexdir: PathBuf,
    pub derived: PathBuf,
}

/// The pin's top-level name: the keys before the first `[table]` header (the
/// `[[artifact]]` tables have names of their own).
fn pin_name(pin: &str) -> Option<String> {
    for line in pin.lines() {
        let l = line.trim_start();
        if l.starts_with('[') {
            return None;
        }
        let Some(rest) = l.strip_prefix("name") else {
            continue;
        };
        if !rest.trim_start().starts_with('=') {
            continue;
        }
        let v = rest.split('"').nth(1)?;
        return (!v.is_empty()).then(|| v.to_string());
    }
    None
}

/// image/bootconfig's `androidboot.*=value` tokens: whitespace-separated,
/// with `#` comments.
fn bootconfig_tokens(text: &str) -> Vec<String> {
    text.lines()
        .flat_map(|l| l.split('#').next().unwrap_or("").split_whitespace())
        .map(str::to_string)
        .collect()
}

/// The flattened APEX tree's suffix: a hash of the vendor APEX selection
/// (`androidboot.vendor.apex.<name>=<file>|none`, which apexd and
/// image/flatten-apex.sh both honour).
fn apex_selection_hash(bootconfig: &[String]) -> String {
    let mut h = Sha256::new();
    for t in bootconfig
        .iter()
        .filter(|t| t.starts_with("androidboot.vendor.apex."))
    {
        h.update(t.as_bytes());
        h.update(b"\n");
    }
    h.finalize()
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_names() {
        let pin = "# c\nname = \"ci-123\"\nurl = \"x\"\n\n[[artifact]]\nname = \"a.zip\"\n";
        assert_eq!(pin_name(pin).as_deref(), Some("ci-123"));
        assert_eq!(pin_name("[[artifact]]\nname = \"a.zip\"\n"), None);
        assert_eq!(
            pin_name("names = \"x\"\nname=\"y\"\n").as_deref(),
            Some("y")
        );
        assert_eq!(pin_name(""), None);
    }

    #[test]
    fn bootconfig() {
        let t = bootconfig_tokens("# c\na=1 b=2 # c=3\n\n  androidboot.vendor.apex.x=none\n");
        assert_eq!(t, ["a=1", "b=2", "androidboot.vendor.apex.x=none"]);
        // What `grep '^androidboot\.vendor\.apex\.' | sha256sum | cut -c1-12`
        // printed, so existing trees are reused.
        assert_eq!(apex_selection_hash(&[]), "e3b0c44298fc");
        assert_eq!(apex_selection_hash(&t), {
            let mut h = Sha256::new();
            h.update(b"androidboot.vendor.apex.x=none\n");
            h.finalize()
                .iter()
                .take(6)
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        });
    }

    #[test]
    fn instance_suffix() {
        assert_eq!(per_instance("linuxapk", ""), "linuxapk");
        assert_eq!(per_instance("linuxapk", "e2e"), "linuxapk-e2e");
    }
}
