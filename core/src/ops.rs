//! User-level operations shared by the CLI and the GUI. They block (aapt2,
//! the session, Android's package manager), so the GUI runs them off its UI
//! thread; progress and warnings go to a [`Report`] callback.

use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::android::{self, Launch};
use crate::bundle::{self, Input};
use crate::desktop::{self, AppEntry, Handler};
use crate::manifest::{self, Aapt2, Badging};
use crate::session::{self, Session};
use crate::xdg::{self, DesktopApp, Dirs};
use crate::{proc, validate};

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

/// The command line that runs `name` (`linuxapk` or `linuxapk-gui`) from a
/// desktop entry: this binary if it is `name`, else `name` next to it, else
/// `name` on `PATH`. It is the bare name if that's what `PATH` finds, else
/// the absolute path; prefixed with the session selection
/// (`LINUXAPK_INSTANCE`, `LINUXAPK_SESSION`) if set, so entries made from a
/// lane's session keep using it.
pub fn command_for(name: &str) -> Result<Vec<String>> {
    let mut v = vec![];
    let inst = session::instance()?;
    let sess = env::var("LINUXAPK_SESSION").unwrap_or_default();
    if !inst.is_empty() || !sess.is_empty() {
        v.push("env".to_string());
        if !inst.is_empty() {
            v.push(format!("LINUXAPK_INSTANCE={inst}"));
        }
        if !sess.is_empty() {
            v.push(format!("LINUXAPK_SESSION={sess}"));
        }
    }
    let exe = env::current_exe()?.canonicalize()?;
    let bin = if exe.file_name().is_some_and(|n| n == name) {
        exe
    } else if let Some(sib) = exe.parent().map(|d| d.join(name))
        && sib.is_file()
    {
        sib.canonicalize()?
    } else {
        proc::which(name)
            .and_then(|p| p.canonicalize().ok())
            .with_context(|| format!("`{name}` not found next to this program or on PATH"))?
    };
    let on_path = proc::which(name).and_then(|p| p.canonicalize().ok());
    if on_path.as_deref() == Some(bin.as_path()) {
        v.push(name.into());
    } else {
        v.push(bin.to_str().context("binary path isn't UTF-8")?.to_string());
    }
    Ok(v)
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
    /// The APK's largest launcher icon bitmap (PNG or WebP), if it has one.
    /// Adaptive (XML) icons are only rendered by Android after install.
    pub icon: Option<Vec<u8>>,
    input: Input,
    /// Badging of each member of `input`, in order.
    badging: Vec<Badging>,
    base: usize,
}

impl Package {
    fn base(&self) -> (&Path, &Badging) {
        (
            self.input.apks[self.base].path.as_path(),
            &self.badging[self.base],
        )
    }

    /// Whether this host can run it, judging by its native code alone (the
    /// session's ABI list is checked again at install).
    pub fn runs_here(&self) -> bool {
        let host: &[&str] = if cfg!(target_arch = "x86_64") {
            &["x86_64", "x86"]
        } else if cfg!(target_arch = "aarch64") {
            &["arm64-v8a", "armeabi-v7a"]
        } else {
            &[]
        };
        self.abis.is_empty() || self.abis.iter().any(|a| host.contains(&a.as_str()))
    }
}

