//! Presentation timestamps are in the compositor-advertised clock domain.
use super::*;
use wayland_protocols::wp::presentation_time::client::{wp_presentation, wp_presentation_feedback};
pub(super) type Presentation = wp_presentation::WpPresentation;

impl Dispatch2<Presentation, State> for Unit {
    fn event(
        &self,
        _: &mut State,
        _: &Presentation,
        event: wp_presentation::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            crate::bench::trace(format_args!("clock id={clk_id}"));
        }
    }
}

pub(super) struct Frame {
    pub stamp: u64,
    pub window: u32,
}

impl Dispatch2<wp_presentation_feedback::WpPresentationFeedback, State> for Frame {
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
