use anyhow::{Context, Result, bail, ensure};
use innkeeper_bench::{default_max_load, frames, input, number, presented, trace};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn command(program: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("timeout")
        .args(["--kill-after=2", "45", program])
        .args(args)
        .output()
        .with_context(|| program.to_owned())?;
    ensure!(
        out.status.success(),
        "{program} {args:?} exited {}: {}{}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
fn session(args: &[&str]) -> Result<String> {
    command("tools/dev/session.sh", args)
}
fn android(args: &[&str]) -> Result<String> {
    let mut a = vec!["exec"];
    a.extend_from_slice(args);
    session(&a)
}
fn read(p: impl AsRef<Path>) -> String {
    fs::read_to_string(p).unwrap_or_default()
}
fn context() -> Value {
    let cpu = read("/proc/cpuinfo")
        .lines()
        .find_map(|l| l.strip_prefix("model name\t: "))
        .unwrap_or("unknown")
        .to_owned();
    let mut governors = vec![];
    if let Ok(entries) = fs::read_dir("/sys/devices/system/cpu/cpufreq") {
        for e in entries.flatten() {
            let g = read(e.path().join("scaling_governor")).trim().to_owned();
            if !g.is_empty() && !governors.contains(&g) {
                governors.push(g);
            }
        }
    }
    let sessions: Vec<_> = fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let pid = e.file_name().to_string_lossy().parse::<u32>().ok()?;
            let cmd = fs::read(e.path().join("cmdline")).ok()?;
            let first = String::from_utf8_lossy(cmd.split(|b| *b == 0).next()?);
            first.ends_with("/innkeeper-brk").then_some(pid)
        })
        .collect();
    json!({"git_sha":command("git",&["rev-parse","HEAD"]).unwrap_or_default().trim(),"git_dirty":!command("git",&["status","--porcelain"]).unwrap_or_default().is_empty(),"cpu":cpu,"logical_cpus":std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1),"loadavg":read("/proc/loadavg").trim(),"governors":governors,"kernel":command("uname",&["-sr"]).unwrap_or_default().trim(),"host_desktop":std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),"compositor":std::env::var("INNKEEPER_TEST_COMPOSITOR").unwrap_or("kwin".into()),"other_session_broker_pids":sessions,"memory":read("/proc/meminfo").lines().take(3).collect::<Vec<_>>()})
}
fn load(context: &Value) -> f64 {
    context["loadavg"]
        .as_str()
        .unwrap_or("")
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(f64::INFINITY)
}
fn wait(mut f: impl FnMut() -> bool, seconds: u64) -> Result<()> {
    let end = Instant::now() + Duration::from_secs(seconds);
    while !f() {
        ensure!(Instant::now() < end, "observation deadline expired");
        sleep(Duration::from_millis(100));
    }
    Ok(())
}
fn log() -> String {
    read(PathBuf::from(std::env::var("INNKEEPER_BENCH_OUT").unwrap()).join("probe-log.txt"))
}

