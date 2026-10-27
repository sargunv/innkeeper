//! `linuxapk`: install Android apps into the Linux.apk session with desktop
//! entries and icons, run them, and manage the session. A thin front end over
//! `linuxapk-core` (shared with the `linuxapk-gui` manager). See docs/cli.md.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use anyhow::Result;
use clap::{Parser, Subcommand};
use linuxapk_core::desktop::Handler;
use linuxapk_core::ops::{self, Event};
use linuxapk_core::session::Session;
use linuxapk_core::{proc, validate};

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
        /// the "open with" handler when the GUI isn't installed).
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
    /// Register Linux.apk as the handler for APK files (double-click to
    /// install), and add the manager app to the app menu if it's installed.
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

/// Progress and warnings on stderr.
fn stderr(e: Event) {
    match e {
        Event::Status(m) | Event::Warning(m) => eprintln!("linuxapk: {m}"),
    }
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
    let p = ops::inspect(file, &stderr)?;
    notify.send(
        "android-package-archive",
        "Installing Android app",
        &p.package,
    );
    let i = ops::install(&p, &stderr)?;
    println!("installed {} as \"{}\"", i.package, i.name);
    println!("  desktop entry: {}", i.desktop.display());
    if !i.schemes.is_empty() {
        println!("  URI schemes: {}", i.schemes.join(", "));
    }
    notify.send(&i.icon, &format!("Installed {}", i.name), &i.package);
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
    let (h, files) = ops::setup()?;
    match h {
        Handler::Gui(_) => println!("APK files now open in Linux.apk (linuxapk-gui):"),
        Handler::Cli(_) => println!("APK files now open with linuxapk install:"),
    }
    for f in files {
        println!("  {}", f.display());
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
        Cmd::Run { package, uri } => ops::run(&package, uri.as_deref(), &stderr),
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
