//! `innkeeper` is the command-line interface. It manages profiles, each of
//! which runs Android or OpenHarmony. It installs, runs, lists and uninstalls
//! apps in either. In an Android profile, apps also get desktop entries and
//! icons, and can open files and URIs. It is a
//! front end over `innkeeper-core`, which `innkeeper-manager` also uses. See
//! host/cli/README.md.

use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use anyhow::{Result, bail, ensure};
use clap::{Parser, Subcommand};
use innkeeper_core::android::Launch;
use innkeeper_core::launcher::Session;
use innkeeper_core::ops::{self, Event, LaunchOptions};
use innkeeper_core::profiles;
use innkeeper_core::{desktop, validate, xdg};
use innkeeper_profile::profile::{ProfileName, Settings, System};
use rustix::fs::{FileType, OFlags};

mod splash;
use splash::Splash;

#[derive(Parser)]
#[command(
    name = "innkeeper",
    version,
    about = "Run Android and OpenHarmony apps on Linux"
)]
struct Cli {
    /// The name of the profile to use, as `profile list` shows it.
    #[arg(long, global = true, value_name = "NAME", default_value = innkeeper_profile::profile::FIRST)]
    profile: String,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Open files with an Android app.
    Open {
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// Install an .apk, .apks, .xapk, .apkm or .hap file.
    Install { file: PathBuf },
    /// Open an app, or files or a URI in it.
    Run {
        package: String,
        /// Open a new window, if the app supports it.
        #[arg(long)]
        new_window: bool,
        files_or_uri: Vec<String>,
    },
    /// Uninstall an app.
    Uninstall {
        package: String,
        /// Keep the app's data.
        #[arg(long)]
        keep_data: bool,
    },
    /// Show or change an app's settings.
    #[command(subcommand)]
    App(AppCmd),
    /// List installed apps and whether each runs in the background.
    List,
    /// Manage profiles.
    #[command(subcommand)]
    Profile(ProfileCmd),
}

#[derive(Subcommand)]
enum AppCmd {
    /// Show storage, settings and permissions.
    Info {
        package: String,
        /// Print JSON.
        #[arg(long)]
        json: bool,
    },
    /// Clear the app's cache.
    ClearCache { package: String },
    /// Clear the app's files, accounts and settings.
    ClearData { package: String },
    /// Force-stop the app.
    Stop { package: String },
    /// Change the app's settings.
    Set {
        package: String,
        /// Let the app run with no window open.
        #[arg(long, value_parser = ["on", "off"])]
        background: Option<String>,
        /// Make Escape act as Back.
        #[arg(long, value_parser = ["on", "off"])]
        escape_back: Option<String>,
        /// Grant or revoke a runtime permission (repeatable).
        #[arg(long, value_name = "NAME=on|off", value_parser = permission_setting)]
        permission: Vec<(String, bool)>,
    },
}

/// Parses `NAME=on` or `NAME=off`.
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
    Create {
        name: String,
        /// The profile's system. It can't be changed later.
        #[arg(long, default_value = "android", value_parser = ["android", "ohos"])]
        system: String,
    },
    /// Rename a stopped profile.
    Rename { from: String, to: String },
    /// Delete a stopped profile and its apps and data.
    Delete { name: String },
    /// Start the profile and wait for it to boot.
    Start {
        /// Start every profile.
        #[arg(long)]
        all: bool,
    },
    /// Stop the profile.
    Stop,
    /// Delete a stopped profile's apps and data, keeping its settings.
    Reset,
    /// Print the profile's kernel log.
    Log {
        /// Keep printing as it grows.
        #[arg(short, long)]
        follow: bool,
    },
    /// Change the profile's settings.
    Set(SetArgs),
}

