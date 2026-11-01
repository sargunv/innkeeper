//! A surface's preferred pixel scale, in units of 1/120.
#[derive(Clone, Copy)]
pub(super) struct Scale(u32);

impl Default for Scale {
    fn default() -> Self {
        Self(120)
    }
}

impl Scale {
    pub fn get(self) -> u32 {
        self.0
    }

    /// Returns whether content and decoration geometry need updating.
    pub fn update(&mut self, preferred: u32) -> bool {
        let next = preferred.clamp(60, 120 * 8);
        if next == self.0 {
            return false;
        }
        self.0 = next;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_moves_change_only_that_surfaces_scale() {
        let mut first = Scale::default();
        let second = Scale::default();
        assert!(!first.update(120));
        assert!(first.update(240));
        assert_eq!(first.get(), 240);
        assert_eq!(second.get(), 120);
        assert!(!first.update(240));
        assert!(first.update(150));
        assert_eq!(first.get(), 150);
        assert!(first.update(120));
    }

    #[test]
    fn content_and_decorations_share_clamped_scale() {
        let mut scale = Scale::default();
        assert!(scale.update(0));
        assert_eq!(scale.get(), 60);
        assert!(!scale.update(59));
        assert!(scale.update(u32::MAX));
        assert_eq!(scale.get(), 960);
        assert!(!scale.update(961));
    }
}
