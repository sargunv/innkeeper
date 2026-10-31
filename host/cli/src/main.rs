//! `innkeeper`: install Android apps into the Innkeeper session with desktop
//! entries and icons, run, list and uninstall them. A thin front end over
//! `innkeeper-core` (shared with `innkeeper-manager`). See host/cli/README.md.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, ensure};
use clap::{Parser, Subcommand};
use innkeeper_core::android::{self, Launch};
use innkeeper_core::launcher::Session;
use innkeeper_core::ops::{self, Event, LaunchOptions};
use innkeeper_core::profiles::{self, Settings};
use innkeeper_core::{desktop, validate, xdg};
use rustix::fs::{FileType, OFlags};

mod splash;
use splash::Splash;

#[derive(Parser)]
#[command(name = "innkeeper", version, about = "Run Android apps on Linux")]
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
    /// Show or change an installed app's Android settings (starting the
    /// profile if needed).
    #[command(subcommand)]
    App(AppCmd),
    /// List installed (user) apps, with whether each may run in the
    /// background.
    List,
    /// Connect your adb to the profile (starting it if needed); adb must be
    /// on for it (`profile set --adb on`).
    Adb,
    /// Manage profiles: separate Android setups, each with its own apps,
    /// data and settings.
    #[command(subcommand)]
    Profile(ProfileCmd),
}

#[derive(Subcommand)]
enum AppCmd {
    /// Storage, settings and runtime permissions.
    Info {
        package: String,
        /// As JSON.
        #[arg(long)]
        json: bool,
    },
    /// Delete the app's cache.
    ClearCache { package: String },
    /// Delete all of the app's data: files, accounts and settings.
    ClearData { package: String },
    /// Force-stop the app.
    Stop { package: String },
    /// Change settings.
    Set {
        package: String,
        /// Let it run with no window open, which keeps the profile running.
        #[arg(long, value_parser = ["on", "off"])]
        background: Option<String>,
        /// Escape acts as Android's Back in its windows.
        #[arg(long, value_parser = ["on", "off"])]
        escape_back: Option<String>,
        /// Grant or revoke a runtime permission (repeatable).
        #[arg(long, value_name = "NAME=on|off", value_parser = permission_setting)]
        permission: Vec<(String, bool)>,
    },
}

/// `NAME=on|off`.
fn permission_setting(s: &str) -> Result<(String, bool), String> {
    match s.rsplit_once('=') {
        Some((name, "on")) if !name.is_empty() => Ok((name.into(), true)),
        Some((name, "off")) if !name.is_empty() => Ok((name.into(), false)),
        _ => Err("expected NAME=on or NAME=off".into()),
    }
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
    /// Change a profile's settings (default: the one in use).
    Set {
        name: Option<String>,
        /// adb on loopback TCP, for Android Studio, Gradle and adb; applies
        /// at once.
        #[arg(long, value_parser = ["on", "off"])]
        adb: Option<String>,
        /// Apps in windows of their own, or the whole device in one window;
        /// applies at the next start.
        #[arg(long, value_parser = ["windowed", "phone"])]
        mode: Option<String>,
        /// The phone's screen.
        #[arg(long, value_parser = ["small-phone", "medium-phone", "medium-tablet"])]
        size: Option<String>,
    },
}

