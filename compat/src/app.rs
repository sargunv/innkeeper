//! One app through the harness: install, launch, first frame, interactions,
//! logs, uninstall.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::corpus::App;
use crate::frame;
use crate::session::Session;

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Works,
    Partly,
    Broken,
    /// The harness or session failed, not the app; not a result.
    Error,
}

impl Status {
    pub fn rank(self) -> u8 {
        match self {
            Status::Works => 2,
            Status::Partly => 1,
            Status::Broken | Status::Error => 0,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Status::Works => "Works",
            Status::Partly => "Partly works",
            Status::Broken => "Doesn't work",
            Status::Error => "Not tested",
        }
    }
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Check {
    pub name: String,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct AppResult {
    pub package: String,
    pub name: String,
    pub group: String,
    pub version_name: String,
    pub version_code: u64,
    pub result: Status,
    pub reason: String,
    /// `am start -W` TotalTime (cold start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_ms: Option<u64>,
    /// Total PSS of the app process after the interactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pss_mb: Option<u64>,
    pub checks: Vec<Check>,
}

const TMP_APK: &str = "/data/local/tmp/compat.apk";
const UI_XML: &str = "/data/local/tmp/compat-ui.xml";

pub struct Run<'a> {
    s: &'a Session,
    app: &'a App,
    dir: PathBuf,
    checks: Vec<Check>,
    launch_ms: Option<u64>,
    pss_mb: Option<u64>,
}

/// Why a step ended the run early.
struct Stop(Status, String);

impl<'a> Run<'a> {
    pub fn new(s: &'a Session, app: &'a App, out: &Path) -> Self {
        let dir = out.join(&app.package);
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::create_dir_all(&dir);
        Self {
            s,
            app,
            dir,
            checks: vec![],
            launch_ms: None,
            pss_mb: None,
        }
    }

    fn check(&mut self, name: &str, ok: bool, detail: impl Into<String>) -> bool {
        let detail = detail.into();
        eprintln!(
            "  {} {name}{}",
            if ok { "ok  " } else { "FAIL" },
            if detail.is_empty() {
                String::new()
            } else {
                format!(": {detail}")
            }
        );
        self.checks.push(Check {
            name: name.into(),
            ok,
            detail,
        });
        ok
    }

    fn save(&self, name: &str, text: &str) {
        let _ = fs::write(self.dir.join(name), text);
    }

    pub fn run(mut self, apk: &Path) -> AppResult {
        let pkg = self.app.package.clone();
        let outcome = self.steps(apk);
        self.collect();
        self.s.run(120, &["pm", "uninstall", &pkg]);
        self.s.run(15, &["rm", "-f", TMP_APK, UI_XML]);
        let (result, reason) = match outcome {
            Err(Stop(st, why)) => (st, why),
            Ok(()) => {
                let failed: Vec<_> = self
                    .checks
                    .iter()
                    .filter(|c| !c.ok)
                    .map(|c| c.name.clone())
                    .collect();
                if failed.is_empty() {
                    (
                        Status::Works,
                        "launches, renders, survives tap/scroll/back".into(),
                    )
                } else {
                    (
                        Status::Partly,
                        format!("first frame ok; failed: {}", failed.join(", ")),
                    )
                }
            }
        };
        eprintln!("  => {}: {reason}", result.label());
        let r = AppResult {
            package: pkg,
            name: self.app.name.clone(),
            group: self.app.group.clone(),
            version_name: self.app.version_name.clone(),
            version_code: self.app.version_code,
            result,
            reason,
            launch_ms: self.launch_ms,
            pss_mb: self.pss_mb,
            checks: self.checks,
        };
        if let Ok(j) = serde_json::to_string_pretty(&r) {
            let _ = fs::write(self.dir.join("result.json"), j + "\n");
        }
        r
    }

