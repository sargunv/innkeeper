//! Input files. An input is a plain `.apk` or a split-APK bundle, such as an
//! `.apks` from bundletool, an `.xapk` or an `.apkm`. All of them are zip
//! archives of APKs.
//!
//! Bundle members are extracted with generated names into a private work
//! directory under the user's cache dir. The work directory is not in `/tmp`,
//! which is RAM-backed on the dev host. Entry names are untrusted, so they
//! never become paths. Sizes are capped to guard against zip bombs.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use crate::ErrorKind;
use anyhow::{Context, Result, bail};

/// Per-APK and total extraction caps.
const MAX_APK: u64 = 4 << 30;
const MAX_TOTAL: u64 = 8 << 30;

/// One APK to install, with the zip entry it came from.
#[derive(Debug, Clone)]
pub struct Member {
    pub path: PathBuf,
    /// The zip entry name, or the file path for a plain APK. It is used only
    /// in messages.
    pub origin: String,
}

/// The APKs of one input, and the work dir to delete afterwards.
pub struct Input {
    pub apks: Vec<Member>,
    pub skipped: Vec<String>,
    work: Option<PathBuf>,
}

impl Drop for Input {
    fn drop(&mut self) {
        if let Some(w) = &self.work {
            let _ = fs::remove_dir_all(w);
        }
    }
}

fn is_zip(path: &Path) -> Result<bool> {
    let mut f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut magic = [0u8; 4];
    Ok(f.read_exact(&mut magic).is_ok() && magic == *b"PK\x03\x04")
}

/// Chooses which entries of a bundle to install. A bundletool `.apks`
/// installs `splits/*.apk`, or else `universal.apk`. Any other bundle, such as
/// an `.xapk` or `.apkm`, installs its APKs at any depth outside
/// `standalones/`, which holds APKs for pre-L devices. OBB files are reported
/// as skipped.
pub fn select_entries(names: &[String]) -> (Vec<String>, Vec<String>) {
    let apks: Vec<&String> = names
        .iter()
        .filter(|n| n.to_ascii_lowercase().ends_with(".apk") && !n.ends_with('/'))
        .collect();
    let skipped: Vec<String> = names
        .iter()
        .filter(|n| n.to_ascii_lowercase().ends_with(".obb"))
        .cloned()
        .collect();
    let splits: Vec<String> = apks
        .iter()
        .filter(|n| n.starts_with("splits/"))
        .map(|s| s.to_string())
        .collect();
    if !splits.is_empty() {
        return (splits, skipped);
    }
    if apks.iter().any(|n| *n == "universal.apk") {
        return (vec!["universal.apk".into()], skipped);
    }
    let v = apks
        .iter()
        .filter(|n| !n.starts_with("standalones/"))
        .map(|s| s.to_string())
        .collect();
    (v, skipped)
}

