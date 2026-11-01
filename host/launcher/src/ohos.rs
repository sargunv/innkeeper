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
const COMPOSE_VERSION: &str = "ohos-3";

/// DMS's config. Its `<dpi>` is the only place DMS takes density from.
const DMS_CONFIG: &str = "vendor/etc/window/resources/display_manager_config.xml";

/// The audio policy's adapters and pipes.
const POLICY_CONFIG: &str = "vendor/etc/audio/audio_policy_config.xml";

/// The audio policy's volume levels. The policy reads the copy in
/// `sys_prod` in place of the image's.
const VOLUME_CONFIG: &str = "etc/audio/audio_volume_config.xml";

/// MMI's mouse acceleration curves. MMI looks the file up in `sys_prod`
/// before `system`.
const POINTER_CONFIG: &str = "etc/multimodalinput/pointer_motion_acceleration_config.json";

/// The density, in pixels per inch, that sizes the panel in millimetres. It
/// is a desktop monitor's density, because the window shows the panel on one.
const PHYSICAL_DPI: f64 = 160.0;

/// A parameter file that makes the bundle manager scan the preinstalled apps
/// at boot, as it does after a system update. Without it, the bundle manager
/// scans them only at a profile's first boot, so a new version of the system
/// app in our layer wouldn't reach an existing profile. The launcher writes
/// the file when it composes the root. It removes the file at a later start
/// once a boot has completed, so a boot that dies before the scan doesn't
/// lose it. init loads the value as a default, which it doesn't save with the
/// persistent parameters.
const UPGRADE_SCAN: &str = "sys_prod/etc/param/innkeeper_upgrade_scan.para";

/// A directory under `/data` that init makes when boot completes
/// (`sys_prod/etc/init/innkeeper_booted.cfg`). The bundle manager scans
/// before boot completes, so the directory shows that the scan ran. The
/// launcher removes it when it composes the root.
const BOOTED: &str = "service/el1/public/innkeeper/booted";

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

/// Returns the physical size in millimetres of a `width` by `height` pixel
/// panel at [`PHYSICAL_DPI`]. DMS derives the x and y dpi that apps see from
/// it.
fn physical_size(width: u32, height: u32) -> (u32, u32) {
    let mm = |px: u32| (f64::from(px) * 25.4 / PHYSICAL_DPI).round() as u32;
    (mm(width), mm(height))
}

/// Returns the ppi that MMI computes from a panel's pixels and physical size
/// (multimodalinput's `MouseTransformProcessor::UpdateMouseMoveLocation`).
/// MMI divides by 25.4 as a float.
fn mmi_ppi(width: u32, height: u32, (mm_width, mm_height): (u32, u32)) -> f64 {
    let diagonal =
        |a: u32, b: u32| (f64::from(a) * f64::from(a) + f64::from(b) * f64::from(b)).sqrt();
    diagonal(width, height) * f64::from(25.4f32) / diagonal(mm_width, mm_height)
}

/// Returns MMI's mouse curve for a panel with a physical size. MMI then
/// scales a unit of motion `v` by `t(v) * speeds[s] * ppi / standard_ppi`,
/// where `t` is `slow[0] * ln(slow[1] * v + slow[2]) + slow[3]` below 13.5
/// and `fast[0] * ln(fast[1] * ln(v) - fast[2]) + fast[3]` above it
/// (`PointerMotionAcceleration::CalcDynamicMouseGain`). With these values each
/// factor is 1, so one unit of motion moves the cursor one panel pixel at
/// every pointer speed. The bridge needs that to place the cursor
/// (host/bridge/src/input/direct.rs), and the host has already accelerated
/// the pointer. The standard ppi is MMI's ppi to the last bit, so the ratio
/// is 1 with no rounding error.
fn pointer_config(width: u32, height: u32, size: (u32, u32)) -> String {
    let speeds = ["1.0"; 20].join(", ");
    let ppi = mmi_ppi(width, height, size);
    format!(
        "{{\n  \"MouseDynamic\": {{\n    \"speeds\": [{speeds}],\n    \
         \"slow_gains\": [0.0, 1.0, 1.0, 1.0],\n    \"fast_gains\": [0.0, 1.0, 0.0, 1.0],\n    \
         \"standard_ppi\": {ppi}\n  }}\n}}\n"
    )
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
        return upgrade_scan(r, &cfg.paths.data, false);
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
    let size = physical_size(w, h);
    let to = r.join("sys_prod").join(POINTER_CONFIG);
    fs::create_dir_all(to.parent().unwrap())?;
    fs::write(&to, pointer_config(w, h, size))?;
    let param = r.join("vendor/etc/param");
    fs::create_dir_all(&param)?;
    fs::write(
        param.join("innkeeper.para"),
        format!(
            "const.innkeeper.display={w}x{h}@{dpi}\nconst.innkeeper.display.size={}x{}\n\
             const.product.udid={udid}\n",
            size.0, size.1
        ),
    )?;
    upgrade_scan(r, &cfg.paths.data, true)?;
    files_read_only(r)?;
    fs::write(&key_file, key)?;
    Ok(())
}