    fn steps(&mut self, apk: &Path) -> Result<(), Stop> {
        let pkg = self.app.package.clone();
        let s = self.s;
        s.run(120, &["pm", "uninstall", &pkg]);

        // Install.
        if let Err(e) = s.push(apk, TMP_APK) {
            return Err(Stop(Status::Error, format!("{e:#}")));
        }
        let o = s.run(
            300,
            &[
                "pm",
                "install",
                "-r",
                "-g",
                "--bypass-low-target-sdk-block",
                TMP_APK,
            ],
        );
        self.save("install.txt", &o.text);
        if !o.text.contains("Success") {
            let why = o
                .text
                .lines()
                .find(|l| l.contains("Failure") || l.contains("Exception"))
                .unwrap_or("pm install failed")
                .trim()
                .to_string();
            self.check("install", false, why.clone());
            return Err(Stop(Status::Broken, format!("install: {why}")));
        }
        self.check("install", true, "");

        // Launcher activity.
        let o = s.run(
            30,
            &[
                "cmd",
                "package",
                "resolve-activity",
                "--brief",
                "-a",
                "android.intent.action.MAIN",
                "-c",
                "android.intent.category.LAUNCHER",
                &pkg,
            ],
        );
        let Some(component) = o
            .text
            .lines()
            .map(str::trim)
            .rfind(|l| l.starts_with(&format!("{pkg}/")))
        else {
            self.check("launcher activity", false, o.text.trim());
            return Err(Stop(Status::Broken, "no launcher activity".into()));
        };
        let component = component.to_string();

        // Launch from a clean slate.
        s.run(30, &["logcat", "-b", "main,system,crash", "-c"]);
        let o = s.run(90, &["am", "start", "-W", "-n", &component]);
        self.save("am-start.txt", &o.text);
        if o.text.contains("Error") || !o.text.contains("Status: ok") {
            let why = o
                .text
                .lines()
                .find(|l| l.contains("Error"))
                .unwrap_or("timeout");
            self.check("launch", false, why.trim());
            return Err(Stop(Status::Broken, format!("launch: {}", why.trim())));
        }
        self.launch_ms = o
            .text
            .lines()
            .find_map(|l| l.strip_prefix("TotalTime: "))
            .and_then(|v| v.trim().parse().ok());
        self.check(
            "launch",
            true,
            format!("{} ms", self.launch_ms.unwrap_or(0)),
        );

        // Settle, watching for an early death.
        let settle = Duration::from_secs(self.app.settle.unwrap_or(8));
        let t = Instant::now();
        while t.elapsed() < settle {
            sleep(Duration::from_secs(2));
            if let Some(why) = self.dead() {
                self.check("alive after launch", false, why.clone());
                return Err(Stop(Status::Broken, why));
            }
        }
        self.check("alive after launch", true, "");

        // First frame.
        let focus = self.focus();
        let focused = focus.contains(&pkg);
        self.check("app window focused", focused, focus.clone());
        let png = self.dir.join("first.png");
        match self.screencap(&png) {
            Ok(st) if st.blank() => {
                self.check("first frame", false, format!("blank: {}", st.describe()));
                return Err(Stop(
                    Status::Broken,
                    format!("blank frame ({})", st.describe()),
                ));
            }
            Ok(st) => {
                self.check("first frame", true, st.describe());
            }
            Err(e) => {
                self.check("first frame", false, format!("{e:#}"));
                return Err(Stop(Status::Error, format!("screencap: {e:#}")));
            }
        }
        if !focused {
            let why = if focus.contains("Not Responding") {
                "ANR dialog"
            } else if focus.contains("Application Error") {
                "crash dialog"
            } else {
                "app window not in front"
            };
            return Err(Stop(Status::Broken, format!("{why} ({focus})")));
        }

        // Per-app script, from a fresh first screen.
        let script = self.app.script.clone();
        if !script.is_empty() {
            let res = self.script(&script);
            self.check("script", res.is_ok(), res.err().unwrap_or_default());
            if let Some(why) = self.dead() {
                self.check("alive after script", false, why);
                return Ok(());
            }
        }

        // Generic interactions.
        for (name, cmd) in [
            ("tap centre", vec!["input", "tap", "640", "420"]),
            (
                "scroll",
                vec!["input", "swipe", "640", "600", "640", "250", "300"],
            ),
            ("back", vec!["input", "keyevent", "BACK"]),
        ] {
            s.run(15, &cmd);
            sleep(Duration::from_millis(2500));
            if let Some(why) = self.dead() {
                self.check(&format!("alive after {name}"), false, why);
                return Ok(());
            }
        }
        self.check("alive after tap/scroll/back", true, "");
        self.pss_mb = self.pss();
        let _ = self.screencap(&self.dir.join("after.png"));
        Ok(())
    }

    /// A crash, ANR or process death since launch, if any.
    fn dead(&self) -> Option<String> {
        let pkg = &self.app.package;
        let crash = self
            .s
            .run(20, &["logcat", "-d", "-b", "crash", "-t", "400"])
            .text;
        if crash.contains(pkg.as_str()) {
            let first = crash
                .lines()
                .find(|l| l.contains("Exception") || l.contains("Error") || l.contains("signal"))
                .map(|l| l.split_once(": ").map_or(l, |x| x.1).trim().to_string())
                .unwrap_or_default();
            return Some(format!("crash: {first}"));
        }
        let am = self
            .s
            .run(
                20,
                &[
                    "logcat",
                    "-d",
                    "-b",
                    "system,main",
                    "-t",
                    "4000",
                    "-s",
                    "ActivityManager",
                ],
            )
            .text;
        if let Some(l) = am
            .lines()
            .find(|l| l.contains("ANR in") && l.contains(pkg.as_str()))
        {
            return Some(format!(
                "ANR: {}",
                l.split_once("ANR in").map_or(l, |x| x.1).trim()
            ));
        }
        if !self.s.run(15, &["pidof", pkg]).ok {
            let why = am
                .lines()
                .rfind(|l| {
                    l.contains(pkg.as_str())
                        && (l.contains("has died") || l.contains("Killing") || l.contains("Force"))
                })
                .map(|l| l.split_once(": ").map_or(l, |x| x.1).trim().to_string())
                .unwrap_or_else(|| "no crash logged".into());
            return Some(format!("process gone: {why}"));
        }
        None
    }

