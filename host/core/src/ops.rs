//! User-level operations shared by the CLI and the GUI. They block (reading
//! APKs, the session, Android's package manager), so the GUI runs them off its UI
//! thread; progress and warnings go to a [`Report`] callback.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

pub use crate::android::LaunchOptions;
use crate::android::{self, Launch};
use crate::apk::{self, ApkInfo};
use crate::bundle::{self, Input};
use crate::launcher::Session;
use crate::xdg::{self, DesktopApp, Dirs};
use crate::{ErrorKind, validate};
use innkeeper_profile::profile::ProfileName;

/// Progress from a long operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// What it's doing now ("waiting for Android to boot").
    Status(String),
    /// Something the user should know that doesn't stop it.
    Warning(String),
}

pub type Report<'a> = &'a dyn Fn(Event);

/// A [`Report`] that ignores everything.
pub fn quiet(_: Event) {}

/// Profile `profile`'s running session, after starting it if needed.
fn session(profile: &ProfileName, report: Report) -> Result<Session> {
    let s = Session::of_profile(profile)?;
    s.ensure(&|m| report(Event::Status(m.to_string())))?;
    Ok(s)
}

/// Starts profile `profile` if it isn't running, and waits for it to boot.
pub fn ensure_session(profile: &ProfileName, report: Report) -> Result<()> {
    session(profile, report).map(drop)
}

/// An APK or bundle, read and checked but not installed yet: what an install
/// confirmation shows.
pub struct Package {
    pub file: PathBuf,
    pub package: String,
    /// The app's name (validated; the package name if it has none).
    pub label: String,
    /// `versionName` (validated; may be empty) and `versionCode`.
    pub version: String,
    pub version_code: String,
    /// Declared permissions (`android.permission.CAMERA`, ...).
    pub permissions: Vec<String>,
    /// ABIs it has native code for (empty: none, runs anywhere).
    pub abis: Vec<String>,
    /// Size of the input file in bytes.
    pub size: u64,
    /// Number of APKs (1 for a plain APK).
    pub apk_count: usize,
    /// A host-rendered adaptive icon or the largest legacy bitmap.
    pub icon: Option<Vec<u8>>,
    input: Input,
    /// Metadata of each member of `input`, in order.
    info: Vec<ApkInfo>,
    base: usize,
}

impl Package {
    fn base(&self) -> (&Path, &ApkInfo) {
        (
            self.input.apks[self.base].path.as_path(),
            &self.info[self.base],
        )
    }

    /// Whether this image can run it, judging by its native code alone (the
    /// session's ABI list is checked again at install).
    pub fn runs_here(&self) -> bool {
        // Use the ABI declaration applied to the pinned image. It excludes
        // NativeBridge ABIs until a translator is installed (image/vendor.prop).
        let abis = include_str!("../../../image/vendor.prop")
            .lines()
            .find_map(|l| l.strip_prefix("ro.product.cpu.abilist="))
            .unwrap_or_default();
        self.abis.is_empty()
            || abis
                .split(',')
                .any(|a| self.abis.iter().any(|b| b == a.trim()))
    }
}

/// Reads an APK or bundle (no session needed).
pub fn inspect(file: &Path, report: Report) -> Result<Package> {
    let size = file
        .metadata()
        .with_context(|| format!("open {}", file.display()))?
        .len();
    let input = bundle::open(file, &Dirs::from_env().cache)?;
    for s in &input.skipped {
        report(Event::Warning(format!(
            "skipping {} (OBB expansion files aren't supported yet)",
            validate::truncate(s, 120)
        )));
    }
    let mut info = vec![];
    for m in &input.apks {
        info.push(
            apk::read(&m.path)
                .context(ErrorKind::NotAnApk)
                .with_context(|| format!("read {}", validate::truncate(&m.origin, 120)))?,
        );
    }
    let bases: Vec<usize> = (0..info.len())
        .filter(|i| info[*i].split.is_empty())
        .collect();
    if bases.len() != 1 {
        return Err(anyhow::anyhow!(
            "expected one base APK in {}, found {}",
            file.display(),
            bases.len()
        )
        .context(ErrorKind::UnsupportedBundle));
    }
    let base = &info[bases[0]];
    let pkg = validate::package(&base.package)?.to_string();
    if let Some(b) = info.iter().find(|b| b.package != pkg) {
        return Err(anyhow::anyhow!(
            "bundle mixes packages ({pkg} and {})",
            validate::truncate(&b.package, 80)
        )
        .context(ErrorKind::UnsupportedBundle));
    }
    let label = validate::label(&base.label).unwrap_or_else(|| pkg.clone());
    let mut abis: BTreeSet<String> = base.native_code.iter().cloned().collect();
    abis.extend(info.iter().filter_map(|b| bundle::split_abi(&b.split)));
    let mut permissions: BTreeSet<String> = BTreeSet::new();
    for b in &info {
        permissions.extend(b.permissions.iter().cloned());
    }
    let icon = apk::icon::render(&input.apks[bases[0]].path, &base.icons);
    Ok(Package {
        file: file.to_path_buf(),
        label,
        version: version_name(&base.version_name),
        version_code: base.version_code.clone(),
        permissions: permissions.into_iter().collect(),
        abis: abis.into_iter().collect(),
        size,
        apk_count: input.apks.len(),
        icon,
        package: pkg,
        input,
        info,
        base: bases[0],
    })
}

