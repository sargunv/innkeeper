//! Presentation timing for the primary Android scheduling clock.
use super::*;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU32, Ordering};
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback};
pub(super) type Presentation = wp_presentation::WpPresentation;

// A callback-only commit can supersede the previous content commit's feedback
// without replacing its pixels. Observe that commit separately for benchmarks.
pub(super) struct Probe {
    pub stamp: u64,
    pub window: u32,
}

impl Dispatch2<wp_presentation_feedback::WpPresentationFeedback, State> for Probe {
    fn event(
        &self,
        _: &mut State,
        _: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                flags,
                ..
            } => {
                let ns = ((u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo)) * 1_000_000_000
                    + u64::from(tv_nsec);
                crate::bench::trace(format_args!(
                    "probe_present frame={} window={} ns={ns} refresh_ns={refresh} flags={flags:?}",
                    self.stamp, self.window
                ));
            }
            wp_presentation_feedback::Event::Discarded => {
                crate::bench::trace(format_args!(
                    "probe_discard frame={} window={}",
                    self.stamp, self.window
                ));
            }
            _ => {}
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Sample {
    ns: u64,
    period: u32,
}

// Membership describes where a surface is mapped. Timing is the one output
// currently presenting it; an enter on another output does not replace it.
struct WindowClock {
    outputs: BTreeMap<u32, u32>,
    timing: OutputClock,
}

struct OutputClock {
    output: u32,
    mode_period: u32,
    generation: u64,
    since: u64,
    period: u32,
    phase: Option<u64>,
}

impl OutputClock {
    fn new(output: u32, period: u32, generation: u64, now: u64) -> Self {
        Self {
            output,
            mode_period: period,
            generation,
            since: now,
            period,
            phase: None,
        }
    }
}

#[derive(Default)]
pub(super) struct Clock {
    monotonic: bool,
    windows: BTreeMap<u32, WindowClock>,
    source: Option<u32>,
    generation: u64,
}

impl Clock {
    // The association boundary takes the complete visible output membership.
    // Empty means hidden/unmapped. A static visible source stays selected.
    fn window(
        &mut self,
        window: u32,
        outputs: &[(u32, u32)],
        focused: Option<u32>,
        now: u64,
    ) -> Option<Sample> {
        let outputs: BTreeMap<_, _> = outputs
            .iter()
            .copied()
            .filter(|(_, period)| (1_000_000..=100_000_000).contains(period))
            .collect();
        if outputs.is_empty() {
            self.windows.remove(&window);
            if self.source != Some(window) {
                return None;
            }
            self.source = None;
        } else {
            let selected = self.windows.get(&window).map(|w| &w.timing);
            let output = selected
                .map(|t| t.output)
                .filter(|id| outputs.contains_key(id))
                .unwrap_or_else(|| *outputs.keys().next().unwrap());
            let period = outputs[&output];
            if selected.is_some_and(|t| t.output == output && t.mode_period == period) {
                self.windows.get_mut(&window).unwrap().outputs = outputs;
                return None;
            }
            self.generation += 1;
            self.windows.insert(
                window,
                WindowClock {
                    outputs,
                    timing: OutputClock::new(output, period, self.generation, now),
                },
            );
            if self.source == Some(window) {
                return Some(self.current(now));
            }
        }
        if self.source.is_some() {
            return None;
        }
        self.source = focused
            .filter(|id| self.windows.contains_key(id))
            .or_else(|| {
                self.windows
                    .iter()
                    .max_by_key(|(_, w)| w.timing.phase.unwrap_or(0))
                    .map(|(&id, _)| id)
            });
        Some(self.current(now))
    }

    fn current(&self, now: u64) -> Sample {
        match self
            .source
            .and_then(|id| self.windows.get(&id))
            .map(|w| &w.timing)
        {
            Some(t) => Sample {
                ns: t.phase.unwrap_or(now),
                period: t.period,
            },
            None => Sample { ns: 0, period: 0 },
        }
    }