    fn focus(&self) -> String {
        let o = self.s.run(30, &["dumpsys", "window", "displays"]);
        o.text
            .lines()
            .find_map(|l| l.trim().strip_prefix("mCurrentFocus="))
            .unwrap_or("?")
            .to_string()
    }

    fn screencap(&self, dst: &Path) -> anyhow::Result<frame::FrameStats> {
        let tmp = "/data/local/tmp/compat-screen.png";
        let o = self.s.run(30, &["screencap", "-p", tmp]);
        if !o.ok {
            anyhow::bail!("screencap failed: {}", o.text.trim());
        }
        self.s.pull(tmp, dst)?;
        self.s.run(10, &["rm", "-f", tmp]);
        frame::analyze(dst)
    }

    fn pss(&self) -> Option<u64> {
        let o = self
            .s
            .run(30, &["dumpsys", "meminfo", "-s", &self.app.package]);
        let l = o
            .text
            .lines()
            .find(|l| l.trim_start().starts_with("TOTAL PSS:"))?;
        let kb: u64 = l.split_whitespace().nth(2)?.parse().ok()?;
        Some(kb / 1024)
    }

    fn script(&self, steps: &[String]) -> Result<(), String> {
        for step in steps {
            let (op, arg) = step.split_once(':').ok_or(format!("bad step {step}"))?;
            match op {
                "wait" => sleep(Duration::from_secs_f64(arg.parse().unwrap_or(1.0))),
                "key" => {
                    self.s.run(15, &["input", "keyevent", arg]);
                    sleep(Duration::from_millis(700));
                }
                "tap" => {
                    let nodes = self.ui_nodes()?;
                    let n = nodes
                        .iter()
                        .find(|n| n.text == arg || n.desc == arg)
                        .ok_or(format!("{step}: no such node"))?;
                    let (x, y) = n.centre;
                    self.s
                        .run(15, &["input", "tap", &x.to_string(), &y.to_string()]);
                    sleep(Duration::from_millis(700));
                }
                "expect" => {
                    let nodes = self.ui_nodes()?;
                    if !nodes
                        .iter()
                        .any(|n| n.text.contains(arg) || n.desc.contains(arg))
                    {
                        return Err(format!("{step}: not on screen"));
                    }
                }
                _ => return Err(format!("unknown step {step}")),
            }
        }
        Ok(())
    }

    fn ui_nodes(&self) -> Result<Vec<Node>, String> {
        // The dump waits for the UI to go idle, which busy apps rarely do on
        // the first try.
        let mut last = String::new();
        for _ in 0..3 {
            let o = self.s.sh(
                60,
                &format!("rm -f {UI_XML}; uiautomator dump {UI_XML} >/dev/null && cat {UI_XML}"),
            );
            if o.text.contains("<hierarchy") {
                return Ok(parse_nodes(&o.text));
            }
            last = o.text.trim().to_string();
        }
        Err(format!("uiautomator dump failed: {last}"))
    }

    /// Bounded logs and state for triage.
    fn collect(&self) {
        let s = self.s;
        let logs = [
            (
                "logcat.txt",
                vec!["logcat", "-d", "-t", "3000", "-b", "main,system,crash"],
            ),
            (
                "crash.txt",
                vec!["logcat", "-d", "-t", "500", "-b", "crash"],
            ),
        ];
        for (f, cmd) in logs {
            self.save(f, &s.run(30, &cmd).text);
        }
        self.save("focus.txt", &self.focus());
    }
}

pub struct Node {
    pub text: String,
    pub desc: String,
    pub centre: (i32, i32),
}

fn attr<'a>(node: &'a str, name: &str) -> &'a str {
    let key = format!(" {name}=\"");
    node.find(&key)
        .map(|i| &node[i + key.len()..])
        .and_then(|r| r.find('"').map(|j| &r[..j]))
        .unwrap_or("")
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Text, content-desc and bounds of each node in a `uiautomator dump`.
pub fn parse_nodes(xml: &str) -> Vec<Node> {
    xml.split("<node ")
        .skip(1)
        .filter_map(|n| {
            let n = &format!(" {n}");
            let b = attr(n, "bounds");
            let v: Vec<i32> = b
                .split(|c: char| !c.is_ascii_digit() && c != '-')
                .filter(|x| !x.is_empty())
                .filter_map(|x| x.parse().ok())
                .collect();
            if v.len() != 4 {
                return None;
            }
            Some(Node {
                text: unescape(attr(n, "text")),
                desc: unescape(attr(n, "content-desc")),
                centre: ((v[0] + v[2]) / 2, (v[1] + v[3]) / 2),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nodes() {
        let xml = r#"<hierarchy><node index="0" text="7" resource-id="" content-desc="" bounds="[487,381][633,477]"><node text="" content-desc="a &amp; b" bounds="[0,0][10,20]"/></node></hierarchy>"#;
        let n = parse_nodes(xml);
        assert_eq!(n.len(), 2);
        assert_eq!(n[0].text, "7");
        assert_eq!(n[0].centre, (560, 429));
        assert_eq!(n[1].desc, "a & b");
    }
}
