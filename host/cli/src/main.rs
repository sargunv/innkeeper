//! `linuxapk`: install Android apps into the Linux.apk session with desktop
//! entries and icons, run, list and uninstall them. A thin front end over
//! `linuxapk-core` (shared with `linuxapk-manager`). See host/cli/README.md.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use linuxapk_core::launcher::Session;
use linuxapk_core::ops::{self, Event, LaunchOptions};
use linuxapk_core::profiles::{self, Settings};
use linuxapk_core::{desktop, validate, xdg};
use rustix::fs::{FileType, OFlags};

mod splash;
use splash::Splash;

#[derive(Parser)]
#[command(
    name = "linuxapk",
    version,
    about = "Android apps on the Linux desktop (Linux.apk)"
)]
struct Cli {
    /// The profile to work with (`default`, or a name from `profile list`).
    #[arg(long, global = true, value_name = "NAME")]
    profile: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open files with an Android app, or show the manager with no files.
    Open { files: Vec<PathBuf> },
    /// Install an .apk, .apks, .xapk or .apkm and add it to the app menu.
    Install { file: PathBuf },
    /// Launch an app, optionally opening files or a URI in it.
    Run {
        package: String,
        /// Open a separate task/window, if the launcher activity supports it.
        #[arg(long)]
        new_window: bool,
        files_or_uri: Vec<String>,
    },
    /// Uninstall an app and remove its desktop entry and icons.
    Uninstall {
        package: String,
        /// Keep the app's data (a later install picks it up again).
        #[arg(long)]
        keep_data: bool,
    },
    /// Read or change an app through the session D-Bus service. Prints current settings as JSON.
    App {
        package: String,
        #[arg(default_value = "info", value_parser = ["info", "clear-cache", "clear-data", "force-stop", "permission", "escape-back", "background"])]
        action: String,
        #[arg(long)]
        permission: Option<String>,
        #[arg(long, value_parser = ["on", "off"])]
        setting: Option<String>,
    },
    /// List installed (user) apps.
    List,
    /// Let an app run in the background with no window open (keeping the
    /// session up), or not; with no setting, list the apps allowed.
    Background {
        package: Option<String>,
        #[arg(value_parser = ["on", "off"])]
        setting: Option<String>,
    },
    /// Manage profiles: separate Android setups, each with its own apps,
    /// data and settings.
    #[command(subcommand)]
    Profile(ProfileCmd),
}

#[derive(Subcommand)]
enum ProfileCmd {
    /// List the profiles and whether they are running.
    List,
    /// Create a profile.
    Create { name: String },
    /// Use a profile when none is named (the manager's Show Apps does too).
    Use { name: String },
    /// Rename a stopped profile; its app-menu entries follow.
    Rename { from: String, to: String },
    /// Delete a stopped profile, with its apps and their data.
    Delete { name: String },
    /// Start a profile (default: the one in use) and wait for Android.
    Start { name: Option<String> },
    /// Stop a profile (default: the one in use).
    Stop { name: Option<String> },
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

/// The static desktop entry handles both APK installation and file VIEW.
fn open(files: &[PathBuf], token: Option<String>) -> Result<()> {
    let (packages, documents): (Vec<_>, Vec<_>) = files.iter().partition(|p| {
        p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
            ["apk", "apks", "xapk", "apkm"]
                .iter()
                .any(|x| e.eq_ignore_ascii_case(x))
        })
    });
    if files.is_empty() || !packages.is_empty() {
        let mut manager =
            std::process::Command::new(std::env::current_exe()?.with_file_name("linuxapk-manager"));
        manager.args(packages);
        if let Some(t) = &token {
            manager.env("XDG_ACTIVATION_TOKEN", t);
        }
        manager.spawn()?;
    }
    if !documents.is_empty() {
        let args = documents
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        run(
            "dev.linuxapk.system",
            &args,
            token,
            Some("dev.linuxapk.system/.documents.OpenWithActivity"),
            false,
        )?;
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
fn run(
    pkg: &str,
    args: &[String],
    token: Option<String>,
    file_component: Option<&str>,
    new_window: bool,
) -> Result<()> {
    anyhow::ensure!(
        !new_window || args.is_empty(),
        "--new-window opens the launcher activity; omit files and URIs"
    );
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
                    app_id: desktop::app_id(pkg),
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
            new_window,
        };
        if args.iter().all(|s| s.is_empty()) {
            return ops::run(pkg, None, &opts, &stderr);
        }
        let paths = linuxapk_core::paths::Prefix::locate()?.session_paths()?;
        let mut reply = String::new();
        let mut chooser_grants = Vec::new();
        for arg in args.iter().filter(|s| !s.is_empty()) {
            let path = if arg.starts_with("file:") {
                Some(
                    linuxapk_bridge::documents::portal::file_path(arg)
                        .ok_or_else(|| anyhow::anyhow!("invalid file URI"))?,
                )
            } else if !arg.contains(":") || Path::new(arg).exists() {
                Some(PathBuf::from(arg))
            } else {
                None
            };
            reply = match path {
                Some(path) => {
                    let file = open_input(&path)?;
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .ok_or_else(|| anyhow::anyhow!("file needs a UTF-8 name"))?;
                    let grant = linuxapk_bridge::apps::file::grant(
                        &paths.run.join("bridge/bridge.sock"),
                        &file,
                        name,
                    )?;
                    if file_component.is_some() {
                        chooser_grants.push(grant);
                        continue;
                    }
                    linuxapk_core::android::launch(
                        &session,
                        &linuxapk_core::android::Launch::File {
                            package: pkg.into(),
                            grants: vec![grant],
                            component: file_component.map(str::to_string),
                        },
                        &opts,
                    )?
                }
                None => ops::run(pkg, Some(arg), &opts, &stderr)?,
            };
        }
        if !chooser_grants.is_empty() {
            reply = linuxapk_core::android::launch(
                &session,
                &linuxapk_core::android::Launch::File {
                    package: pkg.into(),
                    grants: chooser_grants,
                    component: file_component.map(str::to_string),
                },
                &opts,
            )?;
        }
        Ok(reply)
    })();
    if let Some(s) = splash {
        s.close();
    }
    r.map(drop)
}