    pub(super) fn generation(&self, window: u32) -> Option<u64> {
        self.windows.get(&window).map(|w| w.timing.generation)
    }

    fn presented(
        &mut self,
        window: u32,
        generation: Option<u64>,
        output: u32,
        ns: u64,
        refresh: u32,
    ) -> Option<Sample> {
        if !self.monotonic {
            return None;
        }
        let w = self.windows.get_mut(&window)?;
        let t = &mut w.timing;
        if (generation.is_none() && ns < t.since)
            || generation.is_some_and(|g| g != t.generation)
            || t.phase.is_some_and(|last| ns <= last)
        {
            return None;
        }
        // Nested/virtual compositors may omit sync_output. Otherwise the
        // compositor chooses among the mapped outputs, including while spanning.
        let output = if output != 0 {
            output
        } else if refresh != 0 && t.mode_period.abs_diff(refresh) > 1 {
            // A reported fixed refresh can distinguish nested outputs even
            // when sync_output is absent (allow one ns of rounding).
            w.outputs
                .iter()
                .find(|(_, period)| period.abs_diff(refresh) <= 1)
                .map_or(t.output, |(&id, _)| id)
        } else {
            t.output
        };
        let mode_period = *w.outputs.get(&output)?;
        let period = if refresh == 0 { mode_period } else { refresh };
        if !(1_000_000..=100_000_000).contains(&period) {
            return None;
        }
        if output != t.output {
            self.generation += 1;
            *t = OutputClock::new(output, mode_period, self.generation, ns);
        }
        t.phase = Some(ns);
        t.period = period;
        (self.source == Some(window)).then_some(Sample { ns, period })
    }
}

impl State {
    fn send_timing(&self, sample: Sample) {
        crate::bench::trace(format_args!(
            "timing ns={} period_ns={}",
            sample.ns, sample.period
        ));
        let mut w = crate::ipc::wire::Writer::default();
        w.u64(sample.ns);
        w.u32(sample.period);
        let _ = self.events.try_send(ty::VSYNC, &w.into_vec(), &[]);
    }

    /// The single window-state integration point: reconcile complete output
    /// membership, rather than treating each enter/leave as a source change.
    pub(super) fn window_timing(&mut self, window: u32) {
        let outputs: Vec<_> = self
            .visible_outputs(window)
            .into_iter()
            .filter_map(|output| {
                let rate = self.output_rate(&output);
                if rate == 0 {
                    return None;
                }
                let period = u32::try_from(1_000_000_000_000u64 / u64::from(rate)).ok()?;
                Some((output.id().protocol_id(), period))
            })
            .collect();
        crate::bench::trace(format_args!("outputs window={window} outputs={outputs:?}"));
        if let Some(sample) = self.presentation_clock.window(
            window,
            &outputs,
            self.seat.keyboard_focus,
            crate::bench::now(),
        ) {
            self.send_timing(sample);
        }
    }
}

impl Dispatch2<Presentation, State> for Unit {
    fn event(
        &self,
        state: &mut State,
        _: &Presentation,
        event: wp_presentation::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.presentation_clock.monotonic = clk_id == libc::CLOCK_MONOTONIC as u32;
            crate::bench::trace(format_args!("clock id={clk_id}"));
        }
    }
}

pub(super) struct Frame {
    pub stamp: u64,
    pub window: u32,
    pub generation: Option<u64>,
    pub output: AtomicU32,
}

