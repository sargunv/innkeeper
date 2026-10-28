//! Where things are: the install prefix this program runs from, the session
//! instance it works with (the only reader of `LINUXAPK_INSTANCE`), and the
//! XDG base directories.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The session launcher, relative to the prefix.
const LAUNCHER: &str = "libexec/linuxapk/linuxapk-session";

/// An installed Linux.apk: `/app` in the Flatpak, `usr/` in the AppImage,
/// `out/stage` in a checkout.
pub struct Prefix(PathBuf);

impl Prefix {
    /// The prefix this program is installed in: the nearest ancestor of its
    /// executable that holds the launcher (so it works from `bin/` and from
    /// `libexec/linuxapk/`).
    pub fn locate() -> Result<Prefix> {
        let exe = env::current_exe()
            .and_then(|p| p.canonicalize())
            .context("can't find this program's path")?;
        exe.ancestors()
            .skip(1)
            .find(|d| d.join(LAUNCHER).is_file())
            .map(|d| Prefix(d.to_path_buf()))
            .with_context(|| {
                format!(
                    "linuxapk-session not found: {} isn't in an installed Linux.apk \
                     (in a checkout, run `mise run build` and use out/stage)",
                    exe.display()
                )
            })
    }

    pub fn launcher(&self) -> PathBuf {
        self.0.join(LAUNCHER)
    }

    /// A program in `bin/`.
    pub fn bin(&self, name: &str) -> PathBuf {
        self.0.join("bin").join(name)
    }
}

/// `LINUXAPK_INSTANCE` if it is a valid instance name (as the launcher checks
/// it), else an error. Empty for the main session.
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
/// systemd unit and its XDG directories are named this way.
fn per_instance(name: &str) -> Result<String> {
    let inst = instance()?;
    Ok(if inst.is_empty() {
        name.into()
    } else {
        format!("{name}-{inst}")
    })
}

/// The systemd user unit the launcher runs the session in.
pub fn unit_name() -> Result<String> {
    per_instance("linuxapk-session")
}

/// The session's state directory (logs, the documents service's grants):
/// `$XDG_STATE_HOME/linuxapk[-<instance>]`.
pub fn state_dir() -> Result<PathBuf> {
    Ok(xdg_home("XDG_STATE_HOME", ".local/state").join(per_instance("linuxapk")?))
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