/// Reads an APK or bundle (no session needed).
pub fn inspect(file: &Path, report: Report) -> Result<Package> {
    let aapt = Aapt2::find()?;
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
    let mut badging = vec![];
    for m in &input.apks {
        badging.push(
            aapt.badging(&m.path)
                .with_context(|| format!("read {}", validate::truncate(&m.origin, 120)))?,
        );
    }
    let bases: Vec<usize> = (0..badging.len())
        .filter(|i| badging[*i].split.is_empty())
        .collect();
    if bases.len() != 1 {
        bail!(
            "expected one base APK in {}, found {}",
            file.display(),
            bases.len()
        );
    }
    let base = &badging[bases[0]];
    let pkg = validate::package(&base.package)?.to_string();
    if let Some(b) = badging.iter().find(|b| b.package != pkg) {
        bail!(
            "bundle mixes packages ({pkg} and {})",
            validate::truncate(&b.package, 80)
        );
    }
    let label = validate::label(&base.label).unwrap_or_else(|| pkg.clone());
    let mut abis: BTreeSet<String> = base.native_code.iter().cloned().collect();
    abis.extend(badging.iter().filter_map(|b| bundle::split_abi(&b.split)));
    let mut permissions: BTreeSet<String> = BTreeSet::new();
    for b in &badging {
        permissions.extend(b.permissions.iter().cloned());
    }
    let base_apk = &input.apks[bases[0]].path;
    let mut icon = largest_bitmap_icon(base_apk, &base.icons);
    // An adaptive (XML) icon: try the legacy bitmaps of the same resource.
    if icon.is_none()
        && let Some((_, xml)) = base.icons.iter().max_by_key(|(d, _)| *d)
        && let Ok(res) = aapt.resources(base_apk)
    {
        icon = largest_bitmap_icon(base_apk, &manifest::bitmap_variants(&res, xml));
    }
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
        badging,
        base: bases[0],
    })
}

/// A `versionName` safe to show and to write into a desktop entry.
fn version_name(v: &str) -> String {
    validate::label(v)
        .map(|v| v.chars().take(64).collect())
        .unwrap_or_default()
}

/// What [`install`] did.
#[derive(Debug, Clone)]
pub struct Installed {
    pub package: String,
    pub name: String,
    pub desktop: PathBuf,
    /// Icon name in the desktop entry.
    pub icon: String,
    pub schemes: Vec<String>,
}

