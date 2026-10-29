//! tests/compat/results.json, the regression gate and local Markdown reports.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::app::{AppResult, Status};
use crate::budget::{Budgets, Measured};
use crate::corpus::{App, Corpus};

#[derive(Serialize, Deserialize, Default)]
pub struct Results {
    pub date: String,
    /// `ro.build.fingerprint` of the image under test.
    pub build: String,
    /// Whether the session ran without host windows.
    pub headless: bool,
    /// Session cgroup memory at the end of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_mem_mb: Option<u64>,
    /// The budgeted measurements (the last run that took each).
    #[serde(flatten)]
    pub measured: Measured,
    pub apps: Vec<AppResult>,
}

impl Results {
    pub fn load(path: &Path) -> Result<Self> {
        Ok(serde_json::from_str(&fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        fs::write(path, serde_json::to_string_pretty(self)? + "\n")?;
        Ok(())
    }
}

/// Apps whose result got worse than the baseline's. Harness errors don't
/// count: they say nothing about the app.
pub fn regressions(baseline: &Results, fresh: &[AppResult]) -> Vec<String> {
    fresh
        .iter()
        .filter(|r| r.result != Status::Error)
        .filter_map(|r| {
            let old = baseline.apps.iter().find(|o| o.package == r.package)?;
            (r.result.rank() < old.result.rank()).then(|| {
                format!(
                    "{}: {} -> {} ({})",
                    r.package,
                    old.result.label(),
                    r.result.label(),
                    r.reason
                )
            })
        })
        .collect()
}

fn count(apps: &[AppResult], st: Status) -> usize {
    apps.iter().filter(|a| a.result == st).count()
}

pub fn summary(apps: &[AppResult]) -> String {
    format!(
        "{} apps: {} work, {} partly, {} don't, {} not tested",
        apps.len(),
        count(apps, Status::Works),
        count(apps, Status::Partly),
        count(apps, Status::Broken),
        count(apps, Status::Error)
    )
}

/// What kind of code the APK carries.
fn kind(a: &App) -> String {
    match a.native.as_str() {
        "none" => "none".to_string(),
        n if n.split(',').any(|x| x == "x86_64") => "x86_64".to_string(),
        n => format!("{n} only"),
    }
}

pub fn write_markdown(
    path: &Path,
    res: &Results,
    corpus: &Corpus,
    budgets: &Budgets,
) -> Result<()> {
    let mut md = String::from(
        "# Compatibility\n\n\
     Apps from the F-Droid corpus (`tests/compat/corpus.toml`), smoke-tested \
     by the compat harness (`mise run test:compat`): install, launch, a \
     non-blank first frame, then a tap, a scroll and Back without a crash or \
     ANR, plus an optional per-app script. Failures are triaged in \
     [tests/compat](https://github.com/sargunv/linux.apk/tree/main/tests/compat). \
     Nightly results are published on the \
     [wiki](https://github.com/sargunv/linux.apk/wiki/Compatibility). \
     Start is the time to the first \
     frame on a fresh install; memory is the app's total PSS after the \
     interactions.\n\n",
    );
    md += &format!(
        "Last run: {}, image `{}`, {} session. {}.\n\n",
        res.date,
        res.build,
        if res.headless { "headless" } else { "windowed" },
        summary(&res.apps)
    );
    md += "| App | Version | Framework | Native code | Result | Start | Memory | Notes |\n";
    md += "| --- | --- | --- | --- | --- | --- | --- | --- |\n";
    for a in &corpus.app {
        let Some(r) = res.apps.iter().find(|r| r.package == a.package) else {
            continue;
        };
        let start = r.launch_ms.map(|ms| format!("{ms} ms")).unwrap_or_default();
        let mem = r.pss_mb.map(|mb| format!("{mb} MB")).unwrap_or_default();
        let mut notes = r.reason.replace('|', "\\|");
        if r.result == Status::Works {
            notes.clear();
        }
        md += &format!(
            "| {} | {} | {} | {} | {} | {start} | {mem} | {notes} |\n",
            r.name,
            r.version_name,
            a.framework,
            kind(a),
            r.result.label()
        );
    }
    if let Some(mb) = res.session_mem_mb {
        md += &format!("\nSession memory (cgroup) at the end of the run: {mb} MiB.\n");
    }
    md += "\n## Budgets\n\n\
         A run fails when a measurement is over its budget by more than the \
         margin (`tests/compat/budgets.toml`). Cold session start is `start` to \
         `sys.boot_completed` with no session running, on a `/data` that has \
         booted before. Warm app start is the median start of the apps that \
         work, in the running session (full runs only). Idle session memory \
         is the session cgroup's memory without page cache, 90 s after a \
         cold start. Session start and idle memory are measured only when the \
         run starts the session itself.\n\n\
         | Measure | Budget | Margin | Fails above | Last measured |\n\
         | --- | --- | --- | --- | --- |\n";
    for l in budgets.lines(&res.measured) {
        let m = l
            .measured
            .map(|m| format!("{m} {}", l.unit))
            .unwrap_or_default();
        md += &format!(
            "| {} | {} {u} | {:.0}% | {} {u} | {m} |\n",
            l.name,
            l.budget,
            l.margin * 100.0,
            l.limit,
            u = l.unit
        );
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, md)?;
    Ok(())
}
