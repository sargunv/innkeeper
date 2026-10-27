//! `linuxapk`: install Android apps into the Linux.apk session with desktop
//! entries and icons, run them, and manage the session. See docs/cli.md.

mod android;
mod bundle;
mod desktop;
mod manifest;
mod proc;
mod session;
mod validate;
mod xdg;

use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::android::Launch;
use crate::desktop::AppEntry;
use crate::manifest::Aapt2;
use crate::session::Session;
use crate::xdg::Dirs;

#[derive(Parser)]
#[command(
    name = "linuxapk",
    version,
    about = "Android apps on the Linux desktop (Linux.apk)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Install an .apk, .apks, .xapk or .apkm and add it to the app menu.
    Install {
        file: PathBuf,
        /// Report progress and the result as desktop notifications (used by
        /// the "open with" handler).
        #[arg(long)]
        notify: bool,
    },
    /// Uninstall an app and remove its desktop entry and icons.
    Uninstall {
        package: String,
        /// Keep the app's data (a later install picks it up again).
        #[arg(long)]
        keep_data: bool,
    },
    /// Launch an app, optionally opening a URI in it.
    Run {
        package: String,
        uri: Option<String>,
    },
    /// List installed (user) apps.
    List,
    /// Start, stop or query the Android session.
    Session {
        #[command(subcommand)]
        action: SessionCmd,
    },
    /// Register Linux.apk as the handler for APK files (double-click to install).
    Setup {
        /// Unregister and remove the handler.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    Start {
        /// No host window.
        #[arg(long)]
        headless: bool,
    },
    Stop,
    Status,
}

/// The command line that re-invokes this binary from a desktop entry: `linuxapk`
/// if that's what `PATH` finds, else the absolute path; plus the session
/// selection (`LINUXAPK_INSTANCE`, `LINUXAPK_SESSION`) if set, so entries made
/// from a lane's session keep using it.
fn self_command() -> Result<Vec<String>> {
    let mut v = vec![];
    let inst = env::var("LINUXAPK_INSTANCE").unwrap_or_default();
    if !inst
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        bail!("bad LINUXAPK_INSTANCE");
    }
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
    let on_path = proc::which("linuxapk").and_then(|p| p.canonicalize().ok());
    if on_path.as_deref() == Some(exe.as_path()) {
        v.push("linuxapk".into());
    } else {
        v.push(exe.to_str().context("binary path isn't UTF-8")?.to_string());
    }
    Ok(v)
}

/// Desktop notifications for `--notify` (argv to notify-send; the body is
/// markup for some servers, so it's escaped).
struct Notifier(bool);

