//! OpenHarmony as a profile's guest (ohos/README.md, "How it boots"): its
//! image, its composed root and the sandbox's `/` for it. The rest of the
//! session, the kmsg log, the bridge, the control socket and the lifecycle,
//! is Android's ([`crate::sandbox`]).
//!
//! The root is composed on disk as Android's is: `system` and `vendor` are
//! hardlinks to the image cache (or reflink copies), our layer goes over
//! `vendor` and is `sys_prod`, OH's product partition, which the image leaves
//! empty but which is on init's cfg path and the musl linker's search path.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::conf::Config;
use crate::paths::{Image, Paths};
use crate::sandbox::{files_read_only, link, over, remove_tree, tree_linker};

/// Bumped when the composition below changes.
const COMPOSE_VERSION: &str = "ohos-1";

/// DMS's config, whose `<dpi>` is the only place it takes density from.
const DMS_CONFIG: &str = "vendor/etc/window/resources/display_manager_config.xml";

/// Fetches and extracts the pinned image unless it is there.
pub fn prepare(paths: &Paths, image: &Image) -> Result<()> {
    if image.img.join("system/system/bin/init").exists() {
        return Ok(());
    }
    let tools = paths.libexec.join("image");
    let st = Command::new(tools.join("ohos/fetch.sh"))
        .arg(paths.share.join("image/ohos/pin.toml"))
        .arg(image.dir.parent().context("image dir")?)
        .arg(tools.join("fs-extract.sh"))
        .status()
        .context("run image/ohos/fetch.sh")?;
    if !st.success() {
        bail!("fetching the OpenHarmony image failed ({st})");
    }
    Ok(())
}

/// The panel: the profile's display preset, `(width, height, dpi)`.
fn panel(cfg: &Config) -> Result<(u32, u32, u32)> {
    let s = crate::profile::Settings::load(&cfg.paths.data)?;
    s.display
        .screen()
        .with_context(|| format!("OpenHarmony can't show as {}", s.display.name()))
}

/// `display_manager_config.xml` with `dpi` for density.
fn dms_config(xml: &str, dpi: u32) -> String {
    let (Some(a), Some(b)) = (xml.find("<dpi>"), xml.find("</dpi>")) else {
        return xml.into();
    };
    format!("{}<dpi>{dpi}{}", &xml[..a], &xml[b..])
}

/// Makes every directory under `dir` writable by its owner (the image's are
/// read-only, and the layers go into copies of them).
fn dirs_writable(dir: &Path) -> Result<()> {
    let m = dir.symlink_metadata()?;
    if !m.is_dir() {
        return Ok(());
    }
    fs::set_permissions(dir, fs::Permissions::from_mode((m.mode() & 0o7777) | 0o700))?;
    for e in fs::read_dir(dir)? {
        dirs_writable(&e?.path())?;
    }
    Ok(())
}