/// Progress and warnings on stderr.
fn stderr(e: Event) {
    match e {
        Event::Status(m) | Event::Warning(m) => eprintln!("innkeeper: {m}"),
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
        let mut manager = std::process::Command::new(
            std::env::current_exe()?.with_file_name("innkeeper-manager"),
        );
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
            "dev.sargunv.innkeeper.system",
            &args,
            token,
            Some("dev.sargunv.innkeeper.system/.documents.OpenWithActivity"),
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
        eprintln!("innkeeper: {pkg} isn't an installed user app");
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
    ensure!(
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
        let mut reply = String::new();
        let mut chooser_grants = Vec::new();
        for arg in args.iter().filter(|s| !s.is_empty()) {
            let path = if arg.starts_with("file:") {
                Some(
                    innkeeper_core::uri::file_path(arg)
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
                    let grant = innkeeper_core::app_settings::import(&file, name)?;
                    if file_component.is_some() {
                        chooser_grants.push(grant);
                        continue;
                    }
                    android::launch(
                        &session,
                        &Launch::File {
                            package: pkg.into(),
                            grants: vec![grant],
                            component: None,
                        },
                        &opts,
                    )?
                }
                None => ops::run(pkg, Some(arg), &opts, &stderr)?,
            };
        }
        if !chooser_grants.is_empty() {
            reply = android::launch(
                &session,
                &Launch::File {
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

/// Opens a regular file to grant, refusing special files without waiting for
/// a FIFO writer.
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

fn app(cmd: AppCmd) -> Result<()> {
    use innkeeper_core::app_settings::{Command, request};
    let switch = |package: &str, action: &str, on: bool| {
        let mut c = Command::new(package, action);
        c.enabled = on;
        c
    };
    let commands = match cmd {
        AppCmd::Info { package, json } => {
            let info = request(Command::new(&package, "info"), &stderr)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                print!("{}", app_info_text(&info));
            }
            return Ok(());
        }
        AppCmd::ClearCache { package } => vec![Command::new(&package, "clear-cache")],
        AppCmd::ClearData { package } => vec![Command::new(&package, "clear-data")],
        AppCmd::Stop { package } => vec![Command::new(&package, "force-stop")],
        AppCmd::Set {
            package,
            background,
            escape_back,
            permission,
        } => {
            let mut v = vec![];
            if let Some(b) = background {
                v.push(switch(&package, "background", b == "on"));
            }
            if let Some(e) = escape_back {
                v.push(switch(&package, "escape-back", e == "on"));
            }
            for (name, on) in permission {
                let mut c = switch(&package, "permission", on);
                c.permission = name;
                v.push(c);
            }
            ensure!(
                !v.is_empty(),
                "nothing to set: give --background, --escape-back or --permission"
            );
            v
        }
    };
    for c in commands {
        request(c, &stderr)?;
    }
    Ok(())
}

/// `app info`'s text form.
fn app_info_text(i: &innkeeper_core::app_settings::AppInfo) -> String {
    let mb = |b: u64| format!("{:.1} MB", b as f64 / 1e6);
    let on = |b: bool| if b { "on" } else { "off" };
    let mut s = format!(
        "app size     {}\ndata         {} (cache {})\nbackground   {}\nescape-back  {}\n",
        mb(i.app_bytes),
        mb(i.data_bytes),
        mb(i.cache_bytes),
        on(i.background),
        on(i.escape_back),
    );
    if !i.permissions.is_empty() {
        s.push_str("permissions\n");
    }
    for p in &i.permissions {
        s.push_str(&format!(
            "  {}  {}{}\n",
            p.name,
            if p.granted { "granted" } else { "denied" },
            if p.editable { "" } else { " (fixed)" }
        ));
    }
    s
}

fn list() -> Result<()> {
    for a in ops::list(&stderr)? {
        let label = a.label.map_or("(no desktop entry)".into(), |l| {
            validate::truncate(&l, 128).to_string()
        });
        let background = match a.background {
            Some(true) => "background",
            Some(false) => "-",
            None => "?",
        };
        println!("{}\t{background}\t{label}", a.package);
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
                let mode = match p.settings.mode {
                    profiles::Mode::Windowed => "windowed".to_string(),
                    profiles::Mode::Phone => format!("phone {}", p.settings.size.name()),
                };
                let adb = if p.settings.debug_bridge {
                    format!("adb {}", innkeeper_core::adb::serial(&p.settings))
                } else {
                    String::new()
                };
                println!("{mark} {:<16} {state:<8} {mode:<20} {adb}", p.name);
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
            profiles::start(&name(n)?, &|m| eprintln!("innkeeper: {m}"))?;
        }
        ProfileCmd::Stop { name: n } => profiles::stop(&name(n)?)?,
        ProfileCmd::Set {
            name: n,
            adb,
            mode,
            size,
        } => {
            let n = name(n)?;
            ensure!(
                adb.is_some() || mode.is_some() || size.is_some(),
                "nothing to change (see --help)"
            );
            if mode.is_some() || size.is_some() {
                let mode = mode.map(|m| match m.as_str() {
                    "phone" => profiles::Mode::Phone,
                    _ => profiles::Mode::Windowed,
                });
                let size = size.as_deref().and_then(profiles::Size::parse);
                let s = profiles::set_mode(&n, mode, size)?;
                println!(
                    "mode: {} ({}), at the next start",
                    if s.mode == profiles::Mode::Phone {
                        "phone"
                    } else {
                        "windowed"
                    },
                    s.size.name()
                );
            }
            if let Some(on) = adb {
                let s = innkeeper_core::adb::set(&n, on == "on", &stderr)?;
                if s.debug_bridge {
                    println!(
                        "adb on: {} ({})",
                        innkeeper_core::adb::serial(&s),
                        innkeeper_core::adb::address(&s)
                    );
                } else {
                    println!("adb off");
                }
            }
        }
    }
    Ok(())
}

/// The profile a command works with: `--profile`, `INNKEEPER_PROFILE`, the
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
    unsafe { std::env::set_var("INNKEEPER_PROFILE", p) };
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
        eprintln!("innkeeper: {e:#}");
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
        Cmd::App(cmd) => app(cmd),
        Cmd::Profile(cmd) => profile(cmd),
        Cmd::Adb => profiles::resolve(false)
            .and_then(|n| innkeeper_core::adb::connect(&n, &stderr))
            .map(|serial| println!("connected: adb -s {serial} shell")),
    };
    if let Some(n) = Session::find()
        .ok()
        .and_then(|s| ops::single_uid_notice(s.running()))
    {
        eprintln!("innkeeper: note: {n} See {}", ops::SINGLE_UID_DOCS);
    }
    match res {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("innkeeper: {e:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &str) -> Result<Cmd, clap::Error> {
        Cli::try_parse_from(std::iter::once("innkeeper").chain(args.split_whitespace()))
            .map(|c| c.cmd)
    }

    #[test]
    fn app_commands() {
        let Ok(Cmd::App(AppCmd::Set {
            package,
            background,
            escape_back,
            permission,
        })) = parse(
            "app set a.b --background on --permission android.permission.CAMERA=off \
             --permission x.Y=on",
        )
        else {
            panic!("app set did not parse");
        };
        assert_eq!(package, "a.b");
        assert_eq!(background.as_deref(), Some("on"));
        assert_eq!(escape_back, None);
        assert_eq!(
            permission,
            [
                ("android.permission.CAMERA".to_string(), false),
                ("x.Y".to_string(), true)
            ]
        );
        assert!(matches!(
            parse("app info a.b --json"),
            Ok(Cmd::App(AppCmd::Info { json: true, .. }))
        ));
        assert!(matches!(
            parse("app stop a.b"),
            Ok(Cmd::App(AppCmd::Stop { .. }))
        ));
        for bad in [
            "app set a.b --permission CAMERA",
            "app set a.b --permission =on",
            "app set a.b --background yes",
            "app a.b",
            "background a.b on",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn app_info_text_lists_permissions() {
        let info = innkeeper_core::app_settings::AppInfo {
            app_bytes: 2_500_000,
            data_bytes: 1_000_000,
            cache_bytes: 0,
            escape_back: false,
            background: true,
            permissions: vec![innkeeper_core::app_settings::Permission {
                name: "android.permission.CAMERA".into(),
                granted: false,
                editable: false,
            }],
        };
        let t = app_info_text(&info);
        assert!(t.contains("app size     2.5 MB\n"), "{t}");
        assert!(t.contains("background   on\n"), "{t}");
        assert!(
            t.contains("  android.permission.CAMERA  denied (fixed)\n"),
            "{t}"
        );
    }

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