/// Opens `path`. A plain APK is used in place. A bundle is unpacked into a
/// new directory under `work_root`.
pub fn open(path: &Path, work_root: &Path) -> Result<Input> {
    if !is_zip(path)? {
        return Err(anyhow::anyhow!(
            "{} is not an APK or APK bundle (not a zip archive)",
            path.display()
        )
        .context(ErrorKind::NotAnApk));
    }
    let f = File::open(path)?;
    let mut zip = zip::ZipArchive::new(f)
        .with_context(|| format!("read zip {}", path.display()))
        .context(ErrorKind::NotAnApk)?;
    let names: Vec<String> = zip
        .file_names()
        .filter_map(|r| r.ok())
        .map(|c| c.to_string())
        .collect();
    if names.iter().any(|n| n == "AndroidManifest.xml") {
        return Ok(Input {
            apks: vec![Member {
                path: path.to_path_buf(),
                origin: path.display().to_string(),
            }],
            skipped: vec![],
            work: None,
        });
    }
    let (entries, skipped) = select_entries(&names);
    if entries.is_empty() {
        return Err(anyhow::anyhow!(
            "{} contains no APKs (encrypted .apkm files aren't supported)",
            path.display()
        )
        .context(ErrorKind::UnsupportedBundle));
    }
    if entries.len() > 200 {
        return Err(
            anyhow::anyhow!("{} has too many APKs ({})", path.display(), entries.len())
                .context(ErrorKind::UnsupportedBundle),
        );
    }
    fs::create_dir_all(work_root)?;
    // The name is unique per process and per open, because the GUI can hold
    // several inputs at once.
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let work = work_root.join(format!(
        "install-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir(&work).with_context(|| format!("create {}", work.display()))?;
    let mut input = Input {
        apks: vec![],
        skipped,
        work: Some(work.clone()),
    };
    let mut total = 0u64;
    for (i, name) in entries.iter().enumerate() {
        let mut e = zip.by_name(name)?;
        if e.encrypted() {
            return Err(anyhow::anyhow!("{name} in {} is encrypted", path.display())
                .context(ErrorKind::UnsupportedBundle));
        }
        let dst = work.join(format!("{i}.apk"));
        let mut out = File::create(&dst)?;
        let n = io::copy(&mut (&mut e).take(MAX_APK + 1), &mut out)?;
        out.flush()?;
        total += n;
        if n > MAX_APK || total > MAX_TOTAL {
            bail!("{} is too large to unpack", path.display());
        }
        input.apks.push(Member {
            path: dst,
            origin: name.clone(),
        });
    }
    Ok(input)
}

/// The ABI of a config split name, spelled as in `ro.product.cpu.abilist`
/// (`config.arm64_v8a` -> `arm64-v8a`).
pub fn split_abi(split: &str) -> Option<String> {
    let cfg = split.rsplit_once("config.").map(|(_, c)| c)?;
    let abi = match cfg {
        "armeabi_v7a" => "armeabi-v7a",
        "arm64_v8a" => "arm64-v8a",
        "armeabi" | "x86" | "x86_64" | "mips" | "mips64" | "riscv64" => cfg,
        _ => return None,
    };
    Some(abi.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn apks_set() {
        let (e, _) = select_entries(&s(&[
            "toc.pb",
            "splits/base-master.apk",
            "splits/base-x86_64.apk",
            "splits/base-xxhdpi.apk",
            "standalones/standalone-x86_64.apk",
        ]));
        assert_eq!(
            e,
            s(&[
                "splits/base-master.apk",
                "splits/base-x86_64.apk",
                "splits/base-xxhdpi.apk"
            ])
        );
        let (e, _) = select_entries(&s(&["toc.pb", "universal.apk"]));
        assert_eq!(e, s(&["universal.apk"]));
    }

    #[test]
    fn xapk() {
        let (e, sk) = select_entries(&s(&[
            "manifest.json",
            "icon.png",
            "com.example.apk",
            "config.arm64_v8a.apk",
            "config.x86_64.apk",
            "Android/obb/com.example/main.1.com.example.obb",
            "dir/",
        ]));
        assert_eq!(
            e,
            s(&[
                "com.example.apk",
                "config.arm64_v8a.apk",
                "config.x86_64.apk"
            ])
        );
        assert_eq!(sk.len(), 1);
    }

    #[test]
    fn abis() {
        assert_eq!(split_abi("config.arm64_v8a").as_deref(), Some("arm64-v8a"));
        assert_eq!(split_abi("config.x86_64").as_deref(), Some("x86_64"));
        assert_eq!(split_abi("feature.config.x86").as_deref(), Some("x86"));
        assert_eq!(split_abi("config.xxhdpi"), None);
        assert_eq!(split_abi("config.en"), None);
        assert_eq!(split_abi("base"), None);
    }

    #[test]
    fn plain_apk_in_place() {
        let dir = crate::test_dir("bundle-plain-apk");
        let p = dir.join("x.apk");
        {
            let mut w = zip::ZipWriter::new(File::create(&p).unwrap());
            w.start_file(
                "AndroidManifest.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
            w.write_all(b"x").unwrap();
            w.finish().unwrap();
        }
        let i = open(&p, &dir.join("work")).unwrap();
        assert_eq!(i.apks.len(), 1);
        assert_eq!(i.apks[0].path, p);
        // A bundle is unpacked with generated names, then cleaned up.
        let b = dir.join("x.xapk");
        {
            let mut w = zip::ZipWriter::new(File::create(&b).unwrap());
            for n in ["../../evil.apk", "config.x86_64.apk"] {
                w.start_file(n, zip::write::SimpleFileOptions::default())
                    .unwrap();
                w.write_all(b"PK").unwrap();
            }
            w.finish().unwrap();
        }
        let work;
        {
            let i = open(&b, &dir.join("work")).unwrap();
            assert_eq!(i.apks.len(), 2);
            work = i.apks[0].path.parent().unwrap().to_path_buf();
            assert!(i.apks.iter().all(|m| m.path.starts_with(&work)));
            assert_eq!(i.apks[0].path.file_name().unwrap(), "0.apk");
        }
        assert!(!work.exists());
        assert!(!dir.join("evil.apk").exists());
        let notzip = dir.join("notzip.apk");
        fs::write(&notzip, b"hello").unwrap();
        assert!(open(&notzip, &dir.join("work")).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}
