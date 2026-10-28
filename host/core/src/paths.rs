//! Where things are: the install prefix this program runs from, the session
//! instance it works with (the only reader and writer of `LINUXAPK_INSTANCE`),
//! and the XDG base directories.

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

/// The argv that runs `program` against session `instance`: prefixed with
/// `env LINUXAPK_INSTANCE=<instance>` for a named instance, so a desktop entry
/// reaches the session it was made for.
pub fn command(program: &Path, instance: &str) -> Vec<String> {
    let mut v = vec![];
    if !instance.is_empty() {
        v.push("env".into());
        v.push(format!("LINUXAPK_INSTANCE={instance}"));
    }
    v.push(program.to_string_lossy().into_owned());
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

/// The systemd user unit the launcher runs the session in.
pub fn unit_name() -> Result<String> {
    per_instance("linuxapk-session")
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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn commands_read_back() {
        let named = command(Path::new("/x/linuxapk"), "cli");
        assert_eq!(named.join(" "), "env LINUXAPK_INSTANCE=cli /x/linuxapk");
        let i = invocation(&named);
        assert_eq!((i.instance, i.program), ("cli", Some("/x/linuxapk")));
        assert!(!i.foreign_env);
        let main = command(Path::new("/x/linuxapk"), "");
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
