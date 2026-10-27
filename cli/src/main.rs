//! `linuxapk`: install Android apps into the Linux.apk session with desktop
//! entries and icons, run them, and manage the session. A thin front end over
//! `linuxapk-core` (shared with the `linuxapk-gui` manager). See docs/cli.md.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use clap::{Parser, Subcommand};
use linuxapk_core::ops::{self, Event};
use linuxapk_core::session::Session;
use linuxapk_core::validate;

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
    /// Register the Linux.apk manager as the handler for APK files
    /// (double-click to install), and add it to the app menu.
    Setup {
        /// Unregister and remove the handler.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Subcommand)]
enum SessionCmd {
    Start,
    Stop,
    Status,
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
    println!("  desktop entry: {}", i.desktop.display());
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

fn list() -> Result<()> {
    for (p, l) in ops::list(&stderr)? {
        match l {
            Some(l) => println!("{p}\t{}", validate::truncate(&l, 128)),
            None => println!("{p}\t(no desktop entry)"),
        }
    }
    Ok(())
}

fn setup(remove: bool) -> Result<()> {
    if remove {
        ops::unsetup()?;
        println!("removed the APK handler");
        return Ok(());
    }
    let files = ops::setup()?;
    println!("APK files now open in Linux.apk (linuxapk-gui):");
    for f in files {
        println!("  {}", f.display());
    }
    Ok(())
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let res = match cli.cmd {
        Cmd::Install { file } => install(&file),
        Cmd::Uninstall { package, keep_data } => uninstall(&package, keep_data),
        Cmd::Run { package, uri } => ops::run(&package, uri.as_deref(), &stderr),
        Cmd::List => list(),
        Cmd::Session { action } => {
            let r = Session::find().and_then(|s| match action {
                SessionCmd::Start => s.passthrough(&["start"]),
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
        Cmd::Setup { remove } => setup(remove),
    };
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("linuxapk: {e:#}");
            ExitCode::FAILURE
        }
    }
}
