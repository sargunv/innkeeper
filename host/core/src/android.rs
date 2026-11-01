//! Package operations inside the session through `cmd package`, and the types
//! that describe an app launch.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::launcher::Session;
use crate::{ErrorKind, validate};

/// `Success: created install session [1234]` -> 1234.
pub fn parse_session_id(out: &str) -> Option<u32> {
    let l = out.lines().find(|l| l.starts_with("Success"))?;
    let a = l.find('[')?;
    let b = l[a..].find(']')? + a;
    l[a + 1..b].parse().ok()
}

/// Installs one app from its APKs, base first, in a single PackageInstaller
/// session. Each file streams through stdin, so the session makes no copies.
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

/// Lists the user-installed packages (`cmd package list packages -3`) whose
/// names pass validation.
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
    /// A URI from the desktop (`%U`). It becomes a VIEW intent from an
    /// untrusted origin, so it carries BROWSABLE and reaches only activities
    /// that accept web links.
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
    /// A validated activation token, so that the compositor focuses the
    /// window.
    pub token: Option<String>,
    /// Wait until a new window shows its first frame before returning.
    pub wait: bool,
    /// Request a separate task even if the app already has a window.
    pub new_window: bool,
    /// The window's size in dp, used by test harnesses. None keeps the app's
    /// own size.
    pub size: Option<(u32, u32)>,
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
}