/// Reject special files without waiting for a FIFO writer before the fd is checked.
fn open_input(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OFlags::NONBLOCK.bits() as i32)
        .open(path)?;
    let stat = rustix::fs::fstat(&file)?;
    ensure!(
        FileType::from_raw_mode(stat.st_mode) == FileType::RegularFile,
        "not a regular file: {}",
        path.display()
    );
    let flags = rustix::fs::fcntl_getfl(&file)?;
    if flags.contains(OFlags::NONBLOCK) {
        rustix::fs::fcntl_setfl(&file, flags - OFlags::NONBLOCK)?;
    }
    Ok(file)
}

/// The splash colour of an app whose entry doesn't say (Android's light
/// window background).
const DEFAULT_SPLASH: u32 = 0xfafafa;

fn background(pkg: Option<&str>, setting: Option<&str>) -> Result<()> {
    let allowed = ops::background(pkg.zip(setting.map(|s| s == "on")), &stderr)?;
    if allowed.is_empty() {
        println!("no apps run in the background");
    }
    for p in allowed {
        println!("{p}");
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

fn profile(cmd: ProfileCmd) -> Result<()> {
    let name = |n: Option<String>| match n {
        Some(n) => profiles::check_name(&n).map(|()| n),
        None => profiles::resolve(false),
    };
    match cmd {
        ProfileCmd::List => {
            let current = profiles::resolve(false).ok();
            for p in profiles::list()? {
                let mark = if Some(&p.name) == current.as_ref() {
                    "*"
                } else {
                    " "
                };
                let state = if p.running { "running" } else { "stopped" };
                println!("{mark} {:<16} {state}", p.name);
            }
        }
        ProfileCmd::Create { name: n } => {
            profiles::create(&n, &Settings::default())?;
            println!("created profile {n}");
        }
        ProfileCmd::Use { name: n } => {
            ensure!(
                profiles::list()?.iter().any(|p| p.name == n),
                "no profile {n}"
            );
            profiles::choose(&n)?;
        }
        ProfileCmd::Rename { from, to } => {
            profiles::rename(&from, &to)?;
            println!("renamed profile {from} to {to}");
        }
        ProfileCmd::Delete { name: n } => {
            profiles::delete(&n)?;
            println!("deleted profile {n}");
        }
        ProfileCmd::Start { name: n } => {
            profiles::start(&name(n)?, &|m| eprintln!("linuxapk: {m}"))?;
        }
        ProfileCmd::Stop { name: n } => profiles::stop(&name(n)?)?,
    }
    Ok(())
}

/// The profile a command works with: `--profile`, `LINUXAPK_PROFILE`, the
/// one in use, or, for a command that needs one, a first one made now.
fn select_profile(named: Option<&str>, create: bool) -> Result<()> {
    let p = match named {
        Some(p) => {
            profiles::check_name(p)?;
            p.to_string()
        }
        None => match profiles::resolve(create) {
            Ok(p) => p,
            Err(_) if !create => return Ok(()),
            Err(e) => return Err(e),
        },
    };
    // SAFETY: no other threads yet.
    unsafe { std::env::set_var("LINUXAPK_PROFILE", p) };
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
    // Profile commands name theirs; the others need one.
    let create = !matches!(cli.cmd, Cmd::Profile(_));
    if let Err(e) = select_profile(cli.profile.as_deref(), create) {
        eprintln!("linuxapk: {e:#}");
        return ExitCode::FAILURE;
    }
    let res = match cli.cmd {
        Cmd::Open { files } => open(&files, token),
        Cmd::Install { file } => install(&file),
        Cmd::Uninstall { package, keep_data } => uninstall(&package, keep_data),
        Cmd::Run {
            package,
            files_or_uri,
            new_window,
        } => run(&package, &files_or_uri, token, None, new_window),
        Cmd::List => list(),
        Cmd::App {
            package,
            action,
            permission,
            setting,
        } => (|| -> Result<()> {
            ensure!(
                setting.is_some()
                    == matches!(action.as_str(), "permission" | "escape-back" | "background"),
                "use --setting on|off for a switch"
            );
            let mut command = linuxapk_core::app_settings::Command::new(&package, &action);
            command.permission = permission.unwrap_or_default();
            command.enabled = setting.as_deref() == Some("on");
            let info = linuxapk_core::app_settings::request(command, &stderr)?;
            println!("{}", serde_json::to_string_pretty(&info)?);
            Ok(())
        })(),
        Cmd::Background { package, setting } => {
            if package.is_some() != setting.is_some() {
                Err(anyhow::anyhow!("give a package and on or off, or neither"))
            } else {
                background(package.as_deref(), setting.as_deref())
            }
        }
        Cmd::Profile(cmd) => profile(cmd),
    };
    if let Some(n) = Session::find()
        .ok()
        .and_then(|s| ops::single_uid_notice(s.running()))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_without_a_writer_is_rejected_without_blocking() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../out/test/cli")
            .join(format!("fifo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("input.md");
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, rustix::fs::Mode::RUSR).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(open_input(&fifo)).unwrap());
        let result = rx
            .recv_timeout(Duration::from_secs(1))
            .expect("FIFO open blocked");
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("not a regular file")
        );
    }
}
