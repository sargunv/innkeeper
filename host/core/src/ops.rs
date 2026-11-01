//! User-level operations shared by the CLI and the GUI. They block on reading
//! packages, on the session and on the guest's package manager, so the GUI
//! runs them off its UI thread. Progress and warnings go to a [`Report`]
//! callback. Android profiles take APKs and bundles, and OpenHarmony profiles
//! take HAPs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub use crate::android::LaunchOptions;
use crate::android::{self, Launch};
use crate::apk::{self, ApkInfo};
use crate::bundle::{self, Input};
use crate::launcher::Session;
use crate::xdg::{self, DesktopApp, Dirs};
use crate::{ErrorKind, hap, ohos, validate};
use innkeeper_profile::profile::{ProfileName, System};

/// Progress from a long operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// What the operation is doing, such as "waiting for Android to boot".
    Status(String),
    /// Something the user should know that doesn't stop the operation.
    Warning(String),
}

pub type Report<'a> = &'a dyn Fn(Event);

/// A [`Report`] that ignores everything.
pub fn quiet(_: Event) {}

/// Returns profile `profile`'s running session and its system, and starts the
/// session if needed.
fn session(profile: &ProfileName, report: Report) -> Result<(Session, System)> {
    let s = Session::of_profile(profile)?;
    s.ensure(&|m| report(Event::Status(m.to_string())))?;
    let system = s.system();
    Ok((s, system))
}

/// Starts profile `profile` if it isn't running, and waits for it to boot.
pub fn ensure_session(profile: &ProfileName, report: Report) -> Result<()> {
    session(profile, report).map(drop)
}

/// An APK, APK bundle or HAP that has been read and checked but not
/// installed. An install confirmation shows it.
pub struct Package {
    pub file: PathBuf,
    pub package: String,
    /// The app's validated name, or the package name if it has none.
    pub label: String,
    /// The validated `versionName`, which may be empty.
    pub version: String,
    pub version_code: String,
    /// Declared permissions (`android.permission.CAMERA`, ...).
    pub permissions: Vec<String>,
    /// The ABIs the app has native code for. It is empty if the app has no
    /// native code and runs anywhere.
    pub abis: Vec<String>,
    /// Size of the input file in bytes.
    pub size: u64,
    /// A host-rendered adaptive icon or the largest legacy bitmap. HAPs have
    /// none.
    pub icon: Option<Vec<u8>>,
    contents: Contents,
}

enum Contents {
    Android {
        input: Input,
        /// Metadata of each member of `input`, in order.
        info: Vec<ApkInfo>,
        base: usize,
    },
    /// A HAP, which [`Package::file`] holds.
    Ohos,
}

impl Package {
    /// The system that runs the package.
    pub fn system(&self) -> System {
        match self.contents {
            Contents::Android { .. } => System::Android,
            Contents::Ohos => System::Ohos,
        }
    }

