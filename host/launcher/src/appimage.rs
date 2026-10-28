//! Starting a session from the AppImage (packaging/appimage). Its prefix is a
//! FUSE mount that goes away when the program that mounted it (the CLI or
//! the manager) exits, and the session outlives it. So `start` runs the
//! session from a copy of the prefix, one per build:
//! `$XDG_DATA_HOME/linuxapk/appimage/<build>/`. Copies no process runs from
//! any more are removed.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::paths::Paths;
use crate::sys;

/// The prefix `paths` come from, if it is inside the mounted AppImage this
/// launcher runs from (`$APPDIR`, which the AppImage runtime sets).
fn mounted_prefix(paths: &Paths) -> Option<PathBuf> {
    let appdir = env::var_os("APPDIR")
        .filter(|v| !v.is_empty())
        .and_then(|d| fs::canonicalize(d).ok())?;
    let exe = env::current_exe().and_then(fs::canonicalize).ok()?;
    let prefix = paths.libexec.parent()?.parent()?;
    (exe.starts_with(&appdir) && prefix.starts_with(&appdir)).then(|| prefix.to_path_buf())
}

/// The copy of the AppImage's prefix a session runs from, made if needed,
/// when this launcher runs from an AppImage's mount. None otherwise.
pub fn session_prefix(paths: &Paths) -> Result<Option<PathBuf>> {
    let Some(src) = mounted_prefix(paths) else {
        return Ok(None);
    };
    // The manifest lists every staged file's hash: it names the build.
    let manifest = fs::read(paths.share.join("manifest.tsv")).context("read the manifest")?;
    let build: String = Sha256::digest(&manifest)
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect();
    let dir = paths.data_home.join("appimage");
    let dst = dir.join(&build);
    if !dst.join("share/linuxapk/manifest.tsv").is_file() {
        fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let tmp = dir.join(format!(".{build}.{}", sys::pid()));
        let st = Command::new("cp")
            .args(["-R", "--preserve=mode,links", "--"])
            .arg(&src)
            .arg(&tmp)
            .stdin(Stdio::null())
            .status()
            .context("run cp")?;
        if !st.success() {
            let _ = fs::remove_dir_all(&tmp);
            bail!(
                "copying {} to {} failed ({st})",
                src.display(),
                tmp.display()
            );
        }
        // Another start may have made it meanwhile.
        if let Err(e) = fs::rename(&tmp, &dst) {
            fs::remove_dir_all(&tmp)?;
            if !dst.is_dir() {
                return Err(e).with_context(|| format!("create {}", dst.display()));
            }
        }
    }
    prune(&dir, &dst);
    Ok(Some(dst))
}

/// Removes the copies in `dir` other than `keep` that no process runs from,
/// and copies a start that died left half made.
fn prune(dir: &Path, keep: &Path) {
    let Ok(rd) = fs::read_dir(dir) else {
        return;
    };
    let exes = process_exes();
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let in_use = match name.strip_prefix('.') {
            // .<build>.<pid>: being copied by a live start.
            Some(rest) => rest
                .rsplit_once('.')
                .and_then(|(_, pid)| pid.parse().ok())
                .is_some_and(sys::alive),
            None => exes.iter().any(|x| x.starts_with(&p)),
        };
        if p != keep && !in_use {
            let _ = fs::remove_dir_all(&p);
        }
    }
}

/// The executables of every process this user may inspect.
fn process_exes() -> Vec<PathBuf> {
    fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.parse::<u32>().is_ok())
        })
        .filter_map(|e| fs::read_link(e.path().join("exe")).ok())
        .collect()
}
