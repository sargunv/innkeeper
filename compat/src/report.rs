//! compat/results.json, the regression gate and COMPATIBILITY.md.

use std::fs;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::app::{AppResult, Status};
use crate::corpus::Corpus;

#[derive(Serialize, Deserialize, Default)]
pub struct Results {
    pub date: String,
    /// `ro.build.fingerprint` of the image under test.
    pub build: String,
    /// Composer backend during the run (`headless` or `wayland`).
    pub backend: String,
    /// Session cgroup memory at the end of the run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_mem_mb: Option<u64>,
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

fn group_label(g: &str) -> &str {
    match g {
        "noarch" => "Java/Kotlin",
        "compose" => "Compose",
        "native" => "x86_64 native",
        g => g,
    }
}

pub fn write_markdown(path: &Path, res: &Results, corpus: &Corpus) -> Result<()> {
    let mut md = String::from(
        "# Compatibility\n\n\
     Apps from the F-Droid corpus (`compat/corpus.toml`), smoke-tested by the \
     compat harness (`compat/run.sh`): install, launch, a non-blank first \
     frame, then a tap, a scroll and Back without a crash or ANR, plus an \
     optional per-app script. Failures are triaged in \
     [docs/compat-triage.md](docs/compat-triage.md).\n\n",
    );
    md += &format!(
        "Last run: {}, image `{}`, {} composer. {}.\n\n",
        res.date,
        res.build,
        res.backend,
        summary(&res.apps)
    );
    md += "| App | Version | Kind | Result | Cold start | Notes |\n";
    md += "| --- | --- | --- | --- | --- | --- |\n";
    for a in &corpus.app {
        let Some(r) = res.apps.iter().find(|r| r.package == a.package) else {
            continue;
        };
        let start = r.launch_ms.map(|ms| format!("{ms} ms")).unwrap_or_default();
        let mut notes = r.reason.replace('|', "\\|");
        if r.result == Status::Works {
            notes.clear();
        }
        if a.native == "arm64-v8a" {
            notes += if notes.is_empty() { "" } else { "; " };
            notes += "arm64-only APK";
        }
        md += &format!(
            "| {} | {} | {} | {} | {start} | {notes} |\n",
            r.name,
            r.version_name,
            group_label(&r.group),
            r.result.label()
        );
    }
    if let Some(mb) = res.session_mem_mb {
        md += &format!("\nSession memory (cgroup) at the end of the run: {mb} MiB.\n");
    }
    fs::write(path, md)?;
    Ok(())
}
