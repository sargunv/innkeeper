//! Where things are: the install prefix this program runs from, profiles'
//! runtime paths, and the XDG base directories. Profiles are always named by
//! the caller; only programs the launcher starts read their own from
//! `INNKEEPER_PROFILE` ([`Prefix::session_paths`]).

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// The session launcher, relative to the prefix.
const LAUNCHER: &str = "libexec/innkeeper/innkeeper-session";

/// An installed Innkeeper: `/app` in the Flatpak, `out/stage` in a checkout.
pub struct Prefix(PathBuf);

impl Prefix {
    /// The prefix this program is installed in: the nearest ancestor of its
    /// executable that holds the launcher (so it works from `bin/` and from
    /// `libexec/innkeeper/`).
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

    /// The runtime paths of the profile in `INNKEEPER_PROFILE`: for programs
    /// the launcher starts, such as the bridge.
    pub fn session_paths(&self) -> Result<innkeeper_session::Paths> {
        innkeeper_session::Paths::new(&self.0)
    }

    /// Runtime paths of profile `name`.
    pub fn profile_paths(&self, name: &str) -> Result<innkeeper_session::Paths> {
        innkeeper_session::Paths::for_profile(&self.0, name)
    }
}

pub use innkeeper_session::profile::ProfileName;

/// An XDG base directory: `$var` if it is absolute, else `default` under
/// `$HOME`.
pub use innkeeper_session::paths::xdg_home;

/// The argv that runs the CLI ([`crate::xdg::INNKEEPER`]) against profile
/// `profile` with `args`: `innkeeper --profile <profile> <args>`.
pub fn cli(profile: &ProfileName, args: &[&str]) -> Vec<String> {
    let mut v = vec![
        crate::xdg::INNKEEPER.to_string(),
        "--profile".into(),
        profile.to_string(),
    ];
    v.extend(args.iter().map(|a| a.to_string()));
    v
}