#[derive(clap::Args)]
struct SetArgs {
    /// Show apps in their own windows or in one device window. Applies at the
    /// next start.
    #[arg(long, value_parser = ["windowed", "small-phone", "medium-phone", "medium-tablet"])]
    display: Option<String>,
    /// Run adb on localhost.
    #[arg(long, value_parser = ["on", "off"])]
    adb: Option<String>,
    /// Minutes with no apps open before the profile stops (1 to 120).
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=120))]
    idle_minutes: Option<u32>,
    /// Keep every profile running, and start them at login.
    #[arg(long, value_parser = ["on", "off"])]
    keep_running: Option<String>,
    /// Follow the desktop's dark mode.
    #[arg(long, value_parser = ["on", "off"])]
    follow_dark: Option<String>,
    /// Follow the desktop's accent colour.
    #[arg(long, value_parser = ["on", "off"])]
    follow_accent: Option<String>,
    /// Follow the desktop's languages and regional formats.
    #[arg(long, value_parser = ["on", "off"])]
    follow_locale: Option<String>,
    /// Follow the desktop's 12- or 24-hour clock.
    #[arg(long, value_parser = ["on", "off"])]
    follow_clock: Option<String>,
}

impl SetArgs {
    /// Returns the first given setting that OpenHarmony profiles don't have.
    /// OpenHarmony has no adb or desktop following. Its session doesn't stop
    /// when idle, because Android's system app is what reports activity.
    fn android_only(&self) -> Option<&'static str> {
        [
            (self.adb.is_some(), "--adb"),
            (self.idle_minutes.is_some(), "--idle-minutes"),
            (self.follow_dark.is_some(), "--follow-dark"),
            (self.follow_accent.is_some(), "--follow-accent"),
            (self.follow_locale.is_some(), "--follow-locale"),
            (self.follow_clock.is_some(), "--follow-clock"),
        ]
        .into_iter()
        .find_map(|(given, what)| given.then_some(what))
    }

    /// Applies the given profile settings to `s`, and returns whether any were
    /// given.
    fn apply(&self, s: &mut innkeeper_profile::profile::Settings) -> bool {
        let on = |v: &Option<String>| v.as_deref().map(|v| v == "on");
        let mut changed = false;
        let mut set = |field: &mut bool, v: Option<bool>| {
            if let Some(v) = v {
                *field = v;
                changed = true;
            }
        };
        set(&mut s.follow_dark, on(&self.follow_dark));
        set(&mut s.follow_accent, on(&self.follow_accent));
        set(&mut s.follow_locale, on(&self.follow_locale));
        set(&mut s.follow_clock, on(&self.follow_clock));
        if let Some(d) = self
            .display
            .as_deref()
            .and_then(innkeeper_profile::profile::Display::parse)
        {
            s.display = d;
            changed = true;
        }
        if let Some(m) = self.idle_minutes {
            s.idle_minutes = m;
            changed = true;
        }
        changed
    }
}

/// Prints progress and warnings to stderr.
fn stderr(e: Event) {
    match e {
        Event::Status(m) | Event::Warning(m) => eprintln!("innkeeper: {m}"),
    }
}

fn install(profile: &ProfileName, file: &Path) -> Result<()> {
    let p = ops::inspect(file, &stderr)?;
    ops::install(profile, &p, &stderr)?;
    println!("installed {} as \"{}\"", p.package, p.label);
    Ok(())
}

/// The package formats that `install` takes. `open` refuses them.
const PACKAGE_EXTENSIONS: [&str; 5] = ["apk", "apks", "xapk", "apkm", "hap"];

/// Opens documents in an Android app that the user picks. Each file is
/// granted to the session, and then the system app shows its app chooser.
fn open(profile: &ProfileName, files: &[PathBuf], token: Option<String>) -> Result<()> {
    if let Some(p) = files.iter().find(|p| {
        p.extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| PACKAGE_EXTENSIONS.iter().any(|x| e.eq_ignore_ascii_case(x)))
    }) {
        bail!(
            "{} is an app package. Install it with `innkeeper install`",
            p.display()
        );
    }
    ops::ensure_session(profile, &stderr)?;
    let grants = files
        .iter()
        .map(|p| grant(profile, p))
        .collect::<Result<Vec<_>>>()?;
    innkeeper_core::app_settings::launch(
        profile,
        &Launch::File {
            package: "dev.sargunv.innkeeper.system".into(),
            grants,
            component: Some("dev.sargunv.innkeeper.system/.documents.OpenWithActivity".into()),
        },
        &LaunchOptions {
            token,
            wait: false,
            new_window: false,
            size: None,
        },
    )
    .map(drop)
}

