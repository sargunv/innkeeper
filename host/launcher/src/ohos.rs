//! OpenHarmony as a profile's guest system (ohos/README.md, "How it boots").
//! This module prepares its image, composes its root and sets up the
//! sandbox's `/` for it. The rest of the session is shared with Android
//! ([`crate::sandbox`]), including the kmsg log, the bridge, the control
//! socket and the lifecycle.
//!
//! The root is composed on disk like Android's. `system` and `vendor` are
//! hardlinks or reflink copies of the image cache. Our layer goes over
//! `vendor` and also forms `sys_prod`, OH's product partition. The image
//! leaves `sys_prod` empty, but it is on init's cfg path and the musl
//! linker's search path.

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::Path;

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::conf::Config;
use crate::paths::{Image, Paths};
use crate::profile::System;
use crate::sandbox::{files_read_only, link, over, remove_tree, tree_linker};

/// Bumped when the composition below changes.
const COMPOSE_VERSION: &str = "ohos-2";

/// DMS's config. Its `<dpi>` is the only place DMS takes density from.
const DMS_CONFIG: &str = "vendor/etc/window/resources/display_manager_config.xml";

/// The audio policy's adapters and pipes.
const POLICY_CONFIG: &str = "vendor/etc/audio/audio_policy_config.xml";

/// The audio policy's volume levels. The policy reads the copy in
/// `sys_prod` in place of the image's.
const VOLUME_CONFIG: &str = "etc/audio/audio_volume_config.xml";

/// The screen lock service's per-user "lock screen disabled" flags, under
/// `/data`.
const SCREENLOCK_STATE: &str = "service/el1/public/screenlock/screenlock_state.xml";

/// Flags that turn the lock screen off for user 100, the first user.
const SCREENLOCK_OFF: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
    <preferences version=\"1.0\"><bool key=\"100\" value=\"true\"/></preferences>\n";

/// Fetches and extracts the pinned image if it is missing, then prunes the
/// cache as Android's preparation does. The returned lease keeps the image
/// while the session runs.
pub fn prepare(paths: &Paths, image: &Image) -> Result<crate::prune::Lease> {
    let _prepare = crate::prune::prepare_lock(&paths.images)?;
    let lease = crate::prune::lease(paths, image)?;
    if !image.img.join("system/system/bin/init").exists() {
        crate::image::fetch(paths, System::Ohos)?;
    }
    crate::prune::prune(paths, &crate::prune::pinned(paths)?)?;
    Ok(lease)
}

/// The panel size of the profile's display preset, as `(width, height, dpi)`.
fn panel(cfg: &Config) -> Result<(u32, u32, u32)> {
    let s = crate::profile::Settings::load(&cfg.paths.data)?;
    s.display
        .screen()
        .with_context(|| format!("OpenHarmony can't show as {}", s.display.name()))
}

/// Returns `display_manager_config.xml` with its density set to `dpi`.
fn dms_config(xml: &str, dpi: u32) -> String {
    let (Some(a), Some(b)) = (xml.find("<dpi>"), xml.find("</dpi>")) else {
        return xml.into();
    };
    format!("{}<dpi>{dpi}{}", &xml[..a], &xml[b..])
}

/// Returns `audio_volume_config.xml` with each volume type's default level
/// at its maximum. With `const.multimedia.audio.fixedvolume` the levels
/// can't change, so OH applies no system volume and the desktop mixer is the
/// control.
fn volume_config(xml: &str) -> String {
    let attr = |tag: &str, name: &str| -> Option<(usize, usize)> {
        let key = format!(" {name}=\"");
        let a = tag.find(&key)? + key.len();
        Some((a, a + tag[a..].find('"')?))
    };
    let mut out = String::with_capacity(xml.len());
    let mut rest = xml;
    while let Some(start) = rest.find("<volume_type ") {
        let Some(len) = rest[start..].find('>') else {
            break;
        };
        let tag = &rest[start..start + len];
        out.push_str(&rest[..start]);
        match (attr(tag, "maxidx"), attr(tag, "defaultidx")) {
            (Some((ma, mb)), Some((da, db))) => {
                out.push_str(&tag[..da]);
                out.push_str(&tag[ma..mb]);
                out.push_str(&tag[db..]);
            }
            _ => out.push_str(tag),
        }
        rest = &rest[start + len..];
    }
    out.push_str(rest);
    out
}

