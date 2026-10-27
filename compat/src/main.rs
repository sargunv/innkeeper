//! Linux.apk compat harness. See docs/compat-triage.md.
//!
//!   linuxapk-compat [run] [--window] [--only PKG,...] [--accept] [--no-finish]
//!   linuxapk-compat fetch
//!   linuxapk-compat report
//!
//! `run` fetches the corpus, makes sure a session is up (starting a headless
//! one if needed), runs every app, writes out/compat/, and then, unless an app
//! that worked before got worse, compat/results.json and COMPATIBILITY.md.
//! `--accept` records the results even with regressions.

mod app;
mod corpus;
mod frame;
mod report;
mod session;

use std::path::PathBuf;
use std::process::{Command, ExitCode};
use std::time::Duration;
use std::{env, fs};

use anyhow::{Context, Result, bail};

use app::{AppResult, Run, Status};
use report::Results;
use session::Session;

struct Opts {
    cmd: String,
    window: bool,
    only: Vec<String>,
    accept: bool,
    finish: bool,
}

fn parse_args() -> Result<Opts> {
    let mut o = Opts {
        cmd: "run".into(),
        window: false,
        only: vec![],
        accept: false,
        finish: true,
    };
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "run" | "fetch" | "report" => o.cmd = a,
            "--window" => o.window = true,
            "--accept" => o.accept = true,
            "--no-finish" => o.finish = false,
            "--only" => {
                let v = args.next().context("--only needs a value")?;
                o.only.extend(v.split(',').map(str::to_string));
            }
            _ => bail!("unknown argument {a} (see compat/src/main.rs)"),
        }
    }
    Ok(o)
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("compat: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode> {
    let o = parse_args()?;
    let repo = PathBuf::from(env::var("LINUXAPK_REPO").unwrap_or_else(|_| ".".into()));
    let launcher = env::var("LINUXAPK_SESSION")
        .map(PathBuf::from)
        .unwrap_or_else(|_| repo.join("session/linuxapk-session"));
    let corpus = corpus::load(&repo.join("compat/corpus.toml"))?;
    let baseline_path = repo.join("compat/results.json");
    let baseline = Results::load(&baseline_path).unwrap_or_default();

    if o.cmd == "report" {
        report::write_markdown(&repo.join("COMPATIBILITY.md"), &baseline, &corpus)?;
        return Ok(ExitCode::SUCCESS);
    }

    let apps: Vec<_> = corpus
        .app
        .iter()
        .filter(|a| o.only.is_empty() || o.only.contains(&a.package))
        .collect();
    if apps.is_empty() {
        bail!("no apps selected");
    }
    let apk_dir = repo.join("out/corpus");
    let mut apks = vec![];
    for a in &apps {
        apks.push(corpus::fetch(a, &apk_dir)?);
    }
    if o.cmd == "fetch" {
        return Ok(ExitCode::SUCCESS);
    }

    let s = Session::new(launcher);
    s.ensure(o.window)?;
    let out = repo.join("out/compat");
    fs::create_dir_all(&out)?;
    let mut results: Vec<AppResult> = vec![];
    for (a, apk) in apps.iter().zip(&apks) {
        let mut tries = 0;
        loop {
            tries += 1;
            eprintln!("compat: {} {} ({})", a.name, a.version_name, a.package);
            if let Err(e) = s.ensure(o.window) {
                eprintln!("compat: session: {e:#}");
            }
            let pid = s.pid();
            let mut r = Run::new(&s, a, &out).run(apk);
            // Another lane may restart the session under us; that's not the app's
            // fault, so retry once on a fresh session.
            if pid.is_none() || s.pid() != pid || !s.booted() {
                r.result = Status::Error;
                r.reason = "session restarted or died during the run".into();
                if tries < 2 {
                    eprintln!("compat: session changed during the run, retrying");
                    continue;
                }
            }
            results.push(r);
            break;
        }
    }

    let mut new = Results {
        date: today(),
        build: s.getprop("ro.build.fingerprint"),
        backend: s.backend(),
        session_mem_mb: session_mem_mb(),
        apps: vec![],
    };
    // Merge: keep baseline entries for apps not run, or that hit a harness error.
    for a in &corpus.app {
        let fresh = results.iter().find(|r| r.package == a.package);
        let old = baseline.apps.iter().find(|r| r.package == a.package);
        match (fresh, old) {
            (Some(r), Some(old)) if r.result == Status::Error => new.apps.push(old.clone()),
            (Some(r), _) => new.apps.push(r.clone()),
            (None, Some(old)) => new.apps.push(old.clone()),
            (None, None) => {}
        }
    }
    new.save(&out.join("results.json"))?;
    report::write_markdown(&out.join("COMPATIBILITY.md"), &new, &corpus)?;

    let regressions = report::regressions(&baseline, &results);
    for r in &regressions {
        eprintln!("compat: REGRESSION {r}");
    }
    println!("{}", report::summary(&results));
    if regressions.is_empty() || o.accept {
        new.save(&baseline_path)?;
        report::write_markdown(&repo.join("COMPATIBILITY.md"), &new, &corpus)?;
        eprintln!("compat: wrote compat/results.json and COMPATIBILITY.md");
    } else {
        eprintln!("compat: baseline kept; results in out/compat/ (--accept to record)");
    }

    if o.finish {
        finish_with_window(&s);
    }
    Ok(if regressions.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Leaves a booted session showing its window, for whoever is watching.
fn finish_with_window(s: &Session) {
    if s.pid().is_some() && s.backend() != "headless" {
        return;
    }
    eprintln!("compat: restarting the session with its window");
    s.stop();
    if let Err(e) = s
        .start(true)
        .and_then(|_| s.wait_boot(Duration::from_secs(300)))
    {
        eprintln!("compat: could not start a windowed session: {e:#}");
    } else {
        s.prepare();
    }
}

fn today() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%d"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

/// Memory of the whole session unit (cgroup), in MiB.
fn session_mem_mb() -> Option<u64> {
    let unit = match std::env::var("LINUXAPK_INSTANCE") {
        Ok(i) if !i.is_empty() => format!("linuxapk-session-{i}"),
        _ => "linuxapk-session".to_string(),
    };
    let o = Command::new("systemctl")
        .args(["--user", "show", "-P", "MemoryCurrent", &unit])
        .output()
        .ok()?;
    let b: u64 = String::from_utf8_lossy(&o.stdout).trim().parse().ok()?;
    Some(b >> 20)
}