/// Grants profile `profile`'s session read access to the file at `path`, and
/// returns the grant's id.
fn grant(profile: &ProfileName, path: &Path) -> Result<u64> {
    let file = open_input(path)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("file needs a UTF-8 name"))?;
    innkeeper_core::app_settings::import(profile, &file, name)
}

fn uninstall(profile: &ProfileName, pkg: &str, keep_data: bool) -> Result<()> {
    if ops::uninstall(profile, pkg, keep_data, &stderr)? {
        println!("uninstalled {pkg}");
    } else {
        eprintln!("innkeeper: {pkg} isn't an installed user app");
    }
    Ok(())
}

/// Launches an app. If the session is stopped, a splash window shows right
/// away and passes focus to the app's window when that window opens.
fn run(
    profile: &ProfileName,
    pkg: &str,
    args: &[String],
    token: Option<String>,
    new_window: bool,
) -> Result<()> {
    ensure!(
        !new_window || args.is_empty(),
        "--new-window takes no files or URIs"
    );
    let pkg = validate::package(pkg)?;
    if !args.iter().all(|s| s.is_empty()) {
        profiles::need_android(profile, "opening files or URIs in apps")?;
    }
    let session = Session::of_profile(profile)?;
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
        ops::ensure_session(profile, &stderr)?;
        // The splash used the launch's activation token. The app's window
        // gets a new token from the splash.
        let token = match &splash {
            Some(s) => s.token(Duration::from_secs(2)),
            None => token,
        };
        let opts = LaunchOptions {
            token,
            wait: splash.is_some(),
            new_window,
            size: None,
        };
        if args.iter().all(|s| s.is_empty()) {
            return ops::run(profile, pkg, None, &opts, &stderr);
        }
        let mut reply = String::new();
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
                Some(path) => innkeeper_core::app_settings::launch(
                    profile,
                    &Launch::File {
                        package: pkg.into(),
                        grants: vec![grant(profile, &path)?],
                        component: None,
                    },
                    &opts,
                )?,
                None => ops::run(profile, pkg, Some(arg), &opts, &stderr)?,
            };
        }
        Ok(reply)
    })();
    if let Some(s) = splash {
        s.close();
    }
    r.map(drop)
}

/// Opens a regular file for granting. Special files are refused, and a FIFO
/// is refused without waiting for a writer.
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

/// The splash colour for an app whose entry has none. It matches Android's
/// light window background.
const DEFAULT_SPLASH: u32 = 0xfafafa;

/// Runs `app` on an OpenHarmony profile, which has no app settings yet.
fn ohos_app(profile: &ProfileName, cmd: AppCmd) -> Result<()> {
    use ops::BundleAction;
    let (package, action) = match cmd {
        AppCmd::Stop { package } => (package, BundleAction::Stop),
        AppCmd::ClearCache { package } => (package, BundleAction::ClearCache),
        AppCmd::ClearData { package } => (package, BundleAction::ClearData),
        AppCmd::Info { package, json } => {
            let info = ops::bundle_info(profile, &package, &stderr)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&info)?);
            } else {
                print!("{}", bundle_info_text(&info));
            }
            return Ok(());
        }
        AppCmd::Set { .. } => return profiles::need_android(profile, "app settings"),
    };
    ops::bundle_action(profile, &package, action, &stderr)
}

