//! The uid mode (docs/session.md, "Uid modes") and the session's user
//! namespace.
//!
//! - **subuid:** when the subordinate id files give the user at least 49999
//!   ids each and `newuidmap` is installed. Root is the host user, and
//!   Android's uid and gid ranges map to subordinate ids: 1-29999 (system,
//!   AIDs, apps 10000-19999, app cache gids 20000-29999), 50000-59999 (shared
//!   app gids) and 90000-99999 (isolated and app-zygote uids).
//! - **single:** otherwise, and always in Flatpak mode. Only root is mapped
//!   (which needs no helper and denies setgroups); every Android uid is the
//!   host user and the supervisor virtualizes them.
//!
//! An existing `/data` keeps the mode it was made in: Android's files belong
//! to the host user in single mode.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::paths::Sandbox;
use crate::sys;

/// Ids each subordinate range must hold: Android's highest mapped id is 99999,
/// in 49999 ids (see the module doc).
const SUBID_COUNT: u64 = 49999;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UidMode {
    Subuid,
    Single,
}

impl UidMode {
    pub fn name(self) -> &'static str {
        match self {
            UidMode::Subuid => "subuid",
            UidMode::Single => "single",
        }
    }
}

/// The uid mode and, in subuid mode, where the user's ranges start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Uids {
    pub mode: UidMode,
    /// The first subordinate uid and gid (subuid mode).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subuid_base: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgid_base: Option<u64>,
}

/// The subordinate id files to read (`LINUXAPK_SUBUID_FILE` and
/// `LINUXAPK_SUBGID_FILE` stand in for them in tests).
fn subid_files() -> (PathBuf, PathBuf) {
    let f = |var, default| env::var_os(var).map_or_else(|| PathBuf::from(default), PathBuf::from);
    (
        f("LINUXAPK_SUBUID_FILE", "/etc/subuid"),
        f("LINUXAPK_SUBGID_FILE", "/etc/subgid"),
    )
}

/// The start of the first range for `user` (name or uid) in a subordinate id
/// file that holds Android's ids.
fn subid_base(file: &str, name: &str, uid: u32) -> Option<u64> {
    let uid = uid.to_string();
    file.lines().find_map(|l| {
        let mut f = l.split(':');
        let (who, start, count) = (f.next()?, f.next()?, f.next()?);
        let count: u64 = count.trim().parse().ok()?;
        ((who == name || who == uid) && count >= SUBID_COUNT)
            .then(|| start.trim().parse().ok())
            .flatten()
    })
}

fn bases() -> (Option<u64>, Option<u64>) {
    let (u, g) = subid_files();
    let name = sys::user_name().unwrap_or_default();
    let base = |p: &Path| {
        subid_base(
            &fs::read_to_string(p).unwrap_or_default(),
            &name,
            sys::uid(),
        )
    };
    (base(&u), base(&g))
}

fn on_path(name: &str) -> bool {
    env::var_os("PATH").is_some_and(|p| env::split_paths(&p).any(|d| d.join(name).is_file()))
}

/// The mode a session on `data` uses (see the module doc).
pub fn detect(data: &Path, sandbox: Sandbox) -> UidMode {
    if let Ok(m) = fs::metadata(data.join("system")) {
        return if m.uid() == sys::uid() {
            UidMode::Single
        } else {
            UidMode::Subuid
        };
    }
    if sandbox == Sandbox::Flatpak {
        return UidMode::Single;
    }
    match bases() {
        (Some(_), Some(_)) if on_path("newuidmap") && on_path("newgidmap") => UidMode::Subuid,
        _ => UidMode::Single,
    }
}

/// The mode for `data` with its ranges.
pub fn setup(data: &Path, sandbox: Sandbox) -> Result<Uids> {
    let mode = detect(data, sandbox);
    if mode == UidMode::Single {
        return Ok(Uids {
            mode,
            subuid_base: None,
            subgid_base: None,
        });
    }
    if sandbox == Sandbox::Flatpak {
        bail!(
            "{} was made in subuid mode, which Flatpak can't run",
            data.display()
        );
    }
    let (u, g) = bases();
    let (Some(u), Some(g)) = (u, g) else {
        let (uf, gf) = subid_files();
        bail!(
            "{} was made with subordinate ids, but {} and {} give {} no range of {SUBID_COUNT} now",
            data.display(),
            uf.display(),
            gf.display(),
            sys::user_name().unwrap_or_default()
        );
    };
    Ok(Uids {
        mode,
        subuid_base: Some(u),
        subgid_base: Some(g),
    })
}

