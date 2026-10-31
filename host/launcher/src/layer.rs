//! What `start` writes into the runtime dir for one boot: the start-time
//! layer, the top of every partition's stack, and the fake `/proc/cmdline`
//! and `/proc/bootconfig`.
//!
//! The layer holds:
//! - `vendor/build.prop`: the image's, plus every `props.d/*.prop` (graphics,
//!   audio), the driver selection, the composer backend and
//!   image/vendor.prop. RADV for amdgpu, ANV for i915, else lavapipe.
//! - `vendor/etc/permissions/`: Vulkan features matching that driver.
//! - `apex/<name>/`: our files for the APEXes this image has (the stage's,
//!   then the addons'), so the tree gains no directories apexd and
//!   linkerconfig don't know about.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::conf::Config;

/// `androidboot.*` values (passed through /proc/bootconfig) become `ro.boot.*`
/// properties.
const CMDLINE: &[&str] = &[
    "androidboot.hardware=innkeeper",
    "androidboot.selinux=permissive",
    "androidboot.verifiedbootstate=orange",
    "androidboot.veritymode=disabled",
    "androidboot.serialno=innkeeper0",
    "androidboot.debuggable=1",
    "androidboot.boot_devices=innkeeper",
    "androidboot.dalvik.vm.heapsize=512m",
];

/// Our properties: what vendor/build.prop gets on top of the image's.
fn our_props(cfg: &Config) -> Result<String> {
    let share = &cfg.paths.share;
    if !share.join("props.d/vulkan.prop").is_file() {
        bail!(
            "no graphics properties ({})",
            share.join("props.d/vulkan.prop").display()
        );
    }
    let mut props: Vec<_> = fs::read_dir(share.join("props.d"))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "prop"))
        .collect();
    props.sort();
    let mut s = String::new();
    for p in props {
        s += &format!(
            "# --- innkeeper-session: {} ---\n",
            p.file_name().unwrap().to_string_lossy()
        );
        for l in fs::read_to_string(&p)?
            .lines()
            .filter(|l| !l.starts_with('#'))
        {
            s += l;
            s.push('\n');
        }
    }
    let vk = &cfg.vulkan_driver;
    s += &format!("ro.hardware.vulkan={vk}\n");
    // Composer backend: windows through the bridge, or none.
    let backend = if cfg.headless { "headless" } else { "bridge" };
    s += &format!("vendor.innkeeper.hwc.backend={backend}\n");

    // No mounts, so zygote can't hide other apps' data dirs.
    s += "persist.zygote.app_data_isolation=false\n";
    // The host's network is Android's (the system app's
    // HostNetworkFeature, which follows the sandbox's resolv.conf).
    s += "# --- innkeeper-session: image/vendor.prop ---\n";
    s += &fs::read_to_string(share.join("image/vendor.prop"))?;
    Ok(s)
}

/// `image`'s lines minus every key `ours` sets, then `ours`, so ours take
/// effect.
fn merge_props(image: &str, ours: &str) -> String {
    let key = |l: &str| l.split('=').next().unwrap_or("").to_string();
    let mine: HashSet<String> = ours
        .lines()
        .filter(|l| !l.starts_with('#') && l.contains('='))
        .map(key)
        .collect();
    let mut s = String::new();
    for l in image.lines().filter(|l| !mine.contains(&key(l))) {
        s += l;
        s.push('\n');
    }
    s.push('\n');
    s += ours.trim_end_matches('\n');
    s.push('\n');
    s
}

/// Copies the tree `src` into `dst` (`cp -a`: modes, owners where possible,
/// xattrs, links).
pub fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let st = Command::new("cp")
        .arg("-a")
        .arg(src)
        .arg(dst)
        .status()
        .context("run cp")?;
    if !st.success() {
        bail!("cp -a {} {} failed", src.display(), dst.display());
    }
    Ok(())
}

/// Writes the start-time layer, `cmdline` and `bootconfig` into the runtime
/// dir.
pub fn write(cfg: &Config) -> Result<()> {
    let run = &cfg.paths.run;
    let layer = cfg.run_layer();
    fs::create_dir_all(layer.join("vendor"))?;
    fs::create_dir_all(layer.join("apex"))?;
    let permissions = layer.join("vendor/etc/permissions");
    fs::create_dir_all(&permissions)?;
    for (name, xml) in crate::graphics::feature_xmls(&cfg.vulkan_driver) {
        fs::write(permissions.join(name), xml)?;
    }
    let image_props = cfg.image.img.join("vendor/build.prop");
    let image_props = fs::read_to_string(&image_props)
        .with_context(|| format!("read {}", image_props.display()))?;
    let bp = layer.join("vendor/build.prop");
    let ours = our_props(cfg)? + "# --- innkeeper-session: the profile's adb ---\n";
    let ours = ours + &crate::adb::prepare(&cfg.paths)?;
    let ours = ours
        + "# --- innkeeper-session: the profile's mode ---\n"
        + &crate::phone::props(&crate::profile::Settings::load(&cfg.paths.data)?);
    fs::write(&bp, merge_props(&image_props, &ours))?;
    fs::set_permissions(&bp, fs::Permissions::from_mode(0o644))?;
    let mut srcs = vec![cfg.paths.android().join("apex")];
    srcs.extend(cfg.paths.addon_layers().into_iter().map(|a| a.join("apex")));
    for src in srcs {
        let Ok(dir) = fs::read_dir(&src) else {
            continue;
        };
        let mut dirs: Vec<_> = dir
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        dirs.sort();
        for d in dirs {
            if cfg.image.apexdir.join(d.file_name().unwrap()).is_dir() {
                copy_tree(&d, &layer.join("apex"))?;
            }
        }
    }

    fs::write(run.join("cmdline"), "console=null\n")?;
    let mut bc = String::new();
    let extra = cfg.paths.image_bootconfig()?;
    for t in CMDLINE
        .iter()
        .copied()
        .chain(extra.iter().map(String::as_str))
    {
        if let Some((k, v)) = t
            .split_once('=')
            .filter(|(k, _)| k.starts_with("androidboot."))
        {
            bc += &format!("{k} = \"{v}\"\n");
        }
    }
    fs::write(run.join("bootconfig"), bc)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_override_the_image() {
        let image = "# image\nro.a=1\nro.b=2\nro.c\n";
        let ours = "# --- ours ---\nro.b=3\n\nro.d=4\n";
        assert_eq!(
            merge_props(image, ours),
            "# image\nro.a=1\nro.c\n\n# --- ours ---\nro.b=3\n\nro.d=4\n"
        );
        // The shipped override silences absent tracefs without exposing host tracing.
        let merged = merge_props(
            "log.tag.cutils-trace=E\n",
            include_str!("../../../image/vendor.prop"),
        );
        assert_eq!(
            merged
                .lines()
                .filter(|l| l.starts_with("log.tag.cutils-trace="))
                .collect::<Vec<_>>(),
            ["log.tag.cutils-trace=S"]
        );
    }
}