/// A `versionName` safe to show and to write into a desktop entry.
fn version_name(v: &str) -> String {
    validate::label(v)
        .map(|v| v.chars().take(64).collect())
        .unwrap_or_default()
}

/// Installs an inspected package into the session (starting it if needed).
/// The session's bridge adds it to the app menu (if mirroring is on), as for
/// any app installed inside Android.
pub fn install(profile: &ProfileName, p: &Package, report: Report) -> Result<()> {
    let pkg = p.package.clone();
    let s = session(profile, report)?;
    let abis: Vec<String> = s
        .getprop("ro.product.cpu.abilist")
        .split(',')
        .map(|a| a.trim().to_string())
        .collect();

    // Base first, then splits; ABI splits only for ABIs the session runs.
    let (base_path, base) = p.base();
    let mut apks: Vec<&Path> = vec![base_path];
    let mut abi_splits = 0;
    for (m, b) in p.input.apks.iter().zip(&p.info) {
        if b.split.is_empty() {
            continue;
        }
        if let Some(abi) = bundle::split_abi(&b.split) {
            abi_splits += 1;
            if !abis.contains(&abi) {
                continue;
            }
        }
        apks.push(m.path.as_path());
    }
    if abi_splits > 0
        && !p
            .info
            .iter()
            .any(|b| bundle::split_abi(&b.split).is_some_and(|a| abis.contains(&a)))
    {
        return Err(anyhow::anyhow!(
            "{pkg} has no native code for this session's ABIs ({})",
            abis.join(", ")
        )
        .context(ErrorKind::WrongAbi));
    }
    if !base.native_code.is_empty()
        && abi_splits == 0
        && !base.native_code.iter().any(|c| abis.contains(c))
    {
        return Err(anyhow::anyhow!(
            "{pkg} only has native code for {} (this session runs {})",
            base.native_code.join(", "),
            abis.join(", ")
        )
        .context(ErrorKind::WrongAbi));
    }
    report(Event::Status(format!(
        "installing {pkg} {} ({} APK{})",
        validate::truncate(&base.version_name, 40),
        apks.len(),
        if apks.len() == 1 { "" } else { "s" }
    )));
    android::install(&s, &apks)
}

/// Uninstalls a user app (system apps are refused); the session's bridge
/// removes its app-menu entry. Returns whether Android had it installed.
pub fn uninstall(
    profile: &ProfileName,
    pkg: &str,
    keep_data: bool,
    report: Report,
) -> Result<bool> {
    let pkg = validate::package(pkg)?;
    let s = session(profile, report)?;
    let from_android = android::user_packages(&s)?.iter().any(|p| p == pkg);
    if from_android {
        android::uninstall(&s, pkg, keep_data)?;
    }
    Ok(from_android)
}