impl Uids {
    /// newuidmap/newgidmap arguments after the pid: (inside, outside, count)
    /// triples for `id` (the user's uid or gid) and the range at `base`.
    fn map_args(id: u32, base: u64) -> Vec<String> {
        [
            (0, id as u64, 1),
            (1, base, 29999),
            (50000, base + 29999, 10000),
            (90000, base + 39999, 10000),
        ]
        .iter()
        .flat_map(|(a, b, c)| [a.to_string(), b.to_string(), c.to_string()])
        .collect()
    }

    /// Moves this process into a new user namespace with the session's id
    /// maps, where it is Android's root and files can belong to Android's
    /// uids. The process must be single-threaded.
    pub fn enter_userns(&self) -> Result<()> {
        // Read before unsharing: until the maps are written, the new
        // namespace shows the overflow ids.
        let (uid, gid) = (sys::uid(), sys::gid());
        if self.mode == UidMode::Single {
            sys::unshare(libc::CLONE_NEWUSER)?;
            fs::write("/proc/self/setgroups", "deny").context("deny setgroups")?;
            fs::write("/proc/self/uid_map", format!("0 {uid} 1")).context("uid_map")?;
            fs::write("/proc/self/gid_map", format!("0 {gid} 1")).context("gid_map")?;
            return Ok(());
        }
        let (Some(ub), Some(gb)) = (self.subuid_base, self.subgid_base) else {
            bail!("subuid mode without ranges");
        };
        // newuidmap must run outside the new namespace: a helper waits for
        // this process to unshare, then maps it.
        let (mut r, mut w) = std::io::pipe().context("pipe")?;
        let me = sys::pid();
        let helper = sys::fork()?;
        if helper == 0 {
            drop(w);
            let mut b = [0u8; 1];
            let ok = r.read_exact(&mut b).is_ok()
                && [("newuidmap", uid, ub), ("newgidmap", gid, gb)].iter().all(
                    |(tool, id, base)| {
                        Command::new(tool)
                            .arg(me.to_string())
                            .args(Self::map_args(*id, *base))
                            .status()
                            .is_ok_and(|s| s.success())
                    },
                );
            unsafe { libc::_exit(if ok { 0 } else { 1 }) };
        }
        drop(r);
        let res = sys::unshare(libc::CLONE_NEWUSER);
        let _ = w.write_all(b"x");
        drop(w);
        let st = sys::wait(helper).context("wait for newuidmap")?;
        res?;
        if st != 0 {
            bail!("newuidmap/newgidmap failed ({st})");
        }
        Ok(())
    }
}

/// Runs `argv` as Android's root: in a user namespace with the session's id
/// maps on the host (`_userns`, a fresh single-threaded process), or directly
/// in Flatpak mode, where there is none.
pub fn as_android(
    exe: &Path,
    uids: &Uids,
    sandbox: Sandbox,
    argv: &[&std::ffi::OsStr],
) -> Result<()> {
    let mut c = if sandbox == Sandbox::Flatpak {
        let mut c = Command::new(argv[0]);
        c.args(&argv[1..]);
        c
    } else {
        let mut c = Command::new(exe);
        c.arg("_userns").arg(toml::to_string(uids)?).args(argv);
        c
    };
    let st = c.status().with_context(|| format!("run {:?}", argv[0]))?;
    if !st.success() {
        bail!("{:?} failed ({st})", argv[0]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subid_ranges() {
        let f = "someone-else:100000:65536\nme:200000:1000\nme:300000:65536\n";
        assert_eq!(subid_base(f, "me", 1000), Some(300000));
        assert_eq!(subid_base("1000:100000:49999\n", "me", 1000), Some(100000));
        assert_eq!(subid_base("1000:100000:49998\n", "me", 1000), None);
        assert_eq!(subid_base("other:1:65536\n", "me", 1000), None);
        assert_eq!(subid_base("", "me", 1000), None);
        assert_eq!(subid_base("garbage\n::\n", "me", 1000), None);
    }

    #[test]
    fn maps() {
        assert_eq!(
            Uids::map_args(1000, 100000).join(" "),
            "0 1000 1 1 100000 29999 50000 129999 10000 90000 139999 10000"
        );
    }

    #[test]
    fn data_keeps_its_mode() {
        let d = std::env::temp_dir().join(format!("linuxapk-uid-test-{}", std::process::id()));
        fs::create_dir_all(d.join("system")).unwrap();
        assert_eq!(detect(&d, Sandbox::Host), UidMode::Single);
        fs::remove_dir_all(&d).unwrap();
        // Any /data whose system/ the user doesn't own (status only reads it).
        let root_owned = Path::new("/usr/lib/systemd");
        if fs::metadata(root_owned.join("system")).is_ok_and(|m| m.uid() != sys::uid()) {
            assert_eq!(detect(root_owned, Sandbox::Host), UidMode::Subuid);
        }
        // A fresh /data in Flatpak mode is always single.
        assert_eq!(
            detect(Path::new("/nonexistent"), Sandbox::Flatpak),
            UidMode::Single
        );
    }
}
