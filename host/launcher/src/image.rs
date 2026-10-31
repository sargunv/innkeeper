//! Preparing the pinned image: fetched and extracted, its APEXes flattened,
//! its files read-only, and the derived layers current (image/README.md).

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::paths::{Image, Paths};

const DATA_VERSION: &str = ".innkeeper-data-version";

/// Order Google's CI frameworks by SDK, then numeric build ID, never by the
/// cache directory's name. Read the actual system image, not the pin's label.
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

/// An attempted boot may already have migrated /data before it fails. Keep
/// that floor as well as the last success; retrying it or upgrading is safe.
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

/// Read-only gate, also in the calling sandbox so background start reports
/// the actionable error directly. Preparing an image alone never adopts data.
pub fn check_data(data: &Path, image: &Image) -> Result<()> {
    let current = Framework::read(image)?;
    if let Some(state) = DataVersion::load(data)? {
        state.check(data, &current)?;
    }
    Ok(())
}

/// Called before any setup of /data or Android process. Persist the attempt
/// before exposing data, and give init an atomic replacement to commit at
/// sys.boot_completed=1. An interrupted boot keeps the old `booted` value.
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
        // Another profile may have won the race: fine.
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
