//! Package and activity operations inside the session (`cmd package`,
//! `cmd activity`, and the embedded icon helper under `app_process`).

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine as _;

use crate::session::Session;
use crate::validate;

/// The icon helper dex jar, built from `android/IconDump.java` by `build.rs`
/// (empty if the SDK wasn't available at build time).
const ICON_HELPER: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/icon-helper.jar"));
const ICON_HELPER_PATH: &str = "/data/local/tmp/linuxapk-icon.jar";

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
            bail!(
                "{}",
                validate::truncate(format!("{} {}", t.trim(), o.stderr.trim()).trim(), 600)
            );
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
        _ => bail!("{pkg} has no launcher activity (is it installed?)"),
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
}

/// The window manager's control command (docs/windowing.md): opens the app
/// in a window of its own, or focuses the window it already has.
pub fn wm_launch_args(l: &Launch) -> Vec<String> {
    let mut v: Vec<String> = [
        "am",
        "broadcast",
        "--user",
        "current",
        "-n",
        "dev.linuxapk.system/.wm.ControlReceiver",
        "-a",
        "dev.linuxapk.LAUNCH",
    ]
    .map(String::from)
    .to_vec();
    match l {
        Launch::Main { component } => {
            let pkg = component
                .split_once('/')
                .map_or(component.as_str(), |c| c.0);
            v.extend(["--es", "package", pkg, "--es", "component", component].map(String::from));
        }
        Launch::View { package, uri } => {
            v.extend(["--es", "package", package, "--es", "uri", uri].map(String::from));
        }
    }
    v
}

/// What the window manager answered: Ok(reply) on success, Err(reply) for an
/// error it reported, None if nothing answered the broadcast.
fn wm_reply(text: &str) -> Option<Result<String, String>> {
    let line = text
        .lines()
        .find(|l| l.starts_with("Broadcast completed:"))?;
    let data = line
        .split_once("data=\"")
        .map(|(_, d)| d.trim_end_matches('"').to_string())?;
    if line.contains("result=-1") {
        Some(Ok(data))
    } else {
        Some(Err(data))
    }
}

/// Starts an app through the window manager (one window per app).
pub fn launch(s: &Session, l: &Launch) -> Result<()> {
    let args = wm_launch_args(l);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let o = s.exec(60, &argv)?;
    match wm_reply(&o.text()) {
        Some(Ok(_)) => Ok(()),
        Some(Err(e)) => bail!("launch failed: {e}"),
        None => bail!(
            "launch failed: Linux.apk System's window manager did not answer (Android {})",
            if s.booted() {
                "has booted"
            } else {
                "is still booting"
            }
        ),
    }
}

/// What Android itself reports for an installed app.
#[derive(Debug, Default)]
pub struct AppInfo {
    pub label: String,
    pub category: Option<i32>,
    /// (size, PNG bytes)
    pub icons: Vec<(u32, Vec<u8>)>,
}

pub fn parse_helper_output(out: &str) -> AppInfo {
    let mut info = AppInfo::default();
    let b64 = base64::engine::general_purpose::STANDARD;
    for l in out.lines() {
        if let Some(v) = l.strip_prefix("label ") {
            info.label = v.to_string();
        } else if let Some(v) = l.strip_prefix("category ") {
            info.category = v.trim().parse().ok();
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
/// label and category), via the embedded helper under `app_process`.
pub fn app_info(s: &Session, pkg: &str, sizes: &[u32], work: &Path) -> Result<AppInfo> {
    if ICON_HELPER.is_empty() {
        bail!("this build has no icon helper (the Android SDK wasn't found at build time)");
    }
    std::fs::create_dir_all(work)?;
    let jar = work.join(format!("icon-helper-{}.jar", std::process::id()));
    std::fs::write(&jar, ICON_HELPER)?;
    let pushed = s.push_fixed(&jar, ICON_HELPER_PATH);
    let _ = std::fs::remove_file(&jar);
    pushed?;
    let sizes: Vec<String> = sizes.iter().map(u32::to_string).collect();
    let cp = format!("CLASSPATH={ICON_HELPER_PATH}");
    let mut argv = vec![
        "env",
        &cp,
        "app_process",
        "/system/bin",
        "dev.linuxapk.cli.IconDump",
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
    Ok(parse_helper_output(&o.text()))
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
    fn wm_args_and_reply() {
        let m = wm_launch_args(&Launch::Main {
            component: "org.wikipedia/.main.MainActivity".into(),
        });
        assert!(
            m.windows(3)
                .any(|w| w == ["--es", "package", "org.wikipedia"])
        );
        assert!(
            m.windows(3)
                .any(|w| w == ["--es", "component", "org.wikipedia/.main.MainActivity"])
        );
        let v = wm_launch_args(&Launch::View {
            package: "org.wikipedia".into(),
            uri: "https://en.wikipedia.org/wiki/Linux".into(),
        });
        assert!(
            v.windows(3)
                .any(|w| w == ["--es", "uri", "https://en.wikipedia.org/wiki/Linux"])
        );
        assert_eq!(
            wm_reply(
                "Broadcasting: x\nBroadcast completed: result=-1, data=\"launched display=2\"\n"
            ),
            Some(Ok("launched display=2".into()))
        );
        assert_eq!(
            wm_reply("Broadcast completed: result=0, data=\"error: no launcher activity\""),
            Some(Err("error: no launcher activity".into()))
        );
        assert_eq!(wm_reply("Broadcast completed: result=0\n"), None);
    }

    #[test]
    fn helper_output() {
        let png = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nrest");
        let i = parse_helper_output(&format!(
            "component org.x/.Main\nlabel Calc\ncategory -1\nicon 48 {png}\nicon 64 notbase64!\nicon 128 aGVsbG8=\n"
        ));
        assert_eq!(i.label, "Calc");
        assert_eq!(i.category, Some(-1));
        assert_eq!(i.icons.len(), 1);
        assert_eq!(i.icons[0].0, 48);
    }

    #[test]
    fn helper_embedded() {
        // The SDK is present on dev hosts; the jar is a zip with classes.dex.
        if !ICON_HELPER.is_empty() {
            assert!(ICON_HELPER.starts_with(b"PK"));
        }
    }
}
