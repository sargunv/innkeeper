//! Where things are: the install prefix this program runs from, the session
//! instance it works with (parsed by the launcher library, the only reader of
//! `LINUXAPK_INSTANCE`), and the XDG base directories.

use std::env;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// The session launcher, relative to the prefix.
const LAUNCHER: &str = "libexec/linuxapk/linuxapk-session";

/// An installed Linux.apk: `/app` in the Flatpak,
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

    /// Runtime paths shared by the CLI and its session.
    pub fn session_paths(&self) -> Result<linuxapk_session::Paths> {
        linuxapk_session::Paths::new(&self.0)
    }

    /// A program in `bin/`.
    pub fn bin(&self, name: &str) -> PathBuf {
        self.0.join("bin").join(name)
    }

    /// The command that runs this installation's CLI.
    pub fn cli(&self) -> Vec<String> {
        vec![self.bin("linuxapk").to_string_lossy().into_owned()]
    }
}

/// `LINUXAPK_INSTANCE` if it is a valid instance name, else an error. Empty
/// for the main session.
pub use linuxapk_session::paths::instance;

/// An XDG base directory: `$var` if it is absolute, else `default` under
/// `$HOME`.
pub use linuxapk_session::paths::xdg_home;

/// `name`, suffixed with `-<instance>` for a named instance: the session's
/// XDG directories are named this way.
fn per_instance(name: &str) -> Result<String> {
    Ok(linuxapk_session::paths::per_instance(name, &instance()?))
}

/// The argv that runs `program` (a command line, such as [`Prefix::cli`])
/// against session `instance`: prefixed with `env LINUXAPK_INSTANCE=<instance>`
/// for a named instance, so a desktop entry reaches the session it was made
/// for.
pub fn command(program: &[String], instance: &str) -> Vec<String> {
    let mut v = vec![];
    if !instance.is_empty() {
        v.push("env".into());
        v.push(format!("LINUXAPK_INSTANCE={instance}"));
    }
    v.extend_from_slice(program);
    v
}

/// A [`command`] argv read back (from a desktop entry's `Exec`).
#[derive(Debug, PartialEq, Eq)]
pub struct Invocation<'a> {
    /// The instance it runs against, empty for the main session.
    pub instance: &'a str,
    /// The program, the first word after the environment.
    pub program: Option<&'a str>,
    /// It sets variables [`command`] doesn't (an older Linux.apk's entry).
    pub foreign_env: bool,
}

pub fn invocation(argv: &[String]) -> Invocation<'_> {
    let env_len = match argv.first().map(String::as_str) {
        Some("env") => 1 + argv[1..].iter().take_while(|w| w.contains('=')).count(),
        _ => 0,
    };
    let env = argv.get(1..env_len).unwrap_or_default();
    Invocation {
        instance: env
            .iter()
            .find_map(|w| w.strip_prefix("LINUXAPK_INSTANCE="))
            .unwrap_or_default(),
        program: argv.get(env_len).map(String::as_str),
        foreign_env: env.iter().any(|w| !w.starts_with("LINUXAPK_INSTANCE=")),
    }
}

/// The session's state directory (logs, the documents service's grants):
/// `$XDG_STATE_HOME/linuxapk[-<instance>]`.
pub fn state_dir() -> Result<PathBuf> {
    Ok(xdg_home("XDG_STATE_HOME", ".local/state").join(per_instance("linuxapk")?))
}

/// Where one-time notices remember they were shown:
/// `$XDG_STATE_HOME/linuxapk/notices` (shared by every instance).
pub fn notices_dir() -> PathBuf {
    xdg_home("XDG_STATE_HOME", ".local/state").join("linuxapk/notices")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn commands_read_back() {
        let named = command(&argv("/x/linuxapk"), "cli");
        assert_eq!(named.join(" "), "env LINUXAPK_INSTANCE=cli /x/linuxapk");
        let i = invocation(&named);
        assert_eq!((i.instance, i.program), ("cli", Some("/x/linuxapk")));
        assert!(!i.foreign_env);
        let main = command(&argv("/x/linuxapk"), "");
        assert_eq!(invocation(&main).instance, "");
        let old = argv("env LINUXAPK_SESSION=/s LINUXAPK_INSTANCE=cli linuxapk run a.b");
        let old = invocation(&old);
        assert_eq!((old.instance, old.program), ("cli", Some("linuxapk")));
        assert!(old.foreign_env);
        // Only the environment `env` sets counts.
        let args = argv("linuxapk run LINUXAPK_INSTANCE=x");
        let i = invocation(&args);
        assert_eq!(
            (i.instance, i.program, i.foreign_env),
            ("", Some("linuxapk"), false)
        );
    }
}