/// Returns `audio_policy_config.xml` with the primary adapter's pipes at
/// 48 kHz, the desktop's usual rate, so neither OH nor PipeWire resamples
/// the mix. The image's pipes run at 44.1 kHz.
fn policy_config(xml: &str) -> String {
    let Some(a) = xml.find("<adapter name=\"primary\"") else {
        return xml.into();
    };
    let Some(len) = xml[a..].find("</adapter>") else {
        return xml.into();
    };
    let b = a + len;
    let primary = xml[a..b].replace("sampleRates=\"44100\"", "sampleRates=\"48000\"");
    format!("{}{primary}{}", &xml[..a], &xml[b..])
}

/// Makes every directory under `dir` writable by its owner. The image's
/// directories are read-only, and the layers go into copies of them.
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
/// last composition had the same inputs. The panel's mode is one of the
/// inputs. The composer reads it from the `const.innkeeper.display`
/// parameter, and DMS gets its dpi. At 320 dpi a 720x1280 panel is 360x640
/// vp, a phone like the image's `const.product.devicetype`. The image's 160
/// dpi would make it a tablet 720 vp wide. The profile's UDID is another
/// input. OH would derive one from `ohos.boot.sn`, which the session doesn't
/// set, and reads `const.product.udid` first.
pub fn compose(cfg: &Config) -> Result<()> {
    let (r, layer) = (&cfg.paths.root, cfg.paths.ohos());
    let (w, h, dpi) = panel(cfg)?;
    let mut key = Sha256::new();
    key.update(COMPOSE_VERSION);
    key.update(cfg.image.img.as_os_str().as_encoded_bytes());
    key.update(fs::read(cfg.paths.share.join("manifest.tsv"))?);
    key.update(format!("{w}x{h}@{dpi}"));
    let udid = crate::profile::udid(&cfg.paths.profile);
    key.update(&udid);
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
    let policy = r.join(POLICY_CONFIG);
    let xml = fs::read_to_string(&policy).with_context(|| format!("read {}", policy.display()))?;
    fs::remove_file(&policy)?;
    fs::write(&policy, policy_config(&xml))?;
    let volume = r.join("system").join(VOLUME_CONFIG);
    let xml = fs::read_to_string(&volume).with_context(|| format!("read {}", volume.display()))?;
    let to = r.join("sys_prod").join(VOLUME_CONFIG);
    fs::create_dir_all(to.parent().unwrap())?;
    fs::write(&to, volume_config(&xml))?;
    let param = r.join("vendor/etc/param");
    fs::create_dir_all(&param)?;
    fs::write(
        param.join("innkeeper.para"),
        format!("const.innkeeper.display={w}x{h}@{dpi}\nconst.product.udid={udid}\n"),
    )?;
    files_read_only(r)?;
    fs::write(&key_file, key)?;
    Ok(())
}

/// Sets up the sandbox's `/` as OpenHarmony's root. The sandbox keeps its
/// `/usr`, `/bin` and `/etc` for the glibc side.
pub fn sandbox_root(cfg: &Config) -> Result<()> {
    let (r, slash) = (&cfg.paths.root, Path::new("/"));
    for p in ["system", "vendor", "sys_prod"] {
        link(&r.join(p), &slash.join(p))?;
    }
    let data = &cfg.paths.data;
    fs::create_dir_all(data)?;
    lock_screen_off_by_default(data)?;
    link(data, Path::new("/data"))?;
    // Each app gets its own /data/storage, which the shim keeps open in app
    // processes.
    link(Path::new("/proc/self/fd/990"), &data.join("storage"))?;
    // Add the image's other top-level entries where the sandbox has none,
    // such as /chipset and the empty mount points.
    crate::root::copy_top_level(&cfg.image.img, slash, false)?;
    // appspawn binds the user's files into each app's sandbox at
    // /storage/Users, and the shared files at /storage/Share. An app here
    // sees the session's /storage, so these links stand in for the binds.
    // storage_daemon makes user 100's view when the user starts.
    let storage = Path::new("/storage");
    link(
        Path::new("/mnt/user/100/nosharefs/docs"),
        &storage.join("Users"),
    )?;
    link(Path::new("Users"), &storage.join("users"))?;
    link(
        Path::new("/data/service/el1/public/storage_daemon/share/public"),
        &storage.join("Share"),
    )?;
    // OH binaries ask for /lib/ld-musl-x86_64.so.1, and the image's /lib is
    // /system/lib. The sandbox's /lib is a symlink to /usr/lib, so it is
    // replaced by a directory of the same entries plus the musl loader.
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
    // it lacks, and its own binds such as resolv.conf stay. The broker serves
    // /etc/passwd and /etc/group from /system/etc.
    for e in fs::read_dir(r.join("system/etc"))? {
        let name = e?.file_name();
        let to = Path::new("/etc").join(&name);
        if to.symlink_metadata().is_err() {
            symlink(Path::new("/system/etc").join(&name), &to)?;
        }
    }
    let dev = Path::new("/dev");
    // The broker serves the binder nodes, the access-token node and the
    // code-signing node.
    crate::root::binder_nodes(dev)?;
    for node in ["access_token_id", "code_sign"] {
        let node = dev.join(node);
        fs::write(&node, b"")?;
        fs::set_permissions(&node, fs::Permissions::from_mode(0o666))?;
    }
    crate::sandbox::dev_sockets(cfg)
}