    /// Whether the pinned image can run the app, judging by its native code
    /// alone. [`install`] checks the session's ABI list again.
    pub fn runs_here(&self) -> bool {
        if let Contents::Ohos = self.contents {
            // The pinned OpenHarmony image is x86_64 only.
            return self.abis.is_empty() || self.abis.iter().any(|a| a == "x86_64");
        }
        // Use the ABI list from image/vendor.prop, which the pinned image
        // applies. It excludes NativeBridge ABIs until a translator is
        // installed.
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

/// Reads an APK, APK bundle or HAP. This needs no session.
pub fn inspect(file: &Path, report: Report) -> Result<Package> {
    let size = file
        .metadata()
        .with_context(|| format!("open {}", file.display()))?
        .len();
    if file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("hap"))
    {
        return inspect_hap(file, size);
    }
    let input = bundle::open(file, &Dirs::from_env().cache)?;
    for s in &input.skipped {
        report(Event::Warning(format!(
            "skipping {}: OBB files aren't supported",
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
        icon,
        package: pkg,
        contents: Contents::Android {
            input,
            info,
            base: bases[0],
        },
    })
}

/// Reads a HAP. An app with several HAPs installs each one on its own.
fn inspect_hap(file: &Path, size: u64) -> Result<Package> {
    let h = hap::read(file).context(ErrorKind::NotAHap)?;
    let pkg = validate::package(&h.bundle)?.to_string();
    Ok(Package {
        file: file.to_path_buf(),
        label: validate::label(&h.label).unwrap_or_else(|| pkg.clone()),
        version: version_name(&h.version_name),
        version_code: h.version_code,
        permissions: h.permissions,
        abis: h.native_code,
        size,
        icon: None,
        package: pkg,
        contents: Contents::Ohos,
    })
}

/// A `versionName` safe to show and to write into a desktop entry.
fn version_name(v: &str) -> String {
    validate::label(v)
        .map(|v| v.chars().take(64).collect())
        .unwrap_or_default()
}

/// Installs an inspected package into the session, starting the session if
/// needed. On Android the session's bridge adds the app to the app menu, as
/// it does for any app installed inside Android. A test session adds no menu
/// entries.
pub fn install(profile: &ProfileName, p: &Package, report: Report) -> Result<()> {
    let pkg = p.package.clone();
    let system = crate::profiles::settings(profile)?.system;
    if system != p.system() {
        return Err(anyhow::anyhow!(
            "{pkg} is an {} app, and profile {profile} runs {}",
            p.system().title(),
            system.title()
        )
        .context(ErrorKind::WrongSystem));
    }
    let (s, _) = session(profile, report)?;
    let Contents::Android { input, info, base } = &p.contents else {
        report(Event::Status(format!("installing {pkg} {}", p.version)));
        return ohos::install(&s, &p.file);
    };
    let abis: Vec<String> = s
        .getprop("ro.product.cpu.abilist")
        .split(',')
        .map(|a| a.trim().to_string())
        .collect();

    // Install the base first, then the splits. ABI splits are included only
    // for ABIs the session runs.
    let (base_path, base) = (input.apks[*base].path.as_path(), &info[*base]);
    let mut apks: Vec<&Path> = vec![base_path];
    let mut abi_splits = 0;
    for (m, b) in input.apks.iter().zip(info) {
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
        && !info
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
            "{pkg} has native code for {}, but this session runs {}",
            base.native_code.join(", "),
            abis.join(", ")
        )
        .context(ErrorKind::WrongAbi));
    }
    report(Event::Status(format!(
        "installing {pkg} {}",
        validate::truncate(&base.version_name, 40)
    )));
    android::install(&s, &apks)
}

/// Uninstalls a user app and leaves system apps alone. On Android the
/// session's bridge removes the app-menu entry. Returns whether the guest had
/// the app installed as a user app.
pub fn uninstall(
    profile: &ProfileName,
    pkg: &str,
    keep_data: bool,
    report: Report,
) -> Result<bool> {
    let pkg = validate::package(pkg)?;
    let (s, system) = session(profile, report)?;
    if system == System::Ohos {
        let user = ohos::bundle(&s, pkg)?.is_some_and(|b| !b.preinstalled);
        if user {
            ohos::uninstall(&s, pkg, keep_data)?;
        }
        return Ok(user);
    }
    let from_android = android::user_packages(&s)?.iter().any(|p| p == pkg);
    if from_android {
        android::uninstall(&s, pkg, keep_data)?;
    }
    Ok(from_android)
}

/// Launches an app in its own window, optionally opening a URI in it. An
/// empty URI counts as no URI, because the DynamicLauncher portal writes `%U`
/// quoted as `'%U'`, which launchers expand to an empty argument. Returns the
/// window manager's reply.
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
        let (s, system) = session(profile, report)?;
        if system == System::Ohos {
            if uri.is_some() {
                bail!("OpenHarmony apps can't open URIs yet");
            }
            return ohos::start(&s, pkg);
        }
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
        // The session stopped for idleness just as this launch arrived, so try
        // once more.
        Err(_) if !Session::of_profile(profile)?.running() => launch(),
        r => r,
    }
}

/// What `app` does to an OpenHarmony bundle. Android's equivalents go through
/// the system app ([`crate::app_settings`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BundleAction {
    Stop,
    ClearCache,
    ClearData,
}