/// Installs an inspected package into the session (starting it if needed)
/// and adds it to the app menu.
pub fn install(p: &Package, report: Report) -> Result<Installed> {
    let dirs = Dirs::from_env();
    let aapt = Aapt2::find()?;
    let pkg = p.package.clone();
    let s = Session::find()?;
    s.ensure(&|m| report(Event::Status(m.to_string())))?;
    let abis: Vec<String> = s
        .getprop("ro.product.cpu.abilist")
        .split(',')
        .map(|a| a.trim().to_string())
        .collect();

    // Base first, then splits; ABI splits only for ABIs the session runs.
    let (base_path, base) = p.base();
    let mut apks: Vec<&Path> = vec![base_path];
    let mut abi_splits = 0;
    for (m, b) in p.input.apks.iter().zip(&p.badging) {
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
            .badging
            .iter()
            .any(|b| bundle::split_abi(&b.split).is_some_and(|a| abis.contains(&a)))
    {
        bail!(
            "{pkg} has no native code for this session's ABIs ({})",
            abis.join(", ")
        );
    }
    if !base.native_code.is_empty()
        && abi_splits == 0
        && !base.native_code.iter().any(|c| abis.contains(c))
    {
        bail!(
            "{pkg} only has native code for {} (this session runs {})",
            base.native_code.join(", "),
            abis.join(", ")
        );
    }
    report(Event::Status(format!(
        "installing {pkg} {} ({} APK{})",
        validate::truncate(&base.version_name, 40),
        apks.len(),
        if apks.len() == 1 { "" } else { "s" }
    )));
    android::install(&s, &apks)?;

    report(Event::Status("adding it to the app menu".into()));
    let man = aapt.manifest(base_path)?;
    if man.package != pkg {
        bail!(
            "manifest package {:?} doesn't match {pkg}",
            validate::truncate(&man.package, 80)
        );
    }
    let info = android::app_info(&s, &pkg, xdg::ICON_SIZES, &dirs.cache).unwrap_or_else(|e| {
        report(Event::Warning(format!(
            "couldn't render the icon in Android ({e:#}); using the APK's PNG if it has one"
        )));
        android::AppInfo::default()
    });
    let mut icons = info.icons.clone();
    if icons.is_empty() {
        icons = apk_png_icons(base_path, &base.icons);
    }

    let name = validate::label(&base.label)
        .or_else(|| validate::label(&info.label))
        .unwrap_or_else(|| {
            report(Event::Warning(
                "the app's label is missing or unsafe; using the package name".into(),
            ));
            pkg.clone()
        });
    let mut seen = BTreeSet::new();
    let names = base
        .labels
        .iter()
        .filter_map(|(tag, l)| Some((validate::desktop_locale(tag)?, validate::label(l)?)))
        .filter(|(loc, l)| *l != name && seen.insert(loc.clone()))
        .collect();
    let mut run = command_for("linuxapk")?;
    run.push("run".into());
    let mut entry = AppEntry {
        package: pkg.clone(),
        name: name.clone(),
        names,
        run,
        icon: None,
        version: p.version.clone(),
        categories: desktop::categories_for(man.app_category.or(info.category), man.is_game),
        schemes: man.schemes.clone(),
        web_hosts: man.web_hosts.clone(),
    };
    let path = xdg::install_app(&dirs, &mut entry, &icons)?;
    Ok(Installed {
        package: pkg,
        name,
        desktop: path,
        icon: entry.icon.unwrap_or_default(),
        schemes: entry.schemes,
    })
}

/// Reads up to 4 MiB of one zip entry.
fn zip_entry(zip: &mut zip::ZipArchive<std::fs::File>, path: &str) -> Option<Vec<u8>> {
    let e = zip.by_name(path).ok()?;
    let mut buf = vec![];
    std::io::Read::read_to_end(&mut std::io::Read::take(e, 4 << 20), &mut buf).ok()?;
    Some(buf)
}

/// Fallback icons: the APK's own PNG launcher icons, by pixel size.
fn apk_png_icons(apk: &Path, icons: &[(u32, String)]) -> Vec<(u32, Vec<u8>)> {
    let Ok(f) = std::fs::File::open(apk) else {
        return vec![];
    };
    let Ok(mut zip) = zip::ZipArchive::new(f) else {
        return vec![];
    };
    let mut out: Vec<(u32, Vec<u8>)> = vec![];
    for (_, path) in icons {
        if !path.ends_with(".png") {
            continue;
        }
        let Some(buf) = zip_entry(&mut zip, path) else {
            continue;
        };
        if let Some((w, h)) = xdg::png_size(&buf)
            && w == h
            && !out.iter().any(|(s, _)| *s == w)
        {
            out.push((w, buf));
        }
    }
    out
}

/// The highest-density bitmap (PNG or WebP) launcher icon in the APK.
fn largest_bitmap_icon(apk: &Path, icons: &[(u32, String)]) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(apk).ok()?).ok()?;
    let mut v: Vec<&(u32, String)> = icons
        .iter()
        .filter(|(_, p)| p.ends_with(".png") || p.ends_with(".webp"))
        .collect();
    v.sort_by_key(|(d, _)| std::cmp::Reverse(*d));
    v.into_iter().find_map(|(_, p)| zip_entry(&mut zip, p))
}

/// What [`uninstall`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Uninstalled {
    /// Android had it installed (as a user app) and removed it.
    pub from_android: bool,
    /// Its desktop entry or icons existed and were removed.
    pub from_desktop: bool,
}

/// Uninstalls a user app (system apps are refused) and removes its desktop
/// entry and icons.
pub fn uninstall(pkg: &str, keep_data: bool, report: Report) -> Result<Uninstalled> {
    let pkg = validate::package(pkg)?;
    let s = Session::find()?;
    s.ensure(&|m| report(Event::Status(m.to_string())))?;
    let from_android = android::user_packages(&s)?.iter().any(|p| p == pkg);
    if from_android {
        android::uninstall(&s, pkg, keep_data)?;
    }
    let from_desktop = xdg::remove_app(&Dirs::from_env(), pkg);
    Ok(Uninstalled {
        from_android,
        from_desktop,
    })
}

/// Launches an app (in a window of its own), optionally opening a URI in it.
pub fn run(pkg: &str, uri: Option<&str>, report: Report) -> Result<()> {
    let pkg = validate::package(pkg)?;
    let uri = uri.map(validate::launch_uri).transpose()?;
    let s = Session::find()?;
    s.ensure(&|m| report(Event::Status(m.to_string())))?;
    let l = match uri {
        Some(u) => Launch::View {
            package: pkg.into(),
            uri: u.into(),
        },
        None => Launch::Main {
            component: android::launcher_component(&s, pkg)?,
        },
    };
    android::launch(&s, &l)
}

