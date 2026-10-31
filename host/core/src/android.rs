//! Package and activity operations inside the session (`cmd package`,
//! `cmd activity`, and the system app's icon helper under `app_process`).

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine as _;

use crate::launcher::Session;
use crate::{ErrorKind, validate};

/// The system app, whose `IconDump` renders app icons (and reports labels).
const SYSTEM_APK: &str = "/system_ext/priv-app/InnkeeperSystem/InnkeeperSystem.apk";

/// `Success: created install session [1234]` -> 1234.
pub fn parse_session_id(out: &str) -> Option<u32> {
    let l = out.lines().find(|l| l.starts_with("Success"))?;
    let a = l.find('[')?;
    let b = l[a..].find(']')? + a;
    l[a + 1..b].parse().ok()
}

/// Installs one app from its APKs (base first) in one PackageInstaller
/// session, streaming each file through stdin (no copies in the session).
pub fn install(s: &Session, apks: &[&Path]) -> Result<()> {
    install_apks(s, apks).map_err(crate::errors::classify_install_error)
}

fn install_apks(s: &Session, apks: &[&Path]) -> Result<()> {
    let total: u64 = apks
        .iter()
        .map(|p| p.metadata().map(|m| m.len()).unwrap_or(0))
        .sum();
    let out = s.exec_ok(
        60,
        &[
            "cmd",
            "package",
            "install-create",
            "-r",
            "-S",
            &total.to_string(),
        ],
    )?;
    let sid = parse_session_id(&out).with_context(|| format!("install-create: {}", out.trim()))?;
    let sid_s = sid.to_string();
    let res = (|| -> Result<()> {
        for (i, p) in apks.iter().enumerate() {
            let size = p.metadata()?.len().to_string();
            let name = if i == 0 {
                "base.apk".to_string()
            } else {
                format!("split{i}.apk")
            };
            let f = File::open(p)?;
            let o = s.exec_with(
                600,
                &[
                    "cmd",
                    "package",
                    "install-write",
                    "-S",
                    &size,
                    &sid_s,
                    &name,
                    "-",
                ],
                f.into(),
            )?;
            let t = o.text();
            if !o.ok() || !t.contains("Success") {
                bail!("install-write {name}: {} {}", t.trim(), o.stderr.trim());
            }
        }
        let o = s.exec(600, &["cmd", "package", "install-commit", &sid_s])?;
        let t = o.text();
        if !t.lines().any(|l| l.trim() == "Success") {
            bail!("{} {}", t.trim(), o.stderr.trim());
        }
        Ok(())
    })();
    if res.is_err() {
        let _ = s.exec(30, &["cmd", "package", "install-abandon", &sid_s]);
    }
    res
}

/// User-installed packages (`pm list packages -3`), validated.
pub fn user_packages(s: &Session) -> Result<Vec<String>> {
    let out = s.exec_ok(
        60,
        &[
            "cmd", "package", "list", "packages", "-3", "--user", "current",
        ],
    )?;
    let mut v: Vec<String> = out
        .lines()
        .filter_map(|l| l.trim().strip_prefix("package:"))
        .filter(|p| validate::package(p).is_ok())
        .map(str::to_string)
        .collect();
    v.sort();
    Ok(v)
}

pub fn uninstall(s: &Session, pkg: &str, keep_data: bool) -> Result<()> {
    let mut argv = vec!["cmd", "package", "uninstall"];
    if keep_data {
        argv.push("-k");
    }
    argv.push(pkg);
    let o = s.exec(120, &argv)?;
    let t = o.text();
    if !t.lines().any(|l| l.trim() == "Success") {
        bail!("uninstall {pkg}: {} {}", t.trim(), o.stderr.trim());
    }
    Ok(())
}

/// The launcher activity, e.g. `org.wikipedia/.main.MainActivity`.
pub fn launcher_component(s: &Session, pkg: &str) -> Result<String> {
    let out = s.exec_ok(
        30,
        &[
            "cmd",
            "package",
            "resolve-activity",
            "--brief",
            "--user",
            "current",
            "-a",
            "android.intent.action.MAIN",
            "-c",
            "android.intent.category.LAUNCHER",
            pkg,
        ],
    )?;
    let comp = out.lines().last().unwrap_or("").trim();
    match comp.split_once('/') {
        Some((p, _)) if p == pkg && !comp.contains(char::is_whitespace) => Ok(comp.to_string()),
        _ => Err(
            anyhow::anyhow!("{pkg} has no launcher activity (is it installed?)")
                .context(ErrorKind::NoLauncherActivity),
        ),
    }
}