/// Applies `action` to bundle `pkg` in OpenHarmony profile `profile`.
pub fn bundle_action(
    profile: &ProfileName,
    pkg: &str,
    action: BundleAction,
    report: Report,
) -> Result<()> {
    let pkg = validate::package(pkg)?;
    let (s, system) = session(profile, report)?;
    if system != System::Ohos {
        bail!("profile {profile} runs {}", system.title());
    }
    match action {
        BundleAction::Stop => ohos::force_stop(&s, pkg),
        BundleAction::ClearCache => ohos::clean(&s, pkg, true),
        BundleAction::ClearData => ohos::clean(&s, pkg, false),
    }
}

/// Reads the version, sizes and permissions of bundle `pkg` in OpenHarmony
/// profile `profile`. Android's app info comes from the system app
/// ([`crate::app_settings`]).
pub fn bundle_info(profile: &ProfileName, pkg: &str, report: Report) -> Result<ohos::BundleInfo> {
    let pkg = validate::package(pkg)?;
    let (s, system) = session(profile, report)?;
    if system != System::Ohos {
        bail!("profile {profile} runs {}", system.title());
    }
    ohos::info(&s, pkg)
}

/// A user app in the session.
pub struct ListedApp {
    pub package: String,
    /// The `Name` of its desktop entry.
    pub label: Option<String>,
    /// Whether the app may run in the background. It is None until the system
    /// app's first activity report, and on OpenHarmony.
    pub background: Option<bool>,
}

/// Lists the session's user apps. On OpenHarmony the label is the one that
/// the bundle manager reports.
pub fn list(profile: &ProfileName, report: Report) -> Result<Vec<ListedApp>> {
    let (s, system) = session(profile, report)?;
    if system == System::Ohos {
        return Ok(ohos::user_bundles(&s)?
            .into_iter()
            .map(|(package, label)| ListedApp {
                label: validate::label(&label),
                background: None,
                package,
            })
            .collect());
    }
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

/// Lists profile `profile`'s apps from their desktop entries, which needs no
/// session. If `android` holds the session's user packages, apps installed
/// without an entry are added too.
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

/// Returns profile `profile`'s user packages if it is running and booted. It
/// does not start the profile.
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

        // These are metadata fixtures, so no third-party APK or native library
        // is needed.
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
                icon: None,
                contents: Contents::Android {
                    input: bundle::open(&file, &dir).unwrap(),
                    info: vec![info],
                    base: 0,
                },
            };
            assert_eq!(p.runs_here(), runnable, "native code: {native_code:?}");
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn haps_are_for_openharmony() {
        let dir = crate::test_dir("package-hap");
        let file = dir.join("app.HAP");
        let mut zip = zip::ZipWriter::new(fs::File::create(&file).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("module.json", opts).unwrap();
        std::io::Write::write_all(
            &mut zip,
            br#"{"app": {"bundleName": "dev.example.app", "versionCode": 3,
                 "versionName": "1.2", "label": "Example"},
                 "module": {"name": "entry"}}"#,
        )
        .unwrap();
        zip.start_file("libs/arm64-v8a/libapp.so", opts).unwrap();
        zip.finish().unwrap();
        let mut p = inspect(&file, &quiet).unwrap();
        assert_eq!(p.system(), System::Ohos);
        assert_eq!(
            (p.package.as_str(), p.label.as_str(), p.version.as_str()),
            ("dev.example.app", "Example", "1.2")
        );
        assert_eq!(p.version_code, "3");
        assert!(!p.runs_here());
        p.abis.push("x86_64".into());
        assert!(p.runs_here());
        p.abis.clear();
        assert!(p.runs_here());
        fs::write(&file, b"not a zip").unwrap();
        let e = inspect(&file, &quiet).err().unwrap();
        assert_eq!(e.downcast_ref::<ErrorKind>(), Some(&ErrorKind::NotAHap));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn versions() {
        assert_eq!(version_name(" 2.7.50-r-2024 "), "2.7.50-r-2024");
        assert_eq!(version_name("1.0\u{202E}"), "");
        assert_eq!(version_name(&"9".repeat(100)).chars().count(), 64);
    }
}
