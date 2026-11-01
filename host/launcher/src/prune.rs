//! Garbage collection for the image cache, serialized with preparation.
//! Leases on builds and APEX trees are lock files outside the collected
//! trees. They stay locked from prepare until the session exits, including
//! while a session is starting and has no control socket yet. All guests
//! share the cache, so each guest's pinned image is kept no matter which
//! guest prepares.

use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::paths::{Image, Paths};

pub struct Lease {
    pub was_in_use: bool,
    _build: File,
    /// Android's APEX tree. OpenHarmony's image has none.
    _apex: Option<File>,
}

fn lock_file(path: &Path) -> Result<File> {
    fs::create_dir_all(path.parent().unwrap())?;
    Ok(File::options().create(true).append(true).open(path)?)
}

/// Lock files are never unlinked, because another process may already have
/// one open.
fn build_lock(images: &Path, build: &Path) -> PathBuf {
    images.join(".locks").join(build.file_name().unwrap())
}

pub fn prepare_lock(images: &Path) -> Result<File> {
    let f = lock_file(&images.join(".locks/prepare"))?;
    f.lock()?;
    Ok(f)
}

pub fn lease(paths: &Paths, image: &Image) -> Result<Lease> {
    let dir = build_lock(&paths.images, &image.dir);
    let was_in_use = unused(&dir.join("image"))?.is_none()
        || running_images(paths)?.iter().any(|i| i.dir == image.dir);
    let build = lock_file(&dir.join("image"))?;
    build.lock_shared()?;
    let apex = image.apexdir.file_name().unwrap().to_string_lossy();
    let apex = if apex_tree(&apex) {
        let f = lock_file(&dir.join(&*apex))?;
        f.lock_shared()?;
        Some(f)
    } else {
        None
    };
    Ok(Lease {
        was_in_use,
        _build: build,
        _apex: apex,
    })
}

fn unused(path: &Path) -> Result<Option<File>> {
    let f = lock_file(path)?;
    match f.try_lock() {
        Ok(()) => Ok(Some(f)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(e.into()),
    }
}

/// The image that a running session's `session.conf` names. Only the image
/// table is read, because the session may run another Innkeeper build whose
/// config differs. A session that answers on its socket but has an
/// unreadable config aborts pruning.
fn running_image(conf: &Path) -> Result<Image> {
    #[derive(serde::Deserialize)]
    struct Running {
        image: Image,
    }
    let s = fs::read_to_string(conf).with_context(|| format!("read {}", conf.display()))?;
    Ok(toml::from_str::<Running>(&s)
        .with_context(|| format!("parse {}", conf.display()))?
        .image)
}

fn running_images(paths: &Paths) -> Result<Vec<Image>> {
    let mut images = vec![];
    let Some(runtime) = paths.run.parent() else {
        return Ok(images);
    };
    let entries = match fs::read_dir(runtime) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(images),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("innkeeper-") {
            continue;
        }
        let run = entry.path();
        if crate::control::status(&run.join("control.sock"))
            .is_some_and(|s| s.starts_with("running:"))
        {
            images.push(running_image(&run.join("session.conf"))?);
        }
    }
    Ok(images)
}

/// Every guest's pinned image, which prune keeps.
pub fn pinned(paths: &Paths) -> Result<Vec<Image>> {
    crate::profile::System::ALL
        .iter()
        .map(|s| paths.image_of(*s))
        .collect()
}

fn selected(path: &Path, pinned: &[Image], running: &[Image]) -> bool {
    pinned.iter().chain(running).any(|image| {
        // A build cannot be removed while any of its trees is selected.
        image.dir == path || image.apexdir == path || image.derived == path
    })
}

