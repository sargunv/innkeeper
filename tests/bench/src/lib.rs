//! Parsing and statistics for manual runs. Missing observations stay missing.
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};

pub fn default_max_load(logical_cpus: usize) -> f64 {
    (logical_cpus as f64 * 0.5).max(1.0)
}

pub fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace()
        .find_map(|s| s.strip_prefix(&format!("{key}=")))
}
pub fn number(line: &str, key: &str) -> Option<u64> {
    field(line, key)?.parse().ok()
}

/// Nearest-rank percentiles. Histogram bins are [lo, hi), in milliseconds.
pub fn stats(samples: &[f64]) -> Value {
    let mut s: Vec<_> = samples
        .iter()
        .copied()
        .filter(|x| x.is_finite() && *x >= 0.0)
        .collect();
    s.sort_by(f64::total_cmp);
    if s.is_empty() {
        return Value::Null;
    }
    let rank = |p: f64| s[((p * s.len() as f64).ceil() as usize).saturating_sub(1)];
    let edges = [
        0.0,
        4.0,
        8.0,
        12.0,
        16.0,
        20.0,
        25.0,
        33.34,
        50.0,
        100.0,
        f64::INFINITY,
    ];
    let histogram: Vec<_> = edges.windows(2).map(|b| json!({"lo_ms": b[0], "hi_ms": if b[1].is_finite() {json!(b[1])} else {Value::Null}, "count": s.iter().filter(|v| **v >= b[0] && **v < b[1]).count()})).collect();
    json!({"n":s.len(), "min_ms":s[0], "mean_ms":s.iter().sum::<f64>() / s.len() as f64, "p50_ms":rank(0.5), "p95_ms":rank(0.95), "p99_ms":rank(0.99), "max_ms":s[s.len()-1], "histogram":histogram})
}

pub fn intervals(mut times: Vec<u64>) -> Value {
    times.sort_unstable();
    stats(
        &times
            .windows(2)
            .map(|w| (w[1] - w[0]) as f64 / 1e6)
            .collect::<Vec<_>>(),
    )
}

// Visibility-only commits retain the last submitted content on that surface.
// Resolve their feedback to that content, keeping its first observed presentation.
fn presentations(trace: &str) -> HashMap<u64, u64> {
    let mut commits = HashSet::new();
    let mut current = HashMap::new();
    let mut probes = HashMap::new();
    for line in trace.lines() {
        if line.starts_with("BENCH commit ")
            && let Some(at) = number(line, "ns")
        {
            commits.insert(at);
            if let Some(window) = number(line, "window") {
                current.insert(window, at);
            }
        } else if line.starts_with("BENCH probe ")
            && let (Some(at), Some(window)) = (number(line, "ns"), number(line, "window"))
            && let Some(content) = current.get(&window)
        {
            probes.insert(at, *content);
        }
    }
    let mut result = HashMap::<u64, u64>::new();
    for line in trace.lines() {
        let (Some(frame), Some(at)) = (number(line, "frame"), number(line, "ns")) else {
            continue;
        };
        let content = if line.starts_with("BENCH present ") && commits.contains(&frame) {
            Some(frame)
        } else if line.starts_with("BENCH probe_present ") {
            probes.get(&frame).copied()
        } else {
            None
        };
        if let Some(content) = content {
            result
                .entry(content)
                .and_modify(|old| *old = (*old).min(at))
                .or_insert(at);
        }
    }
    result
}

/// A discarded request alone cannot establish that its content was displayed.
pub fn presented(trace: &str, frame: u64) -> bool {
    presentations(trace).contains_key(&frame)
}

pub fn frames(trace: &str) -> Value {
    let commits: Vec<_> = trace
        .lines()
        .filter(|l| l.starts_with("BENCH commit "))
        .filter_map(|l| number(l, "ns"))
        .collect();
    let presented = presentations(trace).into_values().collect();
    json!({"commit_intervals":intervals(commits.clone()), "presentation_intervals":intervals(presented), "discarded":trace.lines().filter(|l| l.starts_with("BENCH discard ") && number(l,"frame").is_some_and(|id| commits.contains(&id))).count()})
}

pub fn score(s: &str) -> Option<u64> {
    s.lines()
        .find_map(|l| l.split_once("glmark2 Score:")?.1.trim().parse().ok())
}

