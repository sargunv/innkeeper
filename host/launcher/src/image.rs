//! Prepares the pinned Android image. It fetches and extracts the image,
//! flattens its APEXes, makes its files read-only and brings the derived layer
//! up to date. See image/README.md.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::{Image, Paths};
use crate::profile::System;

const DATA_VERSION: &str = ".innkeeper-data-version";

/// An AOSP CI framework build, read from the system image's build.prop.
/// Builds are ordered by SDK and then by numeric build ID. The cache
/// directory's name and the pin's label play no part in the order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Framework {
    image: String,
    sdk: u32,
    build: u64,
}

impl Framework {
    fn read(image: &Image) -> Result<Self> {
        let props = fs::read_to_string(image.img.join("system/system/build.prop"))?;
        let prop = |key| crate::field(&props, key).with_context(|| format!("missing {key}"));
        Ok(Self {
            image: image.name.clone(),
            sdk: prop("ro.build.version.sdk")?
                .parse()
                .context("non-numeric Android SDK")?,
            build: prop("ro.build.version.incremental")?
                .parse()
                .context("non-numeric AOSP CI build ID")?,
        })
    }

    fn version(&self) -> (u32, u64) {
        (self.sdk, self.build)
    }
}

/// The frameworks that have opened /data. A boot attempt may migrate /data
/// before it fails, so the record keeps the last attempt as well as the last
/// successful boot. Starting the attempted framework again or a newer one is
/// safe.
#[derive(Debug, Serialize, Deserialize)]
struct DataVersion {
    started: Framework,
    booted: Option<Framework>,
}

impl DataVersion {
    fn load(data: &Path) -> Result<Option<Self>> {
        let path = data.join(DATA_VERSION);
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map(Some).with_context(|| format!(
                "cannot read data version {}; restore a complete /data backup (docs/session.md)",
                path.display()
            )),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e).with_context(|| format!("read {}", path.display())),
        }
    }

    fn check(&self, data: &Path, current: &Framework) -> Result<()> {
        for previous in std::iter::once(&self.started).chain(&self.booted) {
            if current.version() < previous.version() {
                bail!(
                    "refusing Android data downgrade: {} was opened by {} (SDK {}, CI build {}), \
                     newer than selected {} (SDK {}, CI build {}). \
                     A failed boot may already have migrated /data. \
                     Start {} or a newer framework, restore a complete pre-upgrade /data backup, \
                     or move this data directory aside to start fresh. \
                     Changing the image alone cannot undo a data migration. See docs/session.md",
                    data.display(),
                    previous.image,
                    previous.sdk,
                    previous.build,
                    current.image,
                    current.sdk,
                    current.build,
                    previous.image
                );
            }
        }
        Ok(())
    }
}

/// Fails if `image` is older than a framework that has opened `data`, and
/// writes nothing. `start` also runs it in the calling sandbox so that a
/// background start reports the error directly. Preparing an image does not
/// record a data version.
pub fn check_data(data: &Path, image: &Image) -> Result<()> {
    let current = Framework::read(image)?;
    if let Some(state) = DataVersion::load(data)? {
        state.check(data, &current)?;
    }
    Ok(())
}

/// Records a boot attempt. It must run before /data is set up or any Android
/// process starts. The attempt is persisted before data is exposed, and init
/// gets a replacement record to commit atomically at sys.boot_completed=1. An
/// interrupted boot keeps the old `booted` value.
pub fn begin_boot(data: &Path, image: &Image, innkeeper: &Path) -> Result<()> {
    let current = Framework::read(image)?;
    let previous = DataVersion::load(data)?;
    if let Some(state) = &previous {
        state.check(data, &current)?;
    }
    let record_attempt = previous
        .as_ref()
        .is_none_or(|s| s.started != current || s.booted.as_ref() != Some(&current));
    let mut state = DataVersion {
        started: current.clone(),
        booted: previous.and_then(|s| s.booted),
    };
    fs::create_dir_all(data)?;
    // A completed boot of this framework already has a durable attempt.
    // An interrupted attempt may have stopped between rename and directory
    // sync, so retries must reestablish durability before Android runs.
    if record_attempt {
        let tmp = data.join(format!("{DATA_VERSION}.next"));
        let mut f = fs::File::create(&tmp)?;
        f.write_all(toml::to_string(&state)?.as_bytes())?;
        f.sync_all()?;
        fs::rename(tmp, data.join(DATA_VERSION))?;
        fs::File::open(data)?.sync_all()?;
    }
    state.booted = Some(current);
    fs::write(
        innkeeper.join("data-version-booted"),
        toml::to_string(&state)?,
    )?;
    Ok(())
}

