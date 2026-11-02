//! The install prefix this program runs from and the runtime paths of
//! profiles. The caller names the profile. Programs that the launcher starts
//! read theirs from `INNKEEPER_PROFILE` through [`Prefix::session_paths`].

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// The session launcher, relative to the prefix.
const LAUNCHER: &str = "libexec/innkeeper/innkeeper-session";

/// The prefix of an installed Innkeeper. It is `/app` in the Flatpak and
/// `out/stage` in a checkout.
pub struct Prefix(PathBuf);

impl Prefix {
    /// Finds the prefix this program is installed in. That is the nearest
    /// ancestor of the executable that holds the launcher, which works from
    /// both `bin/` and `libexec/innkeeper/`.
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
                    "innkeeper-session not found: {} isn't in an installed Innkeeper \
                     (in a checkout, run `mise run build` and use out/stage)",
                    exe.display()
                )
            })
            .context(crate::ErrorKind::SessionStartFailed)
    }

    pub fn launcher(&self) -> PathBuf {
        self.0.join(LAUNCHER)
    }

    /// The runtime paths of the profile named by `INNKEEPER_PROFILE`. Programs
    /// that the launcher starts, such as the bridge, use this.
    pub fn session_paths(&self) -> Result<innkeeper_profile::Paths> {
        innkeeper_profile::Paths::new(&self.0)
    }

    /// The runtime paths of profile `name`.
    pub fn profile_paths(&self, name: &str) -> Result<innkeeper_profile::Paths> {
        innkeeper_profile::Paths::for_profile(&self.0, name)
    }
}

/// Builds the argv `innkeeper run <package>`, which an app's desktop entry
/// runs. It names no profile, so that renaming the profile leaves the app-menu
/// entry as it is. The CLI finds the profile in the app's record
/// (`X-Innkeeper-Profile`).
pub fn app_command(package: &str) -> Vec<String> {
    [crate::xdg::INNKEEPER, "run", package]
        .map(String::from)
        .into()
}