impl Notifier {
    fn send(&self, icon: &str, summary: &str, body: &str) {
        if !self.0 || proc::which("notify-send").is_none() {
            return;
        }
        let esc = |s: &str| {
            s.replace('&', "&amp;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        let _ = proc::run(
            Command::new("notify-send").args([
                "--app-name=Linux.apk",
                "-i",
                icon,
                "--",
                &esc(summary),
                &esc(body),
            ]),
            Stdio::null(),
            Duration::from_secs(10),
        );
    }
}

fn install(file: &Path, notify: &Notifier) -> Result<()> {
    let dirs = Dirs::from_env();
    let aapt = Aapt2::find()?;
    let input = bundle::open(file, &dirs.cache)?;
    for s in &input.skipped {
        eprintln!(
            "linuxapk: skipping {} (OBB expansion files aren't supported yet)",
            validate::truncate(s, 120)
        );
    }
    let mut members = vec![];
    for m in &input.apks {
        let b = aapt
            .badging(&m.path)
            .with_context(|| format!("read {}", validate::truncate(&m.origin, 120)))?;
        members.push((m, b));
    }
    let bases: Vec<_> = members.iter().filter(|(_, b)| b.split.is_empty()).collect();
    if bases.len() != 1 {
        bail!(
            "expected one base APK in {}, found {}",
            file.display(),
            bases.len()
        );
    }
    let (base_m, base) = bases[0];
    let pkg = validate::package(&base.package)?.to_string();
    if let Some((_, b)) = members.iter().find(|(_, b)| b.package != pkg) {
        bail!(
            "bundle mixes packages ({pkg} and {})",
            validate::truncate(&b.package, 80)
        );
    }

    notify.send("android-package-archive", "Installing Android app", &pkg);
    let s = Session::find()?;
    s.ensure()?;
    let abis: Vec<String> = s
        .getprop("ro.product.cpu.abilist")
        .split(',')
        .map(|a| a.trim().to_string())
        .collect();

    // Base first, then splits; ABI splits only for ABIs the session runs.
    let mut apks: Vec<&Path> = vec![base_m.path.as_path()];
    let mut abi_splits = 0;
    for (m, b) in &members {
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
        && !members
            .iter()
            .any(|(_, b)| bundle::split_abi(&b.split).is_some_and(|a| abis.contains(&a)))
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
    eprintln!(
        "linuxapk: installing {pkg} {} ({} APK{})",
        validate::truncate(&base.version_name, 40),
        apks.len(),
        if apks.len() == 1 { "" } else { "s" }
    );
    android::install(&s, &apks)?;

    let man = aapt.manifest(&base_m.path)?;
    if man.package != pkg {
        bail!(
            "manifest package {:?} doesn't match {pkg}",
            validate::truncate(&man.package, 80)
        );
    }
    let info = android::app_info(&s, &pkg, xdg::ICON_SIZES, &dirs.cache).unwrap_or_else(|e| {
        eprintln!("linuxapk: couldn't render the icon in Android ({e:#}); using the APK's PNG if it has one");
        android::AppInfo::default()
    });
    let mut icons = info.icons.clone();
    if icons.is_empty() {
        icons = apk_png_icons(&base_m.path, &base.icons);
    }

    let name = validate::label(&base.label)
        .or_else(|| validate::label(&info.label))
        .unwrap_or_else(|| {
            eprintln!("linuxapk: the app's label is missing or unsafe; using the package name");
            pkg.clone()
        });
    let mut seen = BTreeSet::new();
    let names = base
        .labels
        .iter()
        .filter_map(|(tag, l)| Some((validate::desktop_locale(tag)?, validate::label(l)?)))
        .filter(|(loc, l)| *l != name && seen.insert(loc.clone()))
        .collect();
    let mut run = self_command()?;
    run.push("run".into());
    let mut entry = AppEntry {
        package: pkg.clone(),
        name: name.clone(),
        names,
        run,
        icon: None,
        categories: desktop::categories_for(man.app_category.or(info.category), man.is_game),
        schemes: man.schemes.clone(),
        web_hosts: man.web_hosts.clone(),
    };
    let path = xdg::install_app(&dirs, &mut entry, &icons)?;
    println!("installed {pkg} as \"{name}\"");
    println!("  desktop entry: {}", path.display());
    if !entry.schemes.is_empty() {
        println!("  URI schemes: {}", entry.schemes.join(", "));
    }
    notify.send(
        entry.icon.as_deref().unwrap_or("android-package-archive"),
        &format!("Installed {name}"),
        &pkg,
    );
    Ok(())
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
        let Ok(e) = zip.by_name(path) else { continue };
        let mut buf = vec![];
        if std::io::Read::read_to_end(&mut std::io::Read::take(e, 4 << 20), &mut buf).is_err() {
            continue;
        }
        if let Some((w, h)) = xdg::png_size(&buf)
            && w == h
            && !out.iter().any(|(s, _)| *s == w)
        {
            out.push((w, buf));
        }
    }
    out
}

fn uninstall(pkg: &str, keep_data: bool) -> Result<()> {
    let pkg = validate::package(pkg)?;
    let s = Session::find()?;
    s.ensure()?;
    if android::user_packages(&s)?.iter().any(|p| p == pkg) {
        android::uninstall(&s, pkg, keep_data)?;
        println!(
            "uninstalled {pkg}{}",
            if keep_data { " (data kept)" } else { "" }
        );
    } else {
        eprintln!("linuxapk: {pkg} isn't an installed user app");
    }
    if xdg::remove_app(&Dirs::from_env(), pkg) {
        println!("removed the desktop entry and icons");
    }
    Ok(())
}

fn run(pkg: &str, uri: Option<&str>) -> Result<()> {
    let pkg = validate::package(pkg)?;
    let uri = uri.map(validate::launch_uri).transpose()?;
    let s = Session::find()?;
    s.ensure()?;
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

fn list() -> Result<()> {
    let s = Session::find()?;
    s.ensure()?;
    let d = Dirs::from_env();
    for p in android::user_packages(&s)? {
        match xdg::app_label(&d, &p) {
            Some(l) => println!("{p}\t{}", validate::truncate(&l, 128)),
            None => println!("{p}\t(no desktop entry)"),
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let res = match cli.cmd {
        Cmd::Install { file, notify } => {
            let n = Notifier(notify);
            let r = install(&file, &n);
            if let Err(e) = &r {
                n.send(
                    "dialog-error",
                    "Couldn't install Android app",
                    &format!("{e:#}"),
                );
            }
            r
        }
        Cmd::Uninstall { package, keep_data } => uninstall(&package, keep_data),
        Cmd::Run { package, uri } => run(&package, uri.as_deref()),
        Cmd::List => list(),
        Cmd::Session { action } => {
            let r = Session::find().and_then(|s| match action {
                SessionCmd::Start { headless } => s.passthrough(if headless {
                    &["start", "--headless"]
                } else {
                    &["start"]
                }),
                SessionCmd::Stop => s.passthrough(&["stop"]),
                SessionCmd::Status => s.passthrough(&["status"]),
            });
            return match r {
                Ok(code) => ExitCode::from(code.clamp(0, 255) as u8),
                Err(e) => {
                    eprintln!("linuxapk: {e:#}");
                    ExitCode::FAILURE
                }
            };
        }
        Cmd::Setup { remove } => {
            let d = Dirs::from_env();
            if remove {
                xdg::unsetup(&d).map(|()| println!("removed the APK handler"))
            } else {
                self_command()
                    .and_then(|c| xdg::setup(&d, &c))
                    .map(|files| {
                        println!("APK files now open with linuxapk install:");
                        for f in files {
                            println!("  {}", f.display());
                        }
                    })
            }
        }
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("linuxapk: {e:#}");
            ExitCode::FAILURE
        }
    }
}