/// Composes `system`, `vendor` and `sys_prod` under the root dir, unless the
/// last composition had the same inputs. The panel's mode goes in with them:
/// the composer reads it from the `const.innkeeper.display` parameter, and
/// DMS gets its dpi (at 320 dpi a 720x1280 panel is 360x640 vp, a phone like
/// the image's `const.product.devicetype`; the image's 160 would make it a
/// 720 vp wide tablet).
pub fn compose(cfg: &Config) -> Result<()> {
    let (r, layer) = (&cfg.paths.root, cfg.paths.ohos());
    let (w, h, dpi) = panel(cfg)?;
    let mut key = Sha256::new();
    key.update(COMPOSE_VERSION);
    key.update(cfg.image.img.as_os_str().as_encoded_bytes());
    key.update(fs::read(cfg.paths.share.join("manifest.tsv"))?);
    key.update(format!("{w}x{h}@{dpi}"));
    let key: String = key.finalize().iter().map(|b| format!("{b:02x}")).collect();
    let key_file = r.join(".compose-key");
    if fs::read_to_string(&key_file).is_ok_and(|k| k == key) {
        return Ok(());
    }
    remove_tree(r)?;
    fs::create_dir_all(r)?;
    let copy = tree_linker(&cfg.image.img.join(".source"), r);
    copy(&cfg.image.img.join("system/system"), &r.join("system"))?;
    copy(&cfg.image.img.join("vendor"), &r.join("vendor"))?;
    dirs_writable(r)?;
    over(&layer.join("vendor"), &r.join("vendor"))?;
    fs::create_dir(r.join("sys_prod"))?;
    over(&layer.join("sys_prod"), &r.join("sys_prod"))?;
    let dms = r.join(DMS_CONFIG);
    let xml = fs::read_to_string(&dms).with_context(|| format!("read {}", dms.display()))?;
    fs::remove_file(&dms)?;
    fs::write(&dms, dms_config(&xml, dpi))?;
    let param = r.join("vendor/etc/param");
    fs::create_dir_all(&param)?;
    fs::write(
        param.join("innkeeper.para"),
        format!("const.innkeeper.display={w}x{h}@{dpi}\n"),
    )?;
    files_read_only(r)?;
    fs::write(&key_file, key)?;
    Ok(())
}

/// The sandbox's `/` as OpenHarmony's root. The sandbox keeps its `/usr`,
/// `/bin` and `/etc` for the glibc side.
pub fn sandbox_root(cfg: &Config) -> Result<()> {
    let (r, slash) = (&cfg.paths.root, Path::new("/"));
    for p in ["system", "vendor", "sys_prod"] {
        link(&r.join(p), &slash.join(p))?;
    }
    let data = &cfg.paths.data;
    fs::create_dir_all(data)?;
    link(data, Path::new("/data"))?;
    // Each app's own /data/storage, which the shim keeps open in app
    // processes.
    link(Path::new("/proc/self/fd/990"), &data.join("storage"))?;
    // The image's other top-level entries where the sandbox has none:
    // /chipset and the empty mount points.
    crate::root::copy_top_level(&cfg.image.img, slash, false)?;
    // OH binaries ask for /lib/ld-musl-x86_64.so.1, and the image's /lib is
    // /system/lib. The sandbox's /lib is a symlink to /usr/lib: it becomes a
    // directory of the same entries plus the musl loader.
    let lib = Path::new("/lib");
    if lib.symlink_metadata()?.is_symlink() {
        fs::remove_file(lib)?;
        fs::create_dir(lib)?;
        for e in fs::read_dir("/usr/lib")? {
            let e = e?;
            symlink(e.path(), lib.join(e.file_name()))?;
        }
    }
    link(
        Path::new("/system/lib/ld-musl-x86_64.so.1"),
        &lib.join("ld-musl-x86_64.so.1"),
    )?;
    // /etc is /system/etc in the image. The sandbox's /etc gains the entries
    // it lacks (its own binds, such as resolv.conf, stay); the broker serves
    // /etc/passwd and /etc/group from /system/etc.
    for e in fs::read_dir(r.join("system/etc"))? {
        let name = e?.file_name();
        let to = Path::new("/etc").join(&name);
        if to.symlink_metadata().is_err() {
            symlink(Path::new("/system/etc").join(&name), &to)?;
        }
    }
    let dev = Path::new("/dev");
    // The binder nodes and the access-token node, which the broker serves.
    crate::root::binder_nodes(dev)?;
    let token = dev.join("access_token_id");
    fs::write(&token, b"")?;
    fs::set_permissions(&token, fs::Permissions::from_mode(0o666))?;
    crate::sandbox::dev_sockets(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dms_takes_the_panels_dpi() {
        let xml = "<a>\n    <!-- dpi -->\n    <dpi>160</dpi>\n</a>\n";
        assert_eq!(
            dms_config(xml, 320),
            "<a>\n    <!-- dpi -->\n    <dpi>320</dpi>\n</a>\n"
        );
        assert_eq!(dms_config("<a/>", 320), "<a/>");
    }
}