/// A new app window must be displayed before launching a pause-sensitive workload.
pub fn launched_display(reply: &str) -> Option<u64> {
    let data = reply.lines().find_map(|line| {
        line.strip_prefix("Broadcast completed: result=-1, data=\"launched ")?
            .strip_suffix('"')
    })?;
    if field(data, "shown") == Some("no") {
        return None;
    }
    number(data, "display").filter(|id| *id > 0)
}

/// Match a SPACE press to a *changed solid colour*, then its presentation id.
/// Repainting the old colour or presenting another buffer cannot satisfy it.
pub fn input(trace: &str) -> Value {
    let presentations = presentations(trace);
    let monotonic = trace.lines().any(|l| l == "BENCH clock id=1");
    let mut pending = None;
    let mut previous = None;
    let mut commit_ms = vec![];
    let mut present_ms = vec![];
    for line in trace.lines() {
        if line.starts_with("BENCH input ")
            && line.split_whitespace().skip(5).take(2).eq(["57", "1"])
        {
            pending = number(line, "ns");
        }
        if line.starts_with("BENCH commit ")
            && let (Some(color), Some(at)) = (number(line, "color"), number(line, "ns"))
            && [255, 16711680].contains(&color)
        {
            if previous.is_some_and(|old| old != color)
                && let Some(start) = pending.take()
                && at >= start
            {
                commit_ms.push((at - start) as f64 / 1e6);
                if monotonic
                    && let Some(end) = presentations.get(&at)
                    && *end >= start
                {
                    present_ms.push((end - start) as f64 / 1e6);
                }
            }
            previous = Some(color);
        }
    }
    json!({"input_to_commit":stats(&commit_ms), "input_to_presentation":stats(&present_ms), "presentation_clock_monotonic":monotonic, "physical_photon":"not measured; use a photodiode/high-speed camera"})
}