fn app(profile: &ProfileName, cmd: AppCmd) -> Result<()> {
    use innkeeper_core::app_settings::{Command, request};
    if profiles::settings(profile)?.system == System::Ohos {
        return ohos_app(profile, cmd);
    }
    let switch = |package: &str, action: &str, on: bool| {
        let mut c = Command::new(package, action);
        c.enabled = on;
        c
    };
    let commands = match cmd {
        AppCmd::Info { package, json } => {
            let info = request(profile, Command::new(&package, "info"), &stderr)?;
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
                "app set needs --background, --escape-back or --permission"
            );
            v
        }
    };
    for c in commands {
        request(profile, c, &stderr)?;
    }
    Ok(())
}

/// Formats `app info` output as text.
fn app_info_text(i: &innkeeper_core::app_settings::AppInfo) -> String {
    let on = |b: bool| if b { "on" } else { "off" };
    let mut s = sizes_text(i.app_bytes, i.data_bytes, i.cache_bytes);
    s.push_str(&format!(
        "background   {}\nescape-back  {}\n",
        on(i.background),
        on(i.escape_back),
    ));
    s.push_str(&permissions_text(&i.permissions));
    s
}

/// Formats `app info` output for an OpenHarmony bundle as text. OpenHarmony
/// has no background or Escape settings.
fn bundle_info_text(i: &innkeeper_core::ohos::BundleInfo) -> String {
    let mut s = format!("version      {} ({})\n", i.version_name, i.version_code);
    s.push_str(&sizes_text(i.app_bytes, i.data_bytes, i.cache_bytes));
    s.push_str(&permissions_text(&i.permissions));
    s
}

fn sizes_text(app: u64, data: u64, cache: u64) -> String {
    let mb = |b: u64| format!("{:.1} MB", b as f64 / 1e6);
    format!(
        "app size     {}\ndata         {} (cache {})\n",
        mb(app),
        mb(data),
        mb(cache)
    )
}

fn permissions_text(permissions: &[innkeeper_core::app_settings::Permission]) -> String {
    let mut s = String::new();
    if !permissions.is_empty() {
        s.push_str("permissions\n");
    }
    for p in permissions {
        s.push_str(&format!(
            "  {}  {}{}\n",
            p.name,
            if p.granted { "granted" } else { "denied" },
            if p.editable { "" } else { " (fixed)" }
        ));
    }
    s
}

