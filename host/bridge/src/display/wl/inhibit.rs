//! Whether a window needs a Wayland idle inhibitor. Visibility comes from the
//! window lifecycle; Android's aggregate screen-lock policy supplies requests.
#[derive(Default)]
pub(super) struct Inhibit {
    pub requested: bool,
    active: bool,
}

impl Inhibit {
    /// Only transitions allocate or destroy a protocol object.
    pub fn update(&mut self, visible: bool, available: bool) -> Option<bool> {
        let active = self.requested && visible && available;
        if active == self.active {
            return None;
        }
        self.active = active;
        Some(active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_request_visibility_release_and_optional_protocol() {
        let mut state = Inhibit::default();
        assert_eq!(state.update(true, true), None);
        state.requested = true;
        assert_eq!(state.update(true, true), Some(true));
        assert_eq!(state.update(true, true), None);
        assert_eq!(state.update(false, true), Some(false));
        assert_eq!(state.update(false, true), None);
        assert_eq!(state.update(true, true), Some(true));
        state.requested = false;
        assert_eq!(state.update(true, true), Some(false));
        state.requested = true;
        assert_eq!(state.update(true, false), None);
        assert_eq!(state.update(true, true), Some(true));
        // Destroying a window releases its owned inhibitor; a late request
        // cannot target it because the window has left State::windows.
        assert_eq!(state.update(false, true), Some(false));
    }
}
