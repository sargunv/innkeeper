//! Preparing the pinned image: fetched and extracted, its APEXes flattened,
//! its files read-only, and the derived layers current (image/README.md).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::paths::{Image, Paths};

fn run(c: &mut Command) -> Result<()> {
    let st = c
        .status()
        .with_context(|| format!("run {:?}", c.get_program()))?;
    if !st.success() {
        bail!("{:?} failed ({st})", c.get_program());
    }
    Ok(())
}

/// Makes every file under `dir` read-only (a-w), `last` after all others, so
/// a tree whose `last` is read-only is done. Flatpak mode's composed roots
/// hardlink these files, so this keeps a session from writing the cache
/// through them (and ART refuses dex files its process could write).
fn make_read_only(dir: &Path, last: &Path) -> Result<()> {
    if fs::metadata(last).is_ok_and(|m| m.permissions().mode() & 0o222 == 0) {
        return Ok(());
    }
    walk_files(dir, &mut |p, m| {
        if p != last && m & 0o222 != 0 {
            fs::set_permissions(p, fs::Permissions::from_mode(m & !0o222 & 0o7777))
                .with_context(|| format!("chmod {}", p.display()))?;
        }
        Ok(())
    })?;
    let m = fs::metadata(last)?.permissions().mode();
    fs::set_permissions(last, fs::Permissions::from_mode(m & !0o222 & 0o7777))?;
    Ok(())
}

/// Calls `f(path, mode)` for every regular file under `dir`.
pub fn walk_files(dir: &Path, f: &mut dyn FnMut(&Path, u32) -> Result<()>) -> Result<()> {
    for e in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let e = e?;
        let t = e.file_type()?;
        let p = e.path();
        if t.is_dir() {
            walk_files(&p, f)?;
        } else if t.is_file() {
            f(&p, e.metadata()?.permissions().mode())?;
        }
    }
    Ok(())
}

/// What a derived layer was made from: the APEX tree and derive.sh (the
/// format the shell launcher wrote, so existing layers stay current).
fn derive_source(paths: &Paths, image: &Image) -> Result<String> {
    let script = fs::read(paths.libexec.join("image/derive.sh"))?;
    let h: String = Sha256::digest(&script)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("{} {h}  -", image.apexdir.display()))
}

/// Fetches, extracts and flattens the image if needed, and brings the common
/// and single-uid derived layers up to date.
pub fn prepare(paths: &Paths, image: &Image) -> Result<crate::prune::Lease> {
    let _prepare = crate::prune::prepare_lock(&paths.images)?;
    let lease = crate::prune::lease(paths, image)?;
    let tools = paths.libexec.join("image");
    if !image.img.join("system/system").is_dir() {
        run(Command::new(tools.join("fetch.sh"))
            .arg(&paths.pin)
            .arg(&paths.images))?;
    }
    make_read_only(&image.img, &image.img.join("system/system/build.prop"))?;
    let info = image.apexdir.join("apex-info-list.xml");
    if !info.is_file() {
        let sfx = crate::paths::per_instance("", &paths.instance);
        let tmp = image.apexdir.with_file_name(format!(
            "{}.tmp{sfx}",
            image.apexdir.file_name().unwrap().to_string_lossy()
        ));
        let _ = fs::remove_dir_all(&tmp);
        run(Command::new(tools.join("flatten-apex.sh"))
            .arg(&image.img)
            .arg(paths.share.join("image/bootconfig"))
            .arg(&tmp))?;
        make_read_only(&tmp, &tmp.join("apex-info-list.xml"))?;
        // Another instance may have won the race: fine.
        if fs::rename(&tmp, &image.apexdir).is_err() {
            let _ = fs::remove_dir_all(&tmp);
        }
    }
    make_read_only(&image.apexdir, &info)?;
    run(Command::new(tools.join("check-forks.sh"))
        .arg(&image.img)
        .arg(&image.apexdir)
        .arg(paths.share.join("image/fork-bases.sha256"))
        .arg(paths.android()))?;
    let want = derive_source(paths, image)?;
    fs::create_dir_all(&image.derived)?;
    for layer in ["common", "single"] {
        let source = image.derived.join(format!("{layer}.source"));
        if fs::read_to_string(&source).is_ok_and(|s| s.trim_end_matches('\n') == want) {
            continue;
        }
        if lease.was_in_use {
            bail!(
                "derived image layers are in use; stop sessions using {} before preparing changed layers",
                image.name
            );
        }
        let _ = fs::remove_file(&source);
        let derive = tools.join("derive.sh");
        let out = image.derived.join(layer);
        run(Command::new(derive)
            .arg(layer)
            .arg(&image.img)
            .arg(&image.apexdir)
            .arg(out))?;
        fs::write(&source, format!("{want}\n"))?;
    }
    crate::prune::prune(paths, image)?;
    Ok(lease)
}