fn launch(mode: &str) -> Result<()> {
    android(&["am", "force-stop", "dev.sargunv.innkeeper.probes"])?;
    android(&[
        "am",
        "start",
        "-W",
        "-n",
        "dev.sargunv.innkeeper.probes/.BenchProbe",
        "--es",
        "mode",
        mode,
    ])?;
    wait(|| log().contains(&format!("BENCH ready mode={mode}")), 20)
}
fn bridge_log() -> String {
    let profile = std::env::var("INNKEEPER_PROFILE").unwrap();
    read(
        PathBuf::from(std::env::var("XDG_STATE_HOME").unwrap())
            .join(format!("innkeeper-{profile}/bridge.log")),
    )
}
/// The bridge's trace events.
fn bridge() -> String {
    trace(&bridge_log())
}
fn keeper() -> Result<std::process::Child> {
    Ok(Command::new("tools/dev/session.sh")
        .args([
            "exec",
            "sh",
            "-c",
            "echo ready; while true; do sleep 1; done",
        ])
        .stdout(Stdio::null())
        .spawn()?)
}
struct Cleanup {
    out: PathBuf,
    keeper: Option<std::process::Child>,
    logger: Option<std::process::Child>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = fs::write(
            self.out.join("sf.txt"),
            android(&["dumpsys", "SurfaceFlinger"]).unwrap_or_default(),
        );
        let _ = fs::write(self.out.join("bridge-log.txt"), bridge_log());
        let _ = fs::write(
            self.out.join("android-errors.txt"),
            android(&[
                "logcat",
                "-d",
                "-t",
                "1000",
                "-s",
                "InnkeeperBench:I",
                "AndroidRuntime:E",
                "ActivityTaskManager:I",
                "SurfaceFlinger:V",
                "BufferQueueProducer:V",
                "VirtualDisplaySurface:V",
                "BLASTBufferQueue:W",
            ])
            .unwrap_or_else(|e| format!("{e:#}")),
        );
        let _ = session(&["stop"]);
        if let Some(logger) = &mut self.logger {
            let _ = logger.kill();
            let _ = logger.wait();
        }
        if let Some(k) = &mut self.keeper {
            let _ = k.kill();
            let _ = k.wait();
        }
    }
}
fn traces(out: &Path, mode: &str, seconds: u64) -> Result<Value> {
    launch(mode)?;
    let start = bridge().len();
    sleep(Duration::from_secs(seconds));
    let text = bridge();
    let text = text.get(start..).unwrap_or("");
    fs::write(out.join(format!("{mode}-bridge.txt")), text)?;
    let result = frames(text);
    ensure!(
        !result["commit_intervals"].is_null(),
        "no committed frames for {mode}"
    );
    Ok(result)
}
fn solid_frame(trace: &str) -> Option<(u64, u64)> {
    let line = trace.lines().rev().find(|l| l.starts_with("commit "))?;
    let color = number(line, "color")?;
    [255, 16711680]
        .contains(&color)
        .then_some((number(line, "ns")?, color))
}
fn install(apk: &Path) -> Result<()> {
    let file = fs::File::open(apk)?;
    let size = file.metadata()?.len().to_string();
    let status = Command::new("timeout")
        .args([
            "45",
            "tools/dev/session.sh",
            "exec",
            "cmd",
            "package",
            "install",
            "-r",
            "-g",
            "-S",
            &size,
        ])
        .stdin(file)
        .status()?;
    ensure!(status.success(), "APK install failed");
    Ok(())
}
fn collect(name: &str, out: &Path, seconds: u64) -> Result<Value> {
    let mut cleanup = Cleanup {
        out: out.to_owned(),
        keeper: None,
        logger: None,
    };
    session(&["start"])?;
    cleanup.keeper = Some(keeper()?);
    wait(
        || android(&["getprop", "sys.boot_completed"]).is_ok_and(|v| v.trim() == "1"),
        30,
    )?;
    session(&["selftest"])?;
    android(&["setprop", "vendor.innkeeper.hwc.bench", "1"])?;
    cleanup.logger = Some(
        Command::new("timeout")
            .args([
                "--kill-after=2",
                "240",
                "tools/dev/session.sh",
                "exec",
                "logcat",
                "-T",
                "1",
                "-v",
                "raw",
                "-s",
                "InnkeeperBench:I",
                "innkeeper-hwc:I",
                "AndroidRuntime:E",
            ])
            .stdout(fs::File::create(out.join("probe-log.txt"))?)
            .stderr(fs::File::create(out.join("probe-log-errors.txt"))?)
            .spawn()?,
    );
    install(Path::new("out/test/probes/InnkeeperProbes.apk"))?;
    for setting in [
        "window_animation_scale",
        "transition_animation_scale",
        "animator_duration_scale",
    ] {
        android(&["settings", "put", "global", setting, "0"])?;
    }
    let result = match name {
        "input" => {
            launch("input")?;
            wait(|| solid_frame(&bridge()).is_some(), 10)?;
            // A submitted first frame precedes InputDispatcher publishing the
            // moved activity as its target. Exclude that launch/focus wait.
            wait(
                || {
                    android(&["dumpsys", "input"]).is_ok_and(|dump| {
                        dump.split("FocusedWindows:")
                            .nth(1)
                            .and_then(|s| s.split("FocusRequests:").next())
                            .is_some_and(|s| s.contains("BenchProbe"))
                    })
                },
                10,
            )?;
            // Wait for the changed solid frame. A transition or a repaint
            // doesn't count.
            for _ in 0..10 {
                let before = solid_frame(&bridge()).context("missing initial colour")?;
                command(
                    "/usr/bin/python3",
                    &["tests/e2e/seat.py", "key:57,1", "key:57,0"],
                )?;
                wait(
                    || solid_frame(&bridge()).is_some_and(|f| f.0 > before.0 && f.1 != before.1),
                    5,
                )?;
                let frame = solid_frame(&bridge()).unwrap().0;
                wait(
                    || {
                        let b = bridge();
                        b.contains("capabilities presentation=0") || presented(&b, frame)
                    },
                    5,
                )?;
            }
            let b = bridge();
            fs::write(out.join("input-bridge.txt"), &b)?;
            let result = input(&b);
            ensure!(
                result["input_to_commit"]["n"] == 10,
                "no matching colour changes; check probe focus"
            );
            Ok(result)
        }
        "frame" => {
            Ok(json!({"scroll":traces(out,"scroll",seconds)?,"game":traces(out,"game",seconds)?}))
        }
        _ => bail!("unknown benchmark"),
    };
    fs::write(
        out.join("gfxinfo.txt"),
        android(&[
            "dumpsys",
            "gfxinfo",
            "dev.sargunv.innkeeper.probes",
            "framestats",
        ])
        .unwrap_or_default(),
    )?;
    result
}
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let name = args.first().map(String::as_str).unwrap_or("help");
    if !["input", "frame"].contains(&name) {
        println!(
            "mise run bench -- input|frame [--noisy] [--seconds N] [--max-load N]\nFresh named profile, private desktop and data. Outputs: out/bench/<profile>/result.json and summary.txt. --noisy marks proof runs unsuitable for comparison."
        );
        return Ok(());
    }
    let value = |key: &str, default: &str| {
        args.iter()
            .position(|a| a == key)
            .and_then(|i| args.get(i + 1))
            .cloned()
            .unwrap_or(default.to_owned())
    };
    let seconds = value("--seconds", "5").parse::<u64>()?;
    ensure!((1..=60).contains(&seconds), "seconds must be 1..60");
    let logical_cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let default_load = default_max_load(logical_cpus).to_string();
    let max_load = value("--max-load", &default_load).parse::<f64>()?;
    ensure!(
        max_load.is_finite() && max_load > 0.0,
        "max load must be positive"
    );
    let noisy = args.iter().any(|a| a == "--noisy");
    if std::env::var("INNKEEPER_BENCH_INSIDE").is_ok() {
        let out = PathBuf::from(std::env::var("INNKEEPER_BENCH_OUT")?);
        let mut r: Value = serde_json::from_str(&read(out.join("result.json")))?;
        let collected = collect(name, &out, seconds);
        let after = context();
        if load(&after) > max_load {
            r["warnings"]
                .as_array_mut()
                .unwrap()
                .push(json!("load exceeded threshold after measurement"));
        }
        if after["other_session_broker_pids"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
        {
            r["warnings"]
                .as_array_mut()
                .unwrap()
                .push(json!("other innkeeper sessions present after measurement"));
        }
        r["context_after"] = after;
        let profile = std::env::var("INNKEEPER_PROFILE")?;
        let compositor_info = read(
            Path::new("out/test/nested")
                .join(profile)
                .join("kwin-support.txt"),
        );
        fs::write(out.join("compositor.txt"), &compositor_info)?;
        match collected {
            Ok(v) => {
                r["measurement"] = v;
                r["status"] = json!("collected");
            }
            Err(e) => {
                r["status"] = json!("failed");
                r["error"] = json!(format!("{e:#}"));
            }
        }
        r["noisy"] = json!(noisy || !r["warnings"].as_array().unwrap().is_empty());
        fs::write(out.join("result.json"), serde_json::to_string_pretty(&r)?)?;
        let summary = format!(
            "Benchmark: {name}\nStatus: {}\nNOISY / proof only: {}\nContext: {}\nWarnings: {}\nMeasurement: {}\nError: {}\nResult: {}\n",
            r["status"],
            r["noisy"],
            r["context_before"],
            r["warnings"],
            serde_json::to_string_pretty(&r["measurement"])?,
            r["error"],
            out.join("result.json").display()
        );
        fs::write(out.join("summary.txt"), &summary)?;
        print!("{summary}");
        ensure!(
            r["status"] == "collected",
            "collection failed; result file retained"
        );
        return Ok(());
    }
    let before = context();
    println!(
        "Machine context: {}",
        serde_json::to_string_pretty(&before)?
    );
    let mut warnings = vec![];
    if noisy {
        warnings.push("explicit noisy proof run; do not publish these numbers");
    }
    if load(&before) > max_load {
        warnings.push("load average exceeds threshold");
    }
    if before["other_session_broker_pids"]
        .as_array()
        .is_some_and(|a| !a.is_empty())
    {
        warnings.push("other innkeeper sessions are running");
    }
    if !warnings.is_empty() {
        eprintln!("*** BENCHMARK WARNING: {} ***", warnings.join("; "));
        ensure!(
            noisy,
            "machine is not quiet; retry later, or use --noisy for harness proof only"
        );
    }
    print!("{}", command("free", &["-g"])?);
    let id = std::env::var("INNKEEPER_PROFILE").unwrap_or_else(|_| {
        format!(
            "bench-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis()
        )
    });
    ensure!(
        id.starts_with("bench-")
            && id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
        "profile must be named bench-<name>"
    );
    let out = std::env::current_dir()?.join("out/bench").join(&id);
    ensure!(
        !out.exists() && !Path::new("out/dev/flatpak").join(&id).exists(),
        "benchmark profile must be fresh"
    );
    fs::create_dir_all(&out)?;
    fs::write(
        out.join("result.json"),
        serde_json::to_string_pretty(
            &json!({"benchmark":name,"profile":id,"status":"starting","context_before":before,"warnings":warnings,"noisy":noisy,"max_load":max_load}),
        )?,
    )?;
    let status = Command::new("tests/nested.sh")
        .arg(std::env::current_exe()?)
        .args(&args)
        .env("INNKEEPER_PROFILE", &id)
        .env("INNKEEPER_BRIDGE_DEBUG", "1")
        .env("INNKEEPER_BENCH_INSIDE", "1")
        .env("INNKEEPER_BENCH_OUT", &out)
        .env(
            "INNKEEPER_DATA",
            std::env::current_dir()?
                .join("out/dev/flatpak")
                .join(&id)
                .join("android-data"),
        )
        .status()?;
    if !status.success() {
        let mut r: Value = serde_json::from_str(&read(out.join("result.json")))?;
        if r["status"] == "starting" {
            r["status"] = json!("failed");
            r["error"] =
                json!("nested desktop failed before collection; inspect out/test/nested logs");
            fs::write(out.join("result.json"), serde_json::to_string_pretty(&r)?)?;
            fs::write(
                out.join("summary.txt"),
                format!(
                    "Benchmark {name} failed before collection. See result.json and out/test/nested/{id}/ logs.\n"
                ),
            )?;
        }
        bail!(
            "benchmark failed; see {} and nested desktop logs",
            out.display()
        );
    }
    Ok(())
}