/// Launches an app (in a window of its own), optionally opening a URI in it.
/// An empty URI is none: the DynamicLauncher portal quotes `%U` in the
/// entries it writes (`'%U'`), which launchers expand to an empty argument.
/// Returns the window manager's reply.
pub fn run(
    profile: &ProfileName,
    pkg: &str,
    uri: Option<&str>,
    opts: &LaunchOptions,
    report: Report,
) -> Result<String> {
    let pkg = validate::package(pkg)?;
    let uri = uri
        .filter(|u| !u.is_empty())
        .map(validate::launch_uri)
        .transpose()?;
    let launch = || -> Result<String> {
        let s = session(profile, report)?;
        let l = match uri {
            Some(u) => Launch::View {
                package: pkg.into(),
                uri: u.into(),
            },
            None => Launch::Main {
                component: android::launcher_component(&s, pkg)?,
            },
        };
        crate::app_settings::launch(profile, &l, opts)
    };
    match launch() {
        // The session stopped for idleness just as this came: once more.
        Err(_) if !Session::of_profile(profile)?.running() => launch(),
        r => r,
    }
}

/// A user app in the session.
pub struct ListedApp {
    pub package: String,
    /// The `Name` of its desktop entry.
    pub label: Option<String>,
    /// Whether it may run in the background; None before the system app's
    /// first activity report.
    pub background: Option<bool>,
}

/// The session's user apps.
pub fn list(profile: &ProfileName, report: Report) -> Result<Vec<ListedApp>> {
    let s = session(profile, report)?;
    let d = Dirs::from_env();
    let background = s
        .paths()
        .and_then(|p| innkeeper_profile::activity::Activity::read(&p))
        .map(|a| a.background);
    Ok(android::user_packages(&s)?
        .into_iter()
        .map(|package| ListedApp {
            label: xdg::app_label(&d, &package),
            background: background.as_ref().map(|b| b.contains(&package)),
            package,
        })
        .collect())
}

/// Profile `profile`'s apps, from their desktop entries (no session
/// needed). If `android` is given (the session's user packages), apps
/// installed without an entry are added too.
pub fn installed_apps(
    profile: &ProfileName,
    android: Option<&[String]>,
) -> Result<Vec<DesktopApp>> {
    let mut v: Vec<DesktopApp> = xdg::desktop_apps(&Dirs::from_env())
        .into_iter()
        .filter(|a| a.profile.as_deref() == Some(profile.as_str()))
        .collect();
    if let Some(pkgs) = android {
        for p in pkgs {
            if !v.iter().any(|a| &a.package == p) {
                v.push(DesktopApp {
                    package: p.clone(),
                    name: p.clone(),
                    version: None,
                    profile: Some(profile.to_string()),
                    icon: None,
                });
            }
        }
    }
    Ok(v)
}

/// Profile `profile`'s user packages, if it is running and booted (doesn't
/// start it).
pub fn android_packages(profile: &ProfileName) -> Option<Vec<String>> {
    let s = Session::of_profile(profile).ok()?;
    if !s.running() || !s.booted() {
        return None;
    }
    android::user_packages(&s).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn compatibility_uses_image_abis() {
        let dir = crate::test_dir("package-abis");
        let file = dir.join("fixture.apk");
        let mut zip = zip::ZipWriter::new(fs::File::create(&file).unwrap());
        zip.start_file(
            "AndroidManifest.xml",
            zip::write::SimpleFileOptions::default(),
        )
        .unwrap();
        zip.finish().unwrap();

        // Metadata fixtures: no third-party APK or native libraries needed.
        for (native_code, runnable) in [
            (vec!["x86"], false),
            (vec!["x86_64"], true),
            (vec![], true),
            (vec!["arm64-v8a"], false),
            (vec!["x86", "x86_64"], true),
        ] {
            let info = ApkInfo {
                native_code: native_code.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            };
            let p = Package {
                file: file.clone(),
                package: "com.example.fixture".into(),
                label: "Fixture".into(),
                version: String::new(),
                version_code: String::new(),
                permissions: vec![],
                abis: info.native_code.clone(),
                size: 0,
                apk_count: 1,
                icon: None,
                input: bundle::open(&file, &dir).unwrap(),
                info: vec![info],
                base: 0,
            };
            assert_eq!(p.runs_here(), runnable, "native code: {native_code:?}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn versions() {
        assert_eq!(version_name(" 2.7.50-r-2024 "), "2.7.50-r-2024");
        assert_eq!(version_name("1.0\u{202E}"), "");
        assert_eq!(version_name(&"9".repeat(100)).chars().count(), 64);
    }
}
