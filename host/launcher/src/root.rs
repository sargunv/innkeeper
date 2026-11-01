//! Builds Android's root, which holds the partitions, the image's top-level
//! entries, the binder nodes and `/innkeeper`.

use std::fs;
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use flate2::{Compression, write::GzEncoder};

use crate::conf::Config;

/// Every partition, in mount order.
pub const PARTITIONS: &[&str] = &[
    "system",
    "vendor",
    "system_ext",
    "product",
    "odm",
    "system_dlkm",
    "vendor_dlkm",
    "odm_dlkm",
];

/// The partitions our layers go over. The `*_dlkm` partitions come from the
/// image alone.
pub const LAYERED: &[&str] = &["system", "vendor", "system_ext", "product", "odm"];

/// The image's extracted partition `name`. The system partition is
/// system-as-root, so it is under `system/system`.
pub fn image_partition(img: &Path, name: &str) -> PathBuf {
    if name == "system" {
        img.join("system/system")
    } else {
        img.join(name)
    }
}

/// Copies the top-level entries of the system-as-root image, such as the
/// `/bin` symlink, `init.environ.rc` and empty mount points, into `dst` where
/// it lacks them. Symlinks stay symlinks, and files keep their mode and
/// times. Directories are created empty, with the image's mode if
/// `keep_mode` is set and 0755 otherwise.
pub fn copy_top_level(img: &Path, dst: &Path, keep_mode: bool) -> Result<()> {
    let top = img.join("system");
    for e in fs::read_dir(&top).with_context(|| format!("read {}", top.display()))? {
        let e = e?;
        let to = dst.join(e.file_name());
        if to.symlink_metadata().is_ok() {
            continue;
        }
        let t = e.file_type()?;
        if t.is_symlink() {
            symlink(fs::read_link(e.path())?, &to)?;
        } else if t.is_dir() {
            let mode = if keep_mode {
                e.metadata()?.permissions().mode() & 0o7777
            } else {
                0o755
            };
            fs::DirBuilder::new().mode(mode).create(&to)?;
            fs::set_permissions(&to, fs::Permissions::from_mode(mode))?;
        } else {
            copy_file(&e.path(), &to)?;
        }
    }
    Ok(())
}

/// Copies a file like `cp -p`, keeping its contents, mode and times.
pub fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    fs::copy(src, dst).with_context(|| format!("copy {} to {}", src.display(), dst.display()))?;
    let m = fs::metadata(src)?;
    let f = fs::File::open(dst)?;
    let _ = f.set_times(
        fs::FileTimes::new()
            .set_accessed(m.accessed()?)
            .set_modified(m.modified()?),
    );
    Ok(())
}

/// Creates dummy, readable binder nodes in the `/dev` directory `d`. Binder is
/// served in userspace (supervisor/README.md). libbinder checks
/// `access("/dev/binder", R_OK)` in selfIfKernelBinderEnabled(), and the
/// supervisor intercepts real opens and injects a memfd. The nodes are made
/// before the seccomp filter is installed, because init's own open of
/// /dev/binder is intercepted and so init can't create them.
/// `binderfs/features` lists the driver features that binderfs would report,
/// which libbinder reads. The broker implements each of them.
pub fn binder_nodes(d: &Path) -> Result<()> {
    for n in ["binder", "hwbinder", "vndbinder"] {
        fs::write(d.join(n), b"")?;
        fs::set_permissions(d.join(n), fs::Permissions::from_mode(0o666))?;
    }
    let f = d.join("binderfs/features");
    fs::create_dir_all(&f)?;
    for n in [
        "oneway_spam_detection",
        "extended_error",
        "freeze_notification",
    ] {
        fs::write(f.join(n), b"1\n")?;
    }
    Ok(())
}

/// Writes `/innkeeper/`, the files the session fakes for Android. The shim,
/// the broker and our init scripts read them.
///  - `apexd-mounts`: the /proc/mounts lines apexd would see if it had
///    mounted each APEX itself as file-backed erofs
///    (`<file> /apex/<name>@<version>`). /system/bin/innkeeper-apexd reads
///    it.
///  - `proc/`: stand-ins for /proc files. cmdline and bootconfig carry our
///    `androidboot.*` values. init fails if it can't raise ASLR entropy, so
///    it gets a writable mmap_rnd_bits. The host's setting still applies. The
///    directory also holds the BPF sysctls that netbpfload sets, which the
///    broker reads.
///  - `config.gz`: a kernel config for code that wants /proc/config.gz
///    (supervisor/shim).
pub fn innkeeper_dir(cfg: &Config, l: &Path) -> Result<()> {
    let proc = l.join("proc");
    fs::create_dir_all(proc.join("sys/vm"))?;
    let info = cfg.image.apexdir.join("apex-info-list.xml");
    let info = fs::read_to_string(&info).with_context(|| format!("read {}", info.display()))?;
    fs::write(l.join("apexd-mounts"), apexd_mounts(&info))?;
    for f in ["cmdline", "bootconfig"] {
        fs::copy(cfg.paths.run.join(f), proc.join(f))?;
    }
    fs::write(proc.join("sys/vm/mmap_rnd_bits"), "32\n")?;

    fs::create_dir_all(proc.join("sys/kernel"))?;
    fs::create_dir_all(proc.join("sys/net/core"))?;
    fs::write(proc.join("sys/kernel/unprivileged_bpf_disabled"), "2\n")?;
    fs::write(proc.join("sys/net/core/bpf_jit_enable"), "1\n")?;
    fs::write(proc.join("sys/net/core/bpf_jit_kallsyms"), "0\n")?;
    // The sandbox can't read /boot. The config holds only the option that
    // system_server reads, with the value every x86_64 distro kernel uses.
    let mut gz = GzEncoder::new(
        fs::File::create(l.join("config.gz"))?,
        Compression::default(),
    );
    gz.write_all(b"CONFIG_VMAP_STACK=y\n")?;
    gz.finish()?;
    Ok(())
}

/// apex-info-list.xml's APEXes as /proc/mounts lines.
fn apexd_mounts(info: &str) -> String {
    let attr = |el: &str, name: &str| -> Option<String> {
        let i = el.find(&format!(" {name}=\""))? + name.len() + 3;
        Some(el[i..].split('"').next()?.to_string())
    };
    let mut s = String::new();
    for el in info.split("<apex-info").skip(1) {
        let el = el.split("/>").next().unwrap_or(el);
        if let (Some(n), Some(p), Some(v)) = (
            attr(el, "moduleName"),
            attr(el, "modulePath"),
            attr(el, "versionCode"),
        ) {
            s += &format!("{p} /apex/{n}@{v} erofs ro,fsoffset=0 0 0\n");
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apex_mounts() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<apex-info-list>
    <apex-info moduleName="com.android.art" modulePath="/system/apex/com.android.art.capex" preinstalledModulePath="/system/apex/com.android.art.capex" versionCode="1" versionName="" isFactory="true" isActive="true"/>
    <apex-info moduleName="com.android.vndk" modulePath="/vendor/apex/v.apex" versionCode="370000" isActive="true"/>
</apex-info-list>
"#;
        assert_eq!(
            apexd_mounts(xml),
            "/system/apex/com.android.art.capex /apex/com.android.art@1 erofs ro,fsoffset=0 0 0\n\
             /vendor/apex/v.apex /apex/com.android.vndk@370000 erofs ro,fsoffset=0 0 0\n"
        );
    }
}