/// Arranges the bundle manager's scan of the preinstalled apps for the next
/// boot if the root was just composed. Otherwise it cancels the scan if a boot
/// of this root has completed since then. `data` is the profile's `/data`.
fn upgrade_scan(root: &Path, data: &Path, composed: bool) -> Result<()> {
    let file = root.join(UPGRADE_SCAN);
    let booted = data.join(BOOTED);
    if composed {
        match fs::remove_dir(&booted) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                return Err(e).with_context(|| format!("remove {}", booted.display()));
            }
            _ => {}
        }
        fs::create_dir_all(file.parent().expect("a relative path with a parent"))?;
        return fs::write(&file, "persist.bms.test-upgrade=true\n")
            .with_context(|| format!("write {}", file.display()));
    }
    if !booted.is_dir() {
        return Ok(());
    }
    match fs::remove_file(&file) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(e).with_context(|| format!("remove {}", file.display()))
        }
        _ => Ok(()),
    }
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
    // While init parses the image's on-demand console service, it opens
    // /dev/console to watch it for input, and it waits 5 s for a missing
    // node. On /dev/null the open succeeds at once. epoll refuses /dev/null,
    // so init doesn't watch it and the console shell doesn't start.
    link(Path::new("/dev/null"), &dev.join("console"))?;
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

    /// Returns the numbers of `key` in an MMI curve file.
    fn numbers(config: &str, key: &str) -> Vec<f64> {
        let from = config.find(&format!("\"{key}\": ")).unwrap() + key.len() + 4;
        let rest = &config[from..];
        let to = rest.find(['\n', ']']).unwrap();
        rest[..to]
            .trim_start_matches('[')
            .split(", ")
            .map(|n| n.parse().unwrap())
            .collect()
    }

    /// Returns where one unit of motion `v` along x moves MMI's cursor from
    /// 0, at pointer speed `speed`, for a panel with a physical size (MMI's
    /// `UpdateMouseMoveLocation`, `ScreenFactor` and `CalcDynamicMouseGain`).
    fn mmi_move(config: &str, (w, h): (u32, u32), size: (u32, u32), v: f64, speed: usize) -> f64 {
        let (speeds, slow, fast) = (
            numbers(config, "speeds"),
            numbers(config, "slow_gains"),
            numbers(config, "fast_gains"),
        );
        let standard = numbers(config, "standard_ppi")[0];
        let mm = (f64::from(size.0).powi(2) + f64::from(size.1).powi(2)).sqrt();
        let factor = match (mm / f64::from(25.4f32)) as i32 {
            ..=0 => 1.0f32,
            1..=7 => 0.7,
            8..=17 => 1.0,
            18..=26 => 1.2,
            27..=54 => 1.6,
            _ => 2.4,
        };
        let ppi = (f64::from(w).powi(2) + f64::from(h).powi(2)).sqrt() * f64::from(25.4f32) / mm;
        let t = if v < 13.5 {
            slow[0] * (slow[1] * v + slow[2]).ln() + slow[3]
        } else {
            fast[0] * (fast[1] * v.ln() - fast[2]).ln() + fast[3]
        };
        let gain = t * speeds[speed - 1] * (ppi / standard);
        v * f64::from(factor) * gain
    }

    #[test]
    fn mmi_moves_the_cursor_one_pixel_per_unit_on_every_panel() {
        for display in innkeeper_profile::profile::System::Ohos.displays() {
            let (w, h, _) = display.screen().unwrap();
            let size = physical_size(w, h);
            let config = pointer_config(w, h, size);
            for speed in 1..=20 {
                for v in [1.0, 2.0, 13.0, 13.5, 14.0, 255.0, 1024.0] {
                    assert_eq!(mmi_move(&config, (w, h), size, v, speed), v, "{display:?}");
                }
            }
        }
    }

    #[test]
    fn panels_have_a_desktop_density() {
        assert_eq!(physical_size(720, 1280), (114, 203));
        assert_eq!(physical_size(1080, 2400), (171, 381));
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
    fn the_scan_stays_arranged_until_a_boot_completes() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../out/test/upgrade-scan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let (root, data) = (dir.join("root"), dir.join("data"));
        let (file, booted) = (root.join(UPGRADE_SCAN), data.join(BOOTED));
        upgrade_scan(&root, &data, false).unwrap();
        assert!(!file.exists(), "no composition, no scan");
        // A completed boot of the previous root left its stamp.
        fs::create_dir_all(&booted).unwrap();
        upgrade_scan(&root, &data, true).unwrap();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            "persist.bms.test-upgrade=true\n"
        );
        assert!(!booted.exists(), "a composition removes the stamp");
        upgrade_scan(&root, &data, false).unwrap();
        assert!(file.exists(), "a boot that didn't complete keeps the scan");
        fs::create_dir_all(&booted).unwrap();
        upgrade_scan(&root, &data, false).unwrap();
        assert!(!file.exists(), "a completed boot cancels the scan");
        upgrade_scan(&root, &data, false).unwrap();
        assert!(!file.exists());
        fs::remove_dir_all(&dir).unwrap();
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
