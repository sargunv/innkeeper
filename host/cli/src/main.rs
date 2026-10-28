//! `linuxapk`: install Android apps into the Linux.apk session with desktop
//! entries and icons, run, list and uninstall them. A thin front end over
//! `linuxapk-core` (shared with `linuxapk-manager`). See host/cli/README.md.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use linuxapk_core::launcher::Session;
use linuxapk_core::ops::{self, Event, LaunchOptions};
use linuxapk_core::{validate, xdg};

mod splash;
use splash::Splash;

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
    Install { file: PathBuf },
    /// Launch an app, optionally opening a URI in it.
    Run {
        package: String,
        uri: Option<String>,
    },
    /// Uninstall an app and remove its desktop entry and icons.
    Uninstall {
        package: String,
        /// Keep the app's data (a later install picks it up again).
        #[arg(long)]
        keep_data: bool,
    },
    /// List installed (user) apps.
    List,
}

/// Progress and warnings on stderr.
fn stderr(e: Event) {
    match e {
        Event::Status(m) | Event::Warning(m) => eprintln!("linuxapk: {m}"),
    }
}

fn install(file: &Path) -> Result<()> {
    let p = ops::inspect(file, &stderr)?;
    let i = ops::install(&p, &stderr)?;
    println!("installed {} as \"{}\"", i.package, i.name);
    println!("  app menu: {}", i.menu);
    if !i.schemes.is_empty() {
        println!("  URI schemes: {}", i.schemes.join(", "));
    }
    Ok(())
}

fn uninstall(pkg: &str, keep_data: bool) -> Result<()> {
    let u = ops::uninstall(pkg, keep_data, &stderr)?;
    if u.from_android {
        println!(
            "uninstalled {pkg}{}",
            if keep_data { " (data kept)" } else { "" }
        );
    } else {
        eprintln!("linuxapk: {pkg} isn't an installed user app");
    }
    if u.from_desktop {
        println!("removed the desktop entry and icons");
    }
    Ok(())
}

/// Launches an app. From a desktop launch on a stopped session, a splash
/// window shows at once and hands its focus on to the app's window.
fn run(pkg: &str, uri: Option<&str>, token: Option<String>) -> Result<()> {
    let pkg = validate::package(pkg)?;
    let session = Session::find()?;
    let splash = if session.running() {
        None
    } else {
        xdg::launch_look(&xdg::Dirs::from_env(), pkg).and_then(|(name, look, icon)| {
            let (color, width, height) = look.map_or((DEFAULT_SPLASH, 1280, 800), |l| {
                (l.color, l.width, l.height)
            });
            Splash::show(
                splash::Look {
                    app_id: format!("dev.linuxapk.{pkg}"),
                    title: name,
                    color,
                    width,
                    height,
                    icon: icon.and_then(|p| splash::Rgba::load_png(&p).ok()),
                },
                token.clone(),
            )
        })
    };
    let r = (|| {
        ops::ensure_session(&stderr)?;
        // The splash used the launch's token; the app's window gets one of
        // the splash's.
        let token = match &splash {
            Some(s) => s.token(Duration::from_secs(2)),
            None => token,
        };
        let opts = LaunchOptions {
            token,
            wait: splash.is_some(),
        };
        ops::run(pkg, uri, &opts, &stderr)
    })();
    if let Some(s) = splash {
        s.close();
    }
    r.map(drop)
}

/// The splash colour of an app whose entry doesn't say (Android's light
/// window background).
const DEFAULT_SPLASH: u32 = 0xfafafa;

fn list() -> Result<()> {
    for (p, l) in ops::list(&stderr)? {
        match l {
            Some(l) => println!("{p}\t{}", validate::truncate(&l, 128)),
            None => println!("{p}\t(no desktop entry)"),
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    // A desktop launch's token is ours to use once; nothing we start
    // inherits it.
    let token = std::env::var("XDG_ACTIVATION_TOKEN")
        .ok()
        .and_then(|t| validate::activation_token(&t).map(str::to_string));
    // SAFETY: no other threads yet.
    unsafe {
        std::env::remove_var("XDG_ACTIVATION_TOKEN");
        std::env::remove_var("DESKTOP_STARTUP_ID");
    }
    let cli = Cli::parse();
    let res = match cli.cmd {
        Cmd::Install { file } => install(&file),
        Cmd::Uninstall { package, keep_data } => uninstall(&package, keep_data),
        Cmd::Run { package, uri } => run(&package, uri.as_deref(), token),
        Cmd::List => list(),
    };
    if let Some(n) = Session::find()
        .ok()
        .and_then(|s| ops::single_uid_notice(s.uid_mode()))
    {
        eprintln!("linuxapk: note: {n} See {}", ops::SINGLE_UID_DOCS);
    }
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("linuxapk: {e:#}");
            ExitCode::FAILURE
        }
    }
}