/// Turns OH's lock screen off for a new profile, as Android's image does
/// (`ro.lockscreen.disable.default`). SceneBoard doesn't lock the screen
/// while the screen lock service reports it disabled. The service keeps the
/// flags in `data`, so this writes them only if the file doesn't exist yet.
/// The service clears the flag when the user sets a PIN in Settings.
fn lock_screen_off_by_default(data: &Path) -> Result<()> {
    let file = data.join(SCREENLOCK_STATE);
    if file.symlink_metadata().is_ok() {
        return Ok(());
    }
    fs::create_dir_all(file.parent().expect("a relative path with a parent"))?;
    fs::write(&file, SCREENLOCK_OFF).with_context(|| format!("write {}", file.display()))
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

    #[test]
    fn primary_pipes_run_at_48_khz() {
        let xml = concat!(
            "<adapter name=\"primary\">\n",
            "  <streamProp format=\"s16le\" sampleRates=\"44100\"/>\n",
            "  <streamProp format=\"s16le\" sampleRates=\"44100\"/>\n",
            "</adapter>\n",
            "<adapter name=\"remote\">\n",
            "  <streamProp format=\"s16le\" sampleRates=\"44100\"/>\n",
            "</adapter>\n",
        );
        assert_eq!(
            policy_config(xml),
            concat!(
                "<adapter name=\"primary\">\n",
                "  <streamProp format=\"s16le\" sampleRates=\"48000\"/>\n",
                "  <streamProp format=\"s16le\" sampleRates=\"48000\"/>\n",
                "</adapter>\n",
                "<adapter name=\"remote\">\n",
                "  <streamProp format=\"s16le\" sampleRates=\"44100\"/>\n",
                "</adapter>\n",
            )
        );
        assert_eq!(policy_config("<adapters/>"), "<adapters/>");
    }

    #[test]
    fn volume_defaults_to_the_maximum() {
        let xml = concat!(
            "<v>\n",
            "  <volume_type type=\"MUSIC\" minidx=\"0\" maxidx=\"15\" defaultidx=\"7\">\n",
            "    <point idx=\"0\" decibel=\"-2700\"/>\n",
            "  </volume_type>\n",
            "  <volume_type type=\"APP\" defaultidx=\"5\" maxidx=\"100\">\n",
            "  <volume_type type=\"X\" maxidx=\"3\">\n",
            "</v>\n",
        );
        assert_eq!(
            volume_config(xml),
            concat!(
                "<v>\n",
                "  <volume_type type=\"MUSIC\" minidx=\"0\" maxidx=\"15\" defaultidx=\"15\">\n",
                "    <point idx=\"0\" decibel=\"-2700\"/>\n",
                "  </volume_type>\n",
                "  <volume_type type=\"APP\" defaultidx=\"100\" maxidx=\"100\">\n",
                "  <volume_type type=\"X\" maxidx=\"3\">\n",
                "</v>\n",
            )
        );
        assert_eq!(volume_config("<volume_type a"), "<volume_type a");
    }

    #[test]
    fn lock_screen_flags_are_written_once() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../out/test/screenlock-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        lock_screen_off_by_default(&dir).unwrap();
        let file = dir.join(SCREENLOCK_STATE);
        assert!(
            fs::read_to_string(&file)
                .unwrap()
                .contains(r#"<bool key="100" value="true"/>"#)
        );
        let on = r#"<preferences version="1.0"><bool key="100" value="false"/></preferences>"#;
        fs::write(&file, on).unwrap();
        lock_screen_off_by_default(&dir).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), on);
        fs::remove_dir_all(dir).unwrap();
    }
}