pub fn binder(log: &str) -> Value {
    let mut results = vec![];
    for workers in [1, 8] {
        let mut samples = vec![];
        let mut total = None;
        for line in log
            .lines()
            .filter(|l| number(l, "workers") == Some(workers))
        {
            if line.contains("BENCH binder_sample ")
                && let Some(ns) = field(line, "ns")
            {
                samples.extend(
                    ns.split(',')
                        .filter_map(|n| n.parse::<u64>().ok())
                        .map(|n| n as f64 / 1e6),
                );
            } else if line.contains("BENCH binder ")
                && let (Some(count), Some(elapsed)) =
                    (number(line, "count"), number(line, "elapsed_ns"))
                && elapsed > 0
            {
                total = Some((count, elapsed));
            }
        }
        if let Some((count, elapsed)) = total {
            results.push(json!({"workers":workers,"transactions":count,"elapsed_ns":elapsed,"txn_per_s":count as f64 * 1e9 / elapsed as f64,"roundtrip":stats(&samples),"samples_complete":samples.len() == count as usize}));
        }
    }
    json!(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn gpu_launch_requires_a_visible_app_display() {
        assert_eq!(
            launched_display(
                "Broadcasting: Intent { ... }\nBroadcast completed: result=-1, data=\"launched display=3 package=org.linaro.glmark2 size=1280x800 dpi=160\"\n"
            ),
            Some(3)
        );
        for reply in [
            "Broadcast completed: result=0, data=\"error: no virtual display\"",
            "Broadcast completed: result=-1, data=\"launched display=3 shown=no\"",
            "Broadcast completed: result=-1, data=\"launched display=0\"",
            "Broadcast completed: result=-1, data=\"launched display=bad\"",
        ] {
            assert_eq!(launched_display(reply), None, "{reply}");
        }
    }

    #[test]
    fn load_limit_scales_with_cpu_capacity() {
        assert_eq!(default_max_load(32), 16.0);
        assert_eq!(default_max_load(4), 2.0);
        assert_eq!(default_max_load(2), 1.0);
        assert_eq!(default_max_load(1), 1.0);
    }

    #[test]
    fn percentiles_and_histogram() {
        let s = stats(&[50.0, 1.0, 4.0, 4.0, f64::NAN, -1.0]);
        assert_eq!(s["n"], 4);
        assert_eq!(s["p50_ms"], 4.0);
        assert_eq!(s["p95_ms"], 50.0);
        assert_eq!(s["histogram"][0]["count"], 1);
        assert_eq!(s["histogram"][1]["count"], 2);
        assert!(stats(&[]).is_null());
    }
    #[test]
    fn match_changed_frame_and_clock() {
        let t = "BENCH clock id=1\nBENCH commit ns=100 window=3 color=255\nBENCH input ns=1000000 key 2 57 1\nBENCH commit ns=2000000 window=3 color=255\nBENCH commit ns=3000000 window=3 color=16711680\nBENCH present frame=3000000 window=3 ns=5000000 refresh_ns=1\n";
        assert_eq!(input(t)["input_to_commit"]["mean_ms"], 2.0);
        assert_eq!(input(t)["input_to_presentation"]["mean_ms"], 4.0);
        assert!(input(&t.replace("id=1", "id=4"))["input_to_presentation"].is_null());
    }
    #[test]
    fn visibility_commits_present_existing_content_without_adding_frames() {
        let trace = "BENCH clock id=1\n\
BENCH commit ns=100 window=3 color=255\n\
BENCH input ns=1000000 key 2 57 1\n\
BENCH commit ns=2000000 window=3 color=16711680\n\
BENCH probe ns=2100000 window=3\n\
BENCH discard frame=2000000 window=3\n\
BENCH probe ns=3100000 window=3\n\
BENCH probe_present frame=3100000 window=3 ns=4000000\n\
BENCH probe_present frame=2100000 window=3 ns=3000000\n\
BENCH input ns=5000000 key 2 57 1\n\
BENCH commit ns=6000000 window=3 color=255\n\
BENCH present frame=6000000 window=3 ns=7000000\n\
BENCH probe ns=7100000 window=3\n\
BENCH probe_present frame=7100000 window=3 ns=8000000\n";
        assert_eq!(input(trace)["input_to_presentation"]["n"], 2);
        assert!(presented(trace, 2000000));
        assert_eq!(input(trace)["input_to_presentation"]["mean_ms"], 2.0);
        assert_eq!(frames(trace)["presentation_intervals"]["n"], 1);
        assert_eq!(frames(trace)["presentation_intervals"]["mean_ms"], 4.0);
    }

    #[test]
    fn missing_feedback_and_other_windows_cannot_prove_presentation() {
        let trace = "BENCH clock id=1\n\
BENCH commit ns=100 window=3 color=255\n\
BENCH input ns=1000000 key 2 57 1\n\
BENCH commit ns=2000000 window=3 color=16711680\n\
BENCH probe ns=2100000 window=3\n\
BENCH discard frame=2000000 window=3\n\
BENCH commit ns=2200000 window=4 color=255\n\
BENCH probe ns=2300000 window=4\n\
BENCH probe_present frame=2300000 window=4 ns=3000000\n";
        assert_eq!(input(trace)["input_to_commit"]["n"], 1);
        assert!(input(trace)["input_to_presentation"].is_null());
        assert!(!presented(trace, 2000000));
    }
    #[test]
    fn discarded_frames_and_out_of_order_feedback() {
        let t = "BENCH commit ns=1\nBENCH commit ns=2\nBENCH commit ns=3\nBENCH present frame=1 ns=5000000\nBENCH discard frame=2\nBENCH present frame=3 ns=1000000\nBENCH present frame=0 ns=100000";
        assert_eq!(frames(t)["discarded"], 1);
        assert_eq!(frames(t)["presentation_intervals"]["mean_ms"], 4.0);
    }
    #[test]
    fn gpu_score_and_missing_values() {
        assert_eq!(score("noise\nI glmark2: glmark2 Score: 42\n"), Some(42));
        assert_eq!(score("glmark2 Score: unavailable"), None);
        assert_eq!(number("output_ns=-1", "output_ns"), None);
    }
    #[test]
    fn binder_output_not_shell_timing() {
        let r = binder(
            "I InnkeeperBench: BENCH binder_sample workers=8 ns=1000,3000\nI InnkeeperBench: BENCH binder workers=8 count=2 elapsed_ns=10000",
        );
        assert_eq!(r[0]["txn_per_s"], 200000.0);
        assert_eq!(r[0]["roundtrip"]["mean_ms"], 0.002);
        assert!(binder("no observation").as_array().unwrap().is_empty());
    }
}
