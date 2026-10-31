//! Where a session's files are, from the prefix the launcher runs from and the
//! environment (docs/session.md, "Where things live").

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::profile::ProfileName;
use crate::sys;

/// The Flatpak app id, when `FLATPAK_ID` doesn't say.
const APP_ID: &str = "dev.sargunv.Innkeeper";

/// The profile [`select`]ed in this process, if any.
static SELECTED: std::sync::Mutex<Option<ProfileName>> = std::sync::Mutex::new(None);

/// Selects profile `name` for this process, over `INNKEEPER_PROFILE` (the
/// manager switches profiles so). Launchers it runs get it in their
/// environment.
pub fn select(name: &str) -> Result<()> {
    *SELECTED.lock().unwrap() = Some(ProfileName::new(name)?);
    Ok(())
}

/// The profile this process works with: the one [`select`]ed, else
/// `INNKEEPER_PROFILE`, which must name one.
pub fn profile() -> Result<ProfileName> {
    if let Some(s) = SELECTED.lock().unwrap().clone() {
        return Ok(s);
    }
    ProfileName::new(&env::var("INNKEEPER_PROFILE").unwrap_or_default())
        .context("INNKEEPER_PROFILE")
}

/// `name-<profile>`: the session's directories are named this way.
pub fn per_profile(name: &str, profile: &ProfileName) -> String {
    format!("{name}-{profile}")
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

/// `$XDG_DATA_HOME/innkeeper`: the profiles' data, the image cache, addons.
pub fn data_home() -> PathBuf {
    xdg_home("XDG_DATA_HOME", ".local/share").join("innkeeper")
}

/// `$var` as a path, if set and not empty.
fn env_path(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// A session's paths. Every per-profile one carries the profile suffix.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Paths {
    /// Serialized as `profile` in session.conf.
    pub profile: ProfileName,
    /// `<prefix>/libexec/innkeeper`: pivot-exec, the bridge, the image scripts.
    pub libexec: PathBuf,
    /// `<prefix>/share/innkeeper`: the Android tree, the image config.
    pub share: PathBuf,
    /// `$XDG_RUNTIME_DIR/app/<app id>/innkeeper-<profile>`, which every
    /// instance of the app shares: session.conf, pid
    /// files, the kmsg FIFO, the bridge's socket dir, the start-time layer.
    pub run: PathBuf,
    /// `$XDG_STATE_HOME/innkeeper-<profile>`: logs, the documents grants.
    pub state: PathBuf,
    /// `$XDG_DATA_HOME/innkeeper`.
    pub data_home: PathBuf,
    /// Android's `/data` (`INNKEEPER_DATA`, default `data-<profile>`).
    pub data: PathBuf,
    /// The image cache (`INNKEEPER_IMAGES`, default `images`).
    pub images: PathBuf,
    /// The image pin (`INNKEEPER_PIN`, default the prefix's).
    pub pin: PathBuf,
    /// The composed root: `root-<profile>`.
    pub root: PathBuf,
}

impl Paths {
    /// The paths for the prefix at `prefix` and this process's environment.
    pub fn new(prefix: &Path) -> Result<Self> {
        Self::for_profile(prefix, &profile()?)
    }

    /// The paths of profile `name` for the prefix at `prefix`.
    pub fn for_profile(prefix: &Path, name: &str) -> Result<Self> {
        let share = prefix.join("share/innkeeper");
        if !share.join("manifest.tsv").is_file() {
            bail!(
                "no staged build at {} (run `mise run build`)",
                prefix.display()
            );
        }
        let profile = ProfileName::new(name)?;
        // INNKEEPER_DATA is the environment's own profile's.
        let data_env =
            env_path("INNKEEPER_DATA").filter(|_| self::profile().ok().as_ref() == Some(&profile));
        let sfx = |n: &str| per_profile(n, &profile);
        let runtime = env_path("XDG_RUNTIME_DIR")
            .unwrap_or_else(|| format!("/run/user/{}", sys::uid()).into());
        // The app's runtime dir is shared by every sandbox instance.
        let run = runtime
            .join("app")
            .join(env::var("FLATPAK_ID").unwrap_or_else(|_| APP_ID.into()))
            .join(sfx("innkeeper"));
        let data_home = data_home();
        Ok(Paths {
            libexec: prefix.join("libexec/innkeeper"),
            run,
            state: xdg_home("XDG_STATE_HOME", ".local/state").join(sfx("innkeeper")),
            data: data_env.unwrap_or_else(|| data_home.join(sfx("data"))),
            images: env_path("INNKEEPER_IMAGES").unwrap_or_else(|| data_home.join("images")),
            pin: env_path("INNKEEPER_PIN").unwrap_or_else(|| share.join("image/pin.toml")),
            root: data_home.join(sfx("root")),
            data_home,
            share,
            profile,
        })
    }

    /// The paths of the prefix this executable is installed in
    /// (`<prefix>/libexec/innkeeper/innkeeper-session`).
    pub fn of_this_exe() -> Result<Self> {
        let exe = env::current_exe()
            .and_then(|p| p.canonicalize())
            .context("can't find this program's path")?;
        let prefix = exe
            .ancestors()
            .nth(3)
            .context("the launcher is not in <prefix>/libexec/innkeeper")?;
        Self::new(prefix)
    }

    pub fn conf(&self) -> PathBuf {
        self.run.join("session.conf")
    }

    /// Android init's pid.
    pub fn pid_file(&self) -> PathBuf {
        self.run.join("pid")
    }

    /// The process init is a child of: pivot-exec, which execs the broker.
    pub fn outer_pid_file(&self) -> PathBuf {
        self.run.join("outer.pid")
    }

    /// The control socket, which other instances of the app (the CLI, the
    /// manager) reach the session through.
    pub fn control(&self) -> PathBuf {
        self.run.join("control.sock")
    }

    /// What the system app last reported about the session's activity
    /// (written by the bridge, [`crate::lifecycle`]).
    pub fn lifecycle(&self) -> PathBuf {
        self.run.join("lifecycle")
    }

    /// Held shared by every running `exec`, which keeps the session up.
    pub fn exec_lock(&self) -> PathBuf {
        self.run.join("exec.lock")
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

    /// Addon layers over the partitions, one dir each (test:bionic's test
    /// vendor binaries).
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
/// every profile and never rewritten under a running session) and the
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
        assert_eq!(apex_selection_hash(&t[..2]), apex_selection_hash(&[]));
        assert_ne!(apex_selection_hash(&t), apex_selection_hash(&[]));
    }
}