/// User apps in the session, with the `Name` of their desktop entry.
pub fn list(report: Report) -> Result<Vec<(String, Option<String>)>> {
    let s = Session::find()?;
    s.ensure(&|m| report(Event::Status(m.to_string())))?;
    let d = Dirs::from_env();
    Ok(android::user_packages(&s)?
        .into_iter()
        .map(|p| {
            let l = xdg::app_label(&d, &p);
            (p, l)
        })
        .collect())
}

/// Apps installed for this session instance, from their desktop entries (no
/// session needed). If `android` is given (the session's user packages),
/// apps installed without an entry are added too.
pub fn installed_apps(android: Option<&[String]>) -> Vec<DesktopApp> {
    let inst = session::instance().unwrap_or_default();
    let mut v: Vec<DesktopApp> = xdg::desktop_apps(&Dirs::from_env())
        .into_iter()
        .filter(|a| a.instance == inst)
        .collect();
    if let Some(pkgs) = android {
        for p in pkgs {
            if !v.iter().any(|a| &a.package == p) {
                v.push(DesktopApp {
                    package: p.clone(),
                    name: p.clone(),
                    version: None,
                    instance: inst.clone(),
                    icon: None,
                });
            }
        }
    }
    v
}

/// The session's user packages, if it is running and booted (doesn't start
/// it).
pub fn android_packages() -> Option<Vec<String>> {
    let s = Session::find().ok()?;
    if !s.running() || !s.booted() {
        return None;
    }
    android::user_packages(&s).ok()
}

/// The handler `setup` registers: the GUI if it is installed next to this
/// program or on `PATH`, else the CLI.
pub fn default_handler() -> Result<Handler> {
    match command_for("linuxapk-gui") {
        Ok(g) => Ok(Handler::Gui(g)),
        Err(_) => Ok(Handler::Cli(command_for("linuxapk")?)),
    }
}

/// `linuxapk setup`: the APK handler (see [`default_handler`]), plus the
/// manager's app-menu entry if the GUI is installed. Returns the files
/// written.
pub fn setup() -> Result<(Handler, Vec<PathBuf>)> {
    let d = Dirs::from_env();
    let h = default_handler()?;
    let mut files = xdg::setup(&d, &h)?;
    if let Handler::Gui(g) = &h {
        files.extend(xdg::install_manager(&d, g)?);
    }
    Ok((h, files))
}

/// `linuxapk setup --remove`: the handler and the manager's entry.
pub fn unsetup() -> Result<()> {
    let d = Dirs::from_env();
    xdg::unsetup(&d)?;
    xdg::remove_manager(&d);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(version_name(" 2.7.50-r-2024 "), "2.7.50-r-2024");
        assert_eq!(version_name("1.0\u{202E}"), "");
        assert_eq!(version_name(&"9".repeat(100)).chars().count(), 64);
    }

    #[test]
    fn inspects_corpus_apk() {
        let corpus = env::var_os("LINUXAPK_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var("HOME").unwrap_or_default())
                    .join("Projects/linux.apk-wt/compat/out/corpus")
            });
        let apk = corpus.join("com.darkempire78.opencalculator_53.apk");
        if !apk.is_file() || Aapt2::find().is_err() {
            eprintln!("skipping: no corpus APK or aapt2");
            return;
        }
        let p = inspect(&apk, &quiet).unwrap();
        assert_eq!(p.package, "com.darkempire78.opencalculator");
        assert!(!p.version.is_empty());
        assert!(p.size > 0);
        assert_eq!(p.apk_count, 1);
        assert!(p.runs_here());
        // An adaptive icon with legacy PNGs: the largest PNG is found.
        assert!(p.icon.as_ref().is_some_and(|i| i.starts_with(b"\x89PNG")));
        assert!(
            p.permissions
                .iter()
                .any(|x| x == "android.permission.VIBRATE")
        );
    }
}
