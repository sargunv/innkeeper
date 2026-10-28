//! `tests/compat/budgets.toml`: the session's start-time and memory budgets,
//! checked against what a run measured.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
pub struct Budgets {
    pub session_start_ms: Budget,
    pub app_start_ms: Budget,
    pub idle_memory_mib: Budget,
}

#[derive(Deserialize, Clone, Copy)]
pub struct Budget {
    pub budget: u64,
    /// How far over the budget a measurement may go before the run fails
    /// (0.25: 25%).
    pub margin: f64,
}

/// What one run measured; `None` when the run couldn't measure it.
#[derive(Serialize, Deserialize, Default, Clone, Copy)]
pub struct Measured {
    /// Cold session start: `start` to `sys.boot_completed`, with no session
    /// running and a /data that has booted before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_start_ms: Option<u64>,
    /// Warm app start: the median time to the first frame of the apps that
    /// work, each a fresh install in a running session (full corpus only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_start_ms: Option<u64>,
    /// Idle session memory: the session cgroup's memory without page cache,
    /// a while after a cold start, before any app ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_memory_mib: Option<u64>,
}

impl Measured {
    /// This run's measurements, falling back to `old`'s for what it didn't
    /// measure.
    pub fn or(self, old: Measured) -> Measured {
        Measured {
            session_start_ms: self.session_start_ms.or(old.session_start_ms),
            app_start_ms: self.app_start_ms.or(old.app_start_ms),
            idle_memory_mib: self.idle_memory_mib.or(old.idle_memory_mib),
        }
    }
}

/// One budget against one measurement.
pub struct Line {
    pub name: &'static str,
    pub unit: &'static str,
    pub budget: u64,
    pub margin: f64,
    /// The budget plus the margin: anything above fails.
    pub limit: u64,
    pub measured: Option<u64>,
}

impl Line {
    pub fn over(&self) -> bool {
        self.measured.is_some_and(|m| m > self.limit)
    }
}

impl Budgets {
    pub fn load(path: &Path) -> Result<Self> {
        let s = fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
        Ok(toml::from_str(&s)?)
    }

    pub fn lines(&self, m: &Measured) -> Vec<Line> {
        let line = |name, unit, b: Budget, measured| Line {
            name,
            unit,
            budget: b.budget,
            margin: b.margin,
            limit: (b.budget as f64 * (1.0 + b.margin)).round() as u64,
            measured,
        };
        vec![
            line(
                "cold session start",
                "ms",
                self.session_start_ms,
                m.session_start_ms,
            ),
            line("warm app start", "ms", self.app_start_ms, m.app_start_ms),
            line(
                "idle session memory",
                "MiB",
                self.idle_memory_mib,
                m.idle_memory_mib,
            ),
        ]
    }
}

/// The median of `v` (`None` if empty).
pub fn median(mut v: Vec<u64>) -> Option<u64> {
    v.sort_unstable();
    let n = v.len();
    match n {
        0 => None,
        _ if n % 2 == 1 => Some(v[n / 2]),
        _ => Some((v[n / 2 - 1] + v[n / 2]) / 2),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median_odd_even_empty() {
        assert_eq!(median(vec![3, 1, 2]), Some(2));
        assert_eq!(median(vec![4, 1, 3, 2]), Some(2));
        assert_eq!(median(vec![]), None);
    }

    #[test]
    fn over_the_margin_fails() {
        let b = |budget| Budget {
            budget,
            margin: 0.25,
        };
        let b = Budgets {
            session_start_ms: b(2000),
            app_start_ms: b(200),
            idle_memory_mib: b(600),
        };
        let m = Measured {
            session_start_ms: Some(2500),
            app_start_ms: Some(251),
            idle_memory_mib: None,
        };
        let l = b.lines(&m);
        assert!(!l[0].over(), "at the limit passes");
        assert!(l[1].over());
        assert!(!l[2].over(), "unmeasured passes");
    }
}