impl Dispatch2<wp_presentation_feedback::WpPresentationFeedback, State> for Frame {
    fn event(
        &self,
        state: &mut State,
        _: &wp_presentation_feedback::WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        match event {
            wp_presentation_feedback::Event::SyncOutput { output } => {
                self.output
                    .store(output.id().protocol_id(), Ordering::Relaxed);
            }
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                flags,
                ..
            } => {
                let ns = ((u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo)) * 1_000_000_000
                    + u64::from(tv_nsec);
                if let Some(sample) = state.presentation_clock.presented(
                    self.window,
                    self.generation,
                    self.output.load(Ordering::Relaxed),
                    ns,
                    refresh,
                ) {
                    state.send_timing(sample);
                }
                crate::bench::trace(format_args!(
                    "present frame={} window={} ns={ns} refresh_ns={refresh} flags={flags:?}",
                    self.stamp, self.window
                ));
            }
            wp_presentation_feedback::Event::Discarded => {
                crate::bench::trace(format_args!(
                    "discard frame={} window={}",
                    self.stamp, self.window
                ));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Clock, Sample};

    #[test]
    fn visible_static_source_keeps_phase_until_hidden_or_output_changes() {
        let mut c = Clock {
            monotonic: true,
            ..Default::default()
        };
        let t = 1_000_000_000;
        let p = 8_333_333;
        // wl_output seeds 120 Hz before the first presentation.
        assert_eq!(
            c.window(1, &[(10, p)], Some(1), t),
            Some(Sample { ns: t, period: p })
        );
        // The first commit can precede wl_surface.enter; sync_output still
        // identifies the output of that initial feedback.
        assert_eq!(c.presented(1, None, 99, t + p as u64, p), None);
        assert_eq!(
            c.presented(1, None, 10, t + p as u64, p),
            Some(Sample {
                ns: t + p as u64,
                period: p
            })
        );
        // Feedback can refine the nominal mode period without a mode change.
        assert!(
            c.presented(1, c.generation(1), 10, t + 2 * p as u64, p + 1)
                .is_some()
        );
        assert_eq!(c.window(1, &[(10, p)], Some(1), t + 1_000_000_000), None);
        assert_eq!(
            c.current(t + 2_000_000_000),
            Sample {
                ns: t + 2 * p as u64,
                period: p + 1
            }
        );
        // Other windows (including focus changes) do not displace a visible source.
        assert_eq!(c.window(2, &[(20, 16_666_666)], Some(2), t + 1), None);
        assert_eq!(
            c.presented(2, c.generation(2), 20, t + 20_000_000, 16_666_666),
            None
        );
        assert_eq!(c.source, Some(1));
        assert!(
            c.presented(1, c.generation(1), 10, t + 2_000_000_000, p)
                .is_some()
        );
        // Stale feedback cannot move the source phase backwards.
        assert_eq!(c.presented(1, None, 99, t + p as u64, p), None);
        assert_eq!(c.presented(1, None, 10, t + p as u64, p), None);
        // Hiding/unmapping the source chooses another visible window.
        assert_eq!(
            c.window(1, &[], Some(2), t + 3_000_000_000),
            Some(Sample {
                ns: t + 20_000_000,
                period: 16_666_666
            })
        );
        // Moving that source to a new output discards the old output phase.
        let old_generation = c.generation(2);
        assert_eq!(
            c.window(2, &[(30, p)], Some(2), t + 4_000_000_000),
            Some(Sample {
                ns: t + 4_000_000_000,
                period: p
            })
        );
        assert_eq!(
            c.presented(2, old_generation, 20, t + 4_100_000_000, 16_666_666),
            None
        );
        assert_eq!(
            c.window(2, &[], None, t + 5_000_000_000),
            Some(Sample { ns: 0, period: 0 })
        );
        assert_eq!(
            c.presented(2, c.generation(2), 20, t + 6_000_000_000, p),
            None
        );
    }

    #[test]
    fn source_removal_chooses_recent_visible_feedback_and_mode_changes_relock() {
        let mut c = Clock {
            monotonic: true,
            ..Default::default()
        };
        for id in 1..=3 {
            c.window(id, &[(10, 8_333_333)], None, 100);
        }
        c.presented(2, c.generation(2), 10, 200, 8_333_333);
        c.presented(3, c.generation(3), 10, 300, 8_333_333);
        assert_eq!(
            c.window(1, &[], None, 400),
            Some(Sample {
                ns: 300,
                period: 8_333_333
            })
        );
        let old = c.generation(3);
        assert_eq!(
            c.window(3, &[(10, 10_000_000)], Some(3), 500),
            Some(Sample {
                ns: 500,
                period: 10_000_000
            })
        );
        // Same output, new mode: feedback queued before the mode change is stale.
        assert_eq!(c.presented(3, old, 10, 600, 8_333_333), None);
        assert!(
            c.presented(3, c.generation(3), 10, 700, 10_000_000)
                .is_some()
        );
        assert_eq!(
            c.window(3, &[], None, 800),
            Some(Sample {
                ns: 200,
                period: 8_333_333
            })
        );
    }

    #[test]
    fn spanning_outputs_preserves_membership_and_follows_the_presented_output() {
        let mut c = Clock {
            monotonic: true,
            ..Default::default()
        };
        let (a, b) = (10, 20);
        let (pa, pb) = (8_333_333, 16_666_666);
        let t = 1_000_000_000;
        c.window(1, &[(a, pa)], Some(1), t);
        let on_a = c.generation(1);
        // Enter A, enter B, leave B: A never stopped being mapped.
        assert_eq!(c.window(1, &[(a, pa), (b, pb)], Some(1), t + 1), None);
        assert_eq!(c.generation(1), on_a);
        assert_eq!(
            c.presented(1, on_a, a, t + 10_000_000, pa),
            Some(Sample {
                ns: t + 10_000_000,
                period: pa
            })
        );
        assert_eq!(c.window(1, &[(a, pa)], Some(1), t + 20_000_000), None);
        assert_eq!(c.generation(1), on_a);
        assert_eq!(
            c.current(t + 2_000_000_000),
            Sample {
                ns: t + 10_000_000,
                period: pa
            }
        );
        // While spanning again, sync_output can select either mapped output.
        c.window(1, &[(a, pa), (b, pb)], Some(1), t + 30_000_000);
        assert_eq!(
            c.presented(1, on_a, b, t + 40_000_000, pb),
            Some(Sample {
                ns: t + 40_000_000,
                period: pb
            })
        );
        let on_b = c.generation(1);
        assert_ne!(on_a, on_b);
        assert!(c.presented(1, on_b, a, t + 45_000_000, pa).is_some());
        // A nested compositor can omit sync_output; the distinct refresh
        // still identifies B, without losing A's membership.
        assert!(
            c.presented(1, c.generation(1), 0, t + 46_000_000, pb)
                .is_some()
        );
        let on_b = c.generation(1);
        // Leaving the selected B picks the remaining A, not the timer.
        assert_eq!(
            c.window(1, &[(a, pa)], Some(1), t + 50_000_000),
            Some(Sample {
                ns: t + 50_000_000,
                period: pa
            })
        );
        assert_eq!(c.source, Some(1));
        assert_eq!(c.presented(1, on_b, b, t + 60_000_000, pb), None);
        assert!(
            c.presented(1, c.generation(1), 0, t + 70_000_000, pa)
                .is_some()
        );
    }

    #[test]
    fn only_monotonic_feedback_and_valid_periods_can_relock() {
        let mut c = Clock::default();
        c.window(1, &[(1, 8_333_333)], None, 10);
        assert_eq!(c.presented(1, c.generation(1), 1, 20, 8_333_333), None);
        c.monotonic = true;
        assert_eq!(c.presented(1, c.generation(1), 1, 20, 1), None);
        assert_eq!(
            c.presented(1, c.generation(1), 1, 20, 0),
            Some(Sample {
                ns: 20,
                period: 8_333_333
            })
        );
        assert!(c.presented(1, c.generation(1), 0, 21, 8_333_333).is_some());
        assert_eq!(c.presented(1, c.generation(1), 1, 19, 8_333_333), None);
    }
}