fn run(c: &mut Command) -> Result<()> {
    let st = c
        .status()
        .with_context(|| format!("run {:?}", c.get_program()))?;
    if !st.success() {
        bail!("{:?} failed ({st})", c.get_program());
    }
    Ok(())
}

/// Makes every file under `dir` read-only (a-w). `last` goes after all others,
/// so a read-only `last` marks a finished tree. Composed roots hardlink these
/// files, so this keeps a session from writing to the cache through them. ART
/// also refuses dex files that its process could write.
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

/// Describes the inputs of the derived layer, which are the APEX tree and the
/// hash of derive.sh.
fn derive_source(paths: &Paths, image: &Image) -> Result<String> {
    let script = fs::read(paths.libexec.join("image/derive.sh"))?;
    let h: String = Sha256::digest(&script)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(format!("{} {h}  -", image.apexdir.display()))
}

/// Fetches, extracts and flattens the image if needed, and brings the derived
/// layer up to date.
pub fn prepare(paths: &Paths, image: &Image) -> Result<crate::prune::Lease> {
    let _prepare = crate::prune::prepare_lock(&paths.images)?;
    let lease = crate::prune::lease(paths, image)?;
    let tools = paths.libexec.join("image");
    if !image.img.join("system/system").is_dir() {
        fetch(paths, System::Android)?;
    }
    make_read_only(&image.img, &image.img.join("system/system/build.prop"))?;
    let info = image.apexdir.join("apex-info-list.xml");
    if !info.is_file() {
        let sfx = crate::paths::per_profile("", &paths.profile);
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
        // Another profile may have won the race, which is fine.
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
    let source = image.derived_source();
    if !fs::read_to_string(&source).is_ok_and(|s| s.trim_end_matches('\n') == want) {
        if lease.was_in_use {
            bail!(
                "the derived image layer is in use; stop sessions using {} before preparing a changed layer",
                image.name
            );
        }
        run(Command::new(tools.join("derive.sh"))
            .arg(&image.img)
            .arg(&image.apexdir)
            .arg(&image.derived))?;
        fs::write(&source, format!("{want}\n"))?;
    }
    crate::prune::prune(paths, &crate::prune::pinned(paths)?)?;
    Ok(lease)
}

/// Downloads and extracts `system`'s pinned image (image/fetch.sh).
pub fn fetch(paths: &Paths, system: System) -> Result<()> {
    run(Command::new(paths.libexec.join("image/fetch.sh"))
        .arg(system.name())
        .arg(paths.pin(system))
        .arg(&paths.images))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    fn framework(sdk: u32, build: u64) -> Framework {
        Framework {
            image: format!("ci-{build}"),
            sdk,
            build,
        }
    }

    #[test]
    fn framework_order_is_numeric_and_sdk_first() {
        assert!(framework(37, 9).version() < framework(37, 10).version());
        assert!(framework(36, 100).version() < framework(37, 1).version());
        let mut renamed = framework(37, 10);
        renamed.image = "other-cache-name".into();
        assert_eq!(renamed.version(), framework(37, 10).version());
    }

    #[test]
    fn start_allows_adoption_upgrade_and_retry_but_not_downgrade() {
        let data = Path::new("test-data");
        let old = framework(37, 9);
        let new = framework(37, 10);
        let mut state = DataVersion {
            started: old.clone(),
            booted: Some(old.clone()),
        };
        assert!(state.check(data, &old).is_ok());
        assert!(state.check(data, &new).is_ok());
        // The new framework may migrate data before boot completes.
        state.started = new.clone();
        assert!(state.check(data, &new).is_ok());
        assert!(state.check(data, &old).is_err());
        assert!(state.check(data, &framework(38, 1)).is_ok());
        state.booted = Some(new);
        assert!(state.check(data, &old).is_err());
    }

    #[test]
    fn boot_record_survives_interruption_and_refusal_is_read_only() -> Result<()> {
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
            "../../out/test/data-version-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&base);
        let data = base.join("data");
        let innkeeper = base.join("innkeeper");
        let mut image = Image {
            name: "ci-9".into(),
            dir: base.clone(),
            img: base.join("img"),
            apexdir: base.join("apex"),
            derived: base.join("derived"),
        };
        fs::create_dir_all(image.img.join("system/system"))?;
        fs::create_dir_all(&innkeeper)?;
        let props = image.img.join("system/system/build.prop");
        fs::write(
            &props,
            "ro.build.version.sdk=37\nro.build.version.incremental=9\n",
        )?;
        check_data(&data, &image)?;
        assert!(!data.exists());
        begin_boot(&data, &image, &innkeeper)?;
        assert!(DataVersion::load(&data)?.unwrap().booted.is_none());
        fs::copy(
            innkeeper.join("data-version-booted"),
            data.join(DATA_VERSION),
        )?;
        // A cached restart already has a durable attempt for this framework.
        // Keep the original inode open so inode reuse cannot hide a rewrite.
        let record = fs::File::open(data.join(DATA_VERSION))?;
        begin_boot(&data, &image, &innkeeper)?;
        assert_eq!(
            fs::metadata(data.join(DATA_VERSION))?.ino(),
            record.metadata()?.ino()
        );
        assert_eq!(
            DataVersion::load(&data)?.unwrap().booted,
            Some(framework(37, 9))
        );
        fs::write(data.join("app-data"), b"preserve me")?;
        image.name = "ci-10".into();
        fs::write(
            &props,
            "ro.build.version.sdk=37\nro.build.version.incremental=10\n",
        )?;
        begin_boot(&data, &image, &innkeeper)?;
        assert_eq!(
            DataVersion::load(&data)?.unwrap().booted,
            Some(framework(37, 9))
        );
        let record = fs::File::open(data.join(DATA_VERSION))?;
        begin_boot(&data, &image, &innkeeper)?; // Retry an interrupted upgrade.
        assert_ne!(
            fs::metadata(data.join(DATA_VERSION))?.ino(),
            record.metadata()?.ino()
        );
        assert_eq!(
            DataVersion::load(&data)?.unwrap().booted,
            Some(framework(37, 9))
        );
        fs::copy(
            innkeeper.join("data-version-booted"),
            data.join(DATA_VERSION),
        )?;
        assert_eq!(
            DataVersion::load(&data)?.unwrap().booted,
            Some(framework(37, 10))
        );
        let before = fs::read(data.join(DATA_VERSION))?;
        fs::write(
            &props,
            "ro.build.version.sdk=37\nro.build.version.incremental=9\n",
        )?;
        let error = begin_boot(&data, &image, &innkeeper)
            .unwrap_err()
            .to_string();
        assert!(error.contains("refusing Android data downgrade"));
        assert!(error.contains("pre-upgrade /data backup"));
        assert_eq!(fs::read(data.join(DATA_VERSION))?, before);
        assert_eq!(fs::read(data.join("app-data"))?, b"preserve me");
        fs::write(data.join(DATA_VERSION), "broken record")?;
        assert!(check_data(&data, &image).is_err());
        fs::remove_dir_all(base)?;
        Ok(())
    }
}