fn list(profile: &ProfileName) -> Result<()> {
    for a in ops::list(profile, &stderr)? {
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

/// Runs a `profile` subcommand. `name` is the `--profile` value.
fn profile(name: &str, cmd: ProfileCmd) -> Result<()> {
    match cmd {
        ProfileCmd::List => {
            for p in profiles::list()? {
                let state = if p.running { "running" } else { "stopped" };
                let system = p.settings.system.name();
                let display = p.settings.display.name();
                // Android shows adb's serial and OpenHarmony its UDID, which
                // a debug signing profile lists.
                let id = match p.settings.system {
                    System::Ohos => format!("udid {}", innkeeper_profile::profile::udid(&p.name)),
                    System::Android if p.settings.adb => {
                        format!("adb {}", innkeeper_core::adb::serial(&p.settings))
                    }
                    System::Android => String::new(),
                };
                println!("{:<16} {state:<8} {system:<8} {display:<14} {id}", p.name);
            }
        }
        ProfileCmd::Create { name: n, system } => {
            let system = System::parse(&system).expect("clap checks it");
            profiles::create(&n, &Settings::new(system))?;
            println!("created profile {n} ({})", system.title());
        }
        ProfileCmd::Rename { from, to } => {
            profiles::rename(&from, &to)?;
            println!("renamed profile {from} to {to}");
        }
        ProfileCmd::Delete { name: n } => {
            profiles::delete(&n)?;
            println!("deleted profile {n}");
        }
        ProfileCmd::Start { all } => {
            let report = |m: &str| eprintln!("innkeeper: {m}");
            if all {
                // Keep starting the other profiles if one fails.
                let mut failed = None;
                for p in profiles::list()? {
                    if let Err(e) = profiles::start(&p.name, &report) {
                        eprintln!("innkeeper: profile {}: {e:#}", p.name);
                        failed = Some(e);
                    }
                }
                if let Some(e) = failed {
                    return Err(e);
                }
            } else {
                profiles::start(&profiles::existing(name)?, &report)?;
            }
        }
        ProfileCmd::Stop => profiles::stop(&profiles::existing(name)?)?,
        ProfileCmd::Reset => {
            let p = profiles::existing(name)?;
            profiles::reset(&p)?;
            println!("reset profile {p}");
        }
        ProfileCmd::Log { follow } => profiles::log(&profiles::existing(name)?, follow)?,
        ProfileCmd::Set(args) => set(&profiles::existing(name)?, &args)?,
    }
    Ok(())
}

/// Runs `profile set`.
fn set(name: &str, args: &SetArgs) -> Result<()> {
    let mut probe = innkeeper_profile::profile::Settings::default();
    let profile_change = args.apply(&mut probe);
    ensure!(
        profile_change || args.adb.is_some() || args.keep_running.is_some(),
        "nothing to change (see --help)"
    );
    if let Some(what) = args.android_only() {
        profiles::need_android(name, what)?;
    }
    if profile_change {
        let s = profiles::update(name, |s| {
            args.apply(s);
        })?;
        if args.display.is_some() {
            println!("display {} applies at the next start", s.display.name());
        }
    }
    if let Some(on) = &args.keep_running {
        innkeeper_core::preferences::save(innkeeper_profile::preferences::Preferences {
            keep_running: on == "on",
        })?;
    }
    if let Some(on) = &args.adb {
        let s = innkeeper_core::adb::set(name, on == "on", &stderr)?;
        if s.adb {
            println!(
                "adb on: {} ({})",
                innkeeper_core::adb::serial(&s),
                innkeeper_core::adb::address(&s)
            );
        } else {
            println!("adb off");
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    // A desktop launch's activation token is valid for one use. Remove it
    // from the environment so that child processes don't inherit it.
    let token = std::env::var("XDG_ACTIVATION_TOKEN")
        .ok()
        .and_then(|t| validate::activation_token(&t).map(str::to_string));
    // SAFETY: no other threads exist yet.
    unsafe {
        std::env::remove_var("XDG_ACTIVATION_TOKEN");
        std::env::remove_var("DESKTOP_STARTUP_ID");
    }
    let cli = Cli::parse();
    let res = (|| match cli.cmd {
        Cmd::Profile(cmd) => profile(&cli.profile, cmd),
        cmd => {
            let p = profiles::existing(&cli.profile)?;
            match cmd {
                Cmd::Open { files } => {
                    profiles::need_android(&p, "opening files")?;
                    open(&p, &files, token)
                }
                Cmd::Install { file } => install(&p, &file),
                Cmd::Uninstall { package, keep_data } => uninstall(&p, &package, keep_data),
                Cmd::Run {
                    package,
                    files_or_uri,
                    new_window,
                } => run(&p, &package, &files_or_uri, token, new_window),
                Cmd::List => list(&p),
                Cmd::App(cmd) => app(&p, cmd),
                Cmd::Profile(_) => unreachable!("handled above"),
            }
        }
    })();
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
    fn profile_is_named_by_flag_else_default() {
        let p = |args: &str| Cli::try_parse_from(args.split_whitespace()).map(|c| c.profile);
        assert_eq!(p("innkeeper list").unwrap(), "default");
        assert_eq!(p("innkeeper list --profile work").unwrap(), "work");
        assert_eq!(p("innkeeper --profile work run a.b").unwrap(), "work");
    }

    #[test]
    fn profile_set_covers_every_setting() {
        let cli = Cli::try_parse_from(
            "innkeeper --profile work profile set --display small-phone --idle-minutes 30 \
             --follow-dark off"
                .split_whitespace(),
        )
        .unwrap();
        assert_eq!(cli.profile, "work");
        let Cmd::Profile(ProfileCmd::Set(a)) = cli.cmd else {
            panic!("profile set did not parse");
        };
        let mut s = innkeeper_profile::profile::Settings::default();
        assert!(a.apply(&mut s));
        assert_eq!(s.display, innkeeper_profile::profile::Display::SmallPhone);
        assert_eq!(s.idle_minutes, 30);
        assert!(!s.follow_dark && s.follow_accent);
        // --keep-running and --adb don't change the profile's settings file.
        let Ok(Cmd::Profile(ProfileCmd::Set(a))) = parse("profile set --keep-running on --adb on")
        else {
            panic!("profile set did not parse");
        };
        assert!(!a.apply(&mut innkeeper_profile::profile::Settings::default()));
        for bad in [
            "profile set --idle-minutes 0",
            "profile set --idle-minutes 121",
            "profile set --display phone",
            "profile set --mode phone",
            "profile set --follow-dark maybe",
            "profile start work",
            "profile set work --adb on",
            "profile use work",
            "adb",
            "open",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert!(matches!(
            parse("profile log -f"),
            Ok(Cmd::Profile(ProfileCmd::Log { follow: true }))
        ));
        assert!(matches!(
            parse("profile reset"),
            Ok(Cmd::Profile(ProfileCmd::Reset))
        ));
        assert!(matches!(
            parse("profile start --all"),
            Ok(Cmd::Profile(ProfileCmd::Start { all: true }))
        ));
    }

    #[test]
    fn profiles_pick_a_system_and_android_only_settings_are_named() {
        let Ok(Cmd::Profile(ProfileCmd::Create { name, system })) =
            parse("profile create --system ohos harmony")
        else {
            panic!("profile create did not parse");
        };
        assert_eq!((name.as_str(), system.as_str()), ("harmony", "ohos"));
        assert!(matches!(
            parse("profile create work"),
            Ok(Cmd::Profile(ProfileCmd::Create { system, .. })) if system == "android"
        ));
        assert!(parse("profile create --system harmonyos x").is_err());
        assert!(parse("profile set --system ohos").is_err());
        let android_only = |args: &str| {
            let Ok(Cmd::Profile(ProfileCmd::Set(a))) = parse(args) else {
                panic!("{args} did not parse");
            };
            a.android_only()
        };
        assert_eq!(android_only("profile set --display medium-phone"), None);
        assert_eq!(android_only("profile set --keep-running on"), None);
        assert_eq!(
            android_only("profile set --display small-phone --adb on"),
            Some("--adb")
        );
        assert_eq!(
            android_only("profile set --follow-clock off"),
            Some("--follow-clock")
        );
    }

    #[test]
    fn open_refuses_apps_before_starting_anything() {
        let p = ProfileName::new("unit-test").unwrap();
        let e = open(
            &p,
            &[PathBuf::from("/x/a.txt"), PathBuf::from("/x/b.APKM")],
            None,
        );
        assert!(e.unwrap_err().to_string().contains("innkeeper install"));
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
    fn bundle_info_text_has_a_version_and_no_android_settings() {
        let info = innkeeper_core::ohos::BundleInfo {
            version_name: "1.0.2".into(),
            version_code: 1000002,
            app_bytes: 1_300_000,
            data_bytes: 2_100_000,
            cache_bytes: 16_000,
            permissions: vec![
                innkeeper_core::app_settings::Permission {
                    name: "ohos.permission.CAMERA".into(),
                    granted: false,
                    editable: true,
                },
                innkeeper_core::app_settings::Permission {
                    name: "ohos.permission.INTERNET".into(),
                    granted: true,
                    editable: false,
                },
            ],
        };
        assert_eq!(
            bundle_info_text(&info),
            "version      1.0.2 (1000002)\n\
             app size     1.3 MB\n\
             data         2.1 MB (cache 0.0 MB)\n\
             permissions\n  \
             ohos.permission.CAMERA  denied\n  \
             ohos.permission.INTERNET  granted (fixed)\n"
        );
        let bare = innkeeper_core::ohos::BundleInfo::default();
        assert!(!bundle_info_text(&bare).contains("permissions"));
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