fn apex_tree(name: &str) -> bool {
    name.strip_prefix("apex-")
        .is_some_and(|h| h.len() == 12 && h.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// Allocated bytes that unlinking this tree releases. Hardlinks still held by
/// composed roots do not free their file's blocks. Symlinks are not followed.
fn freed_bytes(path: &Path) -> Result<u64> {
    let m = fs::symlink_metadata(path)?;
    let mut bytes = if m.nlink() == 1 || m.is_dir() {
        m.blocks() * 512
    } else {
        0
    };
    if m.is_dir() {
        for e in fs::read_dir(path)? {
            bytes += freed_bytes(&e?.path())?;
        }
    }
    Ok(bytes)
}

fn remove(path: &Path) -> Result<()> {
    let bytes = freed_bytes(path)?;
    crate::sandbox::remove_tree(path)?;
    eprintln!(
        "innkeeper-session: pruned {} (freed {bytes} bytes)",
        path.display()
    );
    Ok(())
}

/// Must be called under the cache's prepare lock, with the current lease
/// held. Keeps the `pinned` images ([`pinned`]) and those of running
/// sessions. Unknown directories, symlinks and unfinished APEX extractions
/// are left alone.
pub fn prune(paths: &Paths, pinned: &[Image]) -> Result<()> {
    let running = running_images(paths).context("find running images before pruning")?;
    for entry in fs::read_dir(&paths.images)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || entry.file_name() == ".locks" {
            continue;
        }
        let build = entry.path();
        // Skip directories that are not image-cache builds.
        if !build.join("img").is_dir() && !build.join("derived").is_dir() {
            continue;
        }
        let locks = build_lock(&paths.images, &build);
        if !selected(&build, pinned, &running)
            && let Some(_unused) = unused(&locks.join("image"))?
        {
            remove(&build)?;
            continue;
        }
        for tree in fs::read_dir(&build)? {
            let tree = tree?;
            if !tree.file_type()?.is_dir() || !apex_tree(&tree.file_name().to_string_lossy()) {
                continue;
            }
            let path = tree.path();
            if !selected(&path, pinned, &running)
                && let Some(_unused) = unused(&locks.join(tree.file_name()))?
            {
                remove(&path)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_the_image_of_a_session_from_another_build() {
        let dir =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/test/prune-conf");
        std::fs::create_dir_all(&dir).unwrap();
        let conf = dir.join("session.conf");
        // A config whose other fields this build doesn't know.
        std::fs::write(
            &conf,
            "headless = true\n[paths]\ninstance = \"old\"\n[image]\nname = \"ci-1\"\ndir = \"/i\"\nimg = \"/i/img\"\napexdir = \"/i/apex\"\nderived = \"/i/derived\"\n",
        )
        .unwrap();
        assert_eq!(super::running_image(&conf).unwrap().name, "ci-1");
        assert!(crate::conf::Config::load(&conf).is_err());
    }

    use super::*;

    fn image(name: &str, hash: &str) -> Image {
        let dir = PathBuf::from("images").join(name);
        Image {
            name: name.into(),
            img: dir.join("img"),
            derived: dir.join("derived"),
            apexdir: dir.join(format!("apex-{hash}")),
            dir,
        }
    }

    #[test]
    fn selection_preserves_current_and_running_builds_and_apexes() {
        let current = [image("ci-new", "aaaaaaaaaaaa")];
        let running = [
            image("ci-old", "bbbbbbbbbbbb"),
            image("ci-new", "cccccccccccc"),
        ];
        for path in [
            "images/ci-new",
            "images/ci-old",
            "images/ci-new/derived",
            "images/ci-old/derived",
            "images/ci-new/apex-aaaaaaaaaaaa",
            "images/ci-old/apex-bbbbbbbbbbbb",
            "images/ci-new/apex-cccccccccccc",
        ] {
            assert!(selected(Path::new(path), &current, &running), "{path}");
        }
        for path in [
            "images/ci-stale",
            "images/ci-old/apex-dddddddddddd",
            "images/ci-new/apex-eeeeeeeeeeee",
        ] {
            assert!(!selected(Path::new(path), &current, &running), "{path}");
        }
        assert!(apex_tree("apex-012345abcdef"));
        assert!(!apex_tree("apex-012345abcdef.tmp-test"));
        assert!(!apex_tree("apex-link"));
    }

    #[test]
    fn prune_skips_leased_trees_then_collects_them_after_exit() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let base = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("../../out/test/prune-{}", std::process::id()));
        fs::create_dir_all(&base)?;
        let base = base.canonicalize()?;
        let images = base.join("images");
        let paths = Paths {
            profile: crate::profile::ProfileName::new("prune-test").unwrap(),
            libexec: base.join("libexec"),
            share: base.join("share"),
            run: base.join("runtime/innkeeper-prune-test"),
            state: base.join("state"),
            data_home: base.clone(),
            data: base.join("data"),
            images: images.clone(),
            root: base.join("root"),
        };
        let make = |name, hash| {
            let mut i = image(name, hash);
            i.dir = images.join(name);
            i.img = i.dir.join("img");
            i.derived = i.dir.join("derived");
            i.apexdir = i.dir.join(format!("apex-{hash}"));
            i
        };
        let current = make("ci-new", "aaaaaaaaaaaa");
        let old = make("ci-old", "bbbbbbbbbbbb");
        let same_build = make("ci-new", "cccccccccccc");
        let stale = make("ci-stale", "dddddddddddd");
        // The other guest's pinned image has no APEX tree, so it has no APEX
        // lease.
        let mut ohos = make("oniro-1", "");
        ohos.apexdir = ohos.dir.join("apex");
        fs::create_dir_all(&ohos.img)?;
        let pinned = [current.clone(), ohos.clone()];
        for i in [&current, &old, &same_build, &stale] {
            fs::create_dir_all(&i.img)?;
            fs::create_dir_all(&i.derived)?;
            fs::create_dir_all(&i.apexdir)?;
            fs::write(i.apexdir.join("payload"), b"stale blocks")?;
        }
        let sentinel = base.join("sentinel");
        fs::create_dir_all(&sentinel)?;
        fs::write(sentinel.join("keep"), b"outside cache")?;
        symlink(&sentinel, stale.dir.join("link"))?;
        symlink(&sentinel, images.join("symlink-build"))?;
        fs::create_dir_all(images.join("unknown"))?;
        let partial = current.dir.join("apex-eeeeeeeeeeee.tmp-prune-test");
        fs::create_dir_all(&partial)?;
        fs::set_permissions(&stale.apexdir, fs::Permissions::from_mode(0o555))?;
        let _prepare = prepare_lock(&images)?;
        let _current = lease(&paths, &current)?;
        let old_lease = lease(&paths, &old)?;
        let same_lease = lease(&paths, &same_build)?;
        assert!(same_lease.was_in_use);
        drop(lease(&paths, &ohos)?);
        assert!(!images.join(".locks/oniro-1/apex").exists());
        prune(&paths, &pinned)?;
        assert!(!stale.dir.exists());
        assert!(old.apexdir.is_dir());
        assert!(old.derived.is_dir());
        assert!(same_build.apexdir.is_dir());
        drop(old_lease);
        drop(same_lease);
        prune(&paths, &pinned)?;
        assert!(ohos.img.is_dir());
        prune(&paths, &pinned[..1])?;
        assert!(!ohos.dir.exists());
        assert!(!old.dir.exists());
        assert!(!same_build.apexdir.exists());
        assert!(current.apexdir.is_dir());
        assert!(current.derived.is_dir());
        assert!(partial.is_dir());
        assert!(images.join("unknown").is_dir());
        assert!(images.join("symlink-build").is_symlink());
        assert!(sentinel.join("keep").is_file());
        crate::sandbox::remove_tree(&base)?;
        Ok(())
    }
}
