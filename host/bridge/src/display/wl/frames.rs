//! FRAME ownership while a dma-buf import is pending or attached to Wayland.
#[derive(Default)]
pub(super) struct Frames {
    pending: Option<[i32; 4]>,
    busy: bool,
    owed: usize,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Action {
    Release { invalid: bool },
    Wait,
    Present([i32; 4]),
}

impl Frames {
    pub fn submit(
        frames: Option<&mut Self>,
        imported: bool,
        configured: bool,
        damage: [i32; 4],
    ) -> Action {
        let Some(frames) = frames else {
            return Action::Release { invalid: true };
        };
        if !configured {
            return Action::Release { invalid: false };
        }
        frames.owed += 1;
        if frames.busy {
            // An already attached buffer stays owned until its Wayland release.
            return Action::Wait;
        }
        frames.pending = Some(damage);
        if imported {
            Action::Present(frames.imported().unwrap())
        } else {
            Action::Wait
        }
    }

    pub fn imported(&mut self) -> Option<[i32; 4]> {
        let damage = self.pending.take()?;
        self.busy = true;
        Some(damage)
    }

    // Used for wl_buffer.release, failed imports and display/buffer teardown.
    // A failed BUFFER_ADD alone owes nothing; its subsequent FRAME is missing.
    pub fn release(&mut self) -> usize {
        self.pending = None;
        self.busy = false;
        std::mem::take(&mut self.owed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_import_resubmission_releases_and_valid_frames_keep_presenting() {
        let mut failed = Frames::default();
        assert_eq!(
            Frames::submit(Some(&mut failed), false, true, [0; 4]),
            Action::Wait
        );
        // Import failure releases the waiting FRAME exactly once.
        assert_eq!(failed.release(), 1);
        assert_eq!(failed.release(), 0);
        // The allocation's old id may be submitted again before cache eviction.
        // Both attempts must release, never consume the composer's two credits.
        for _ in 0..2 {
            assert_eq!(
                Frames::submit(None, false, true, [0; 4]),
                Action::Release { invalid: true }
            );
        }
        // Invalid RELEASE makes the producer re-import under a new id.
        let mut replacement = Frames::default();
        assert_eq!(
            Frames::submit(Some(&mut replacement), false, true, [1; 4]),
            Action::Wait
        );
        assert_eq!(replacement.imported(), Some([1; 4]));
        assert_eq!(replacement.release(), 1);
        for _ in 0..4 {
            assert_eq!(
                Frames::submit(Some(&mut replacement), true, true, [2; 4]),
                Action::Present([2; 4])
            );
            assert_eq!(replacement.release(), 1);
        }
    }

    #[test]
    fn failure_before_frame_and_duplicate_callbacks_do_not_create_releases() {
        let mut frames = Frames::default();
        assert_eq!(frames.release(), 0);
        assert_eq!(
            Frames::submit(None, false, true, [0; 4]),
            Action::Release { invalid: true }
        );
        assert_eq!(frames.imported(), None);
        assert_eq!(frames.release(), 0);
    }

    #[test]
    fn each_frame_is_released_even_while_importing_or_busy() {
        let mut frames = Frames::default();
        for _ in 0..2 {
            assert_eq!(
                Frames::submit(Some(&mut frames), false, true, [0; 4]),
                Action::Wait
            );
        }
        assert_eq!(frames.imported(), Some([0; 4]));
        assert_eq!(
            Frames::submit(Some(&mut frames), true, true, [0; 4]),
            Action::Wait
        );
        assert_eq!(frames.release(), 3);
        assert_eq!(frames.release(), 0);
        assert_eq!(
            Frames::submit(Some(&mut frames), true, false, [0; 4]),
            Action::Release { invalid: false }
        );
        assert_eq!(frames.release(), 0);
    }
}