/// What to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// The app's launcher activity, brought to front like a launcher does.
    Main { component: String },
    /// A URI from the desktop (`%U`): an untrusted-origin VIEW intent, so it
    /// carries BROWSABLE and only reaches activities that accept web links.
    View { package: String, uri: String },
    /// A read-only fd grant from the bridge documents service.
    File {
        package: String,
        grants: Vec<u64>,
        /// Explicit target for the system app's private Open With chooser.
        component: Option<String>,
    },
}

/// How a launch's window comes up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchOptions {
    /// An activation token for the window (validated), so the compositor
    /// focuses it.
    pub token: Option<String>,
    /// Return only once a new window shows its first frame.
    pub wait: bool,
    /// Request a separate task even if the app already has a window.
    pub new_window: bool,
    /// The window's size in dp (harnesses); None: the app's own.
    pub size: Option<(u32, u32)>,
}

/// What Android itself reports for an installed app.
#[derive(Debug, Default)]
pub struct AppInfo {
    pub label: String,
    pub category: Option<i32>,
    /// Its launcher activity's splash background (`0xRRGGBB`) and window
    /// size in dp.
    pub splash: Option<u32>,
    pub window: Option<(u32, u32)>,
    /// (size, PNG bytes)
    pub icons: Vec<(u32, Vec<u8>)>,
    pub mime_types: Vec<String>,
}

pub fn parse_helper_output(out: &str) -> AppInfo {
    let mut info = AppInfo::default();
    let b64 = base64::engine::general_purpose::STANDARD;
    for l in out.lines() {
        if let Some(v) = l.strip_prefix("label ") {
            info.label = v.to_string();
        } else if let Some(v) = l.strip_prefix("mime ") {
            info.mime_types.push(v.to_string());
        } else if let Some(v) = l.strip_prefix("category ") {
            info.category = v.trim().parse().ok();
        } else if let Some(v) = l.strip_prefix("splash ") {
            // AARRGGBB; a transparent one shows whatever is behind.
            info.splash = u32::from_str_radix(v.trim(), 16)
                .ok()
                .filter(|c| c >> 24 != 0)
                .map(|c| c & 0xff_ffff);
        } else if let Some(v) = l.strip_prefix("window ")
            && let Some((w, h)) = v.trim().split_once(' ')
            && let (Ok(w), Ok(h)) = (w.parse::<u32>(), h.parse::<u32>())
            && (1..=16384).contains(&w)
            && (1..=16384).contains(&h)
        {
            info.window = Some((w, h));
        } else if let Some(v) = l.strip_prefix("icon ")
            && let Some((size, data)) = v.split_once(' ')
            && let Ok(size) = size.parse::<u32>()
            && let Ok(png) = b64.decode(data.trim())
            && png.starts_with(b"\x89PNG\r\n\x1a\n")
        {
            info.icons.push((size, png));
        }
    }
    info
}

/// Asks Android to render the app's launcher icon at `sizes` (and report its
/// label and category), via the system app's `IconDump` under `app_process`.
pub fn app_info(s: &Session, pkg: &str, sizes: &[u32]) -> Result<AppInfo> {
    let sizes: Vec<String> = sizes.iter().map(u32::to_string).collect();
    let cp = format!("CLASSPATH={SYSTEM_APK}");
    let mut argv = vec![
        "env",
        &cp,
        "app_process",
        "/",
        "dev.sargunv.innkeeper.system.apps.IconDump",
        pkg,
    ];
    argv.extend(sizes.iter().map(String::as_str));
    let o = s.exec(120, &argv)?;
    if !o.ok() {
        bail!(
            "icon helper failed: {}",
            validate::truncate(o.stderr.trim(), 600)
        );
    }
    let info = parse_helper_output(&o.text());
    if info.icons.len() < sizes.len() {
        bail!(
            "the icon helper rendered {} of {} icons",
            info.icons.len(),
            sizes.len()
        );
    }
    Ok(info)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id() {
        assert_eq!(
            parse_session_id("Success: created install session [1834552]\n"),
            Some(1834552)
        );
        assert_eq!(parse_session_id("Failure [x]\n"), None);
    }

    #[test]
    fn helper_output() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nrest");
        let i = parse_helper_output(&format!(
            "component org.x/.Main\nlabel Calc\ncategory -1\nsplash ff202124\nwindow 412 915\nicon 48 {png}\nicon 64 notbase64!\nicon 128 aGVsbG8=\n"
        ));
        assert_eq!(i.label, "Calc");
        assert_eq!(i.category, Some(-1));
        assert_eq!(i.splash, Some(0x202124));
        assert_eq!(i.window, Some((412, 915)));
        assert_eq!(i.icons.len(), 1);
        assert_eq!(i.icons[0].0, 48);
    }
}
