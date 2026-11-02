//! A dma-buf is imported directly only when the compositor advertises its
//! exact format and modifier pair. Otherwise the frame falls back to wl_shm
//! ARGB8888, which every compositor must support. The source's pixel layout is
//! never relabelled as another format.
//!
//! An NV12 layer also needs the compositor to take its BT.601 limited-range
//! matrix through `wp_color_representation_v1`. A compositor without it picks
//! its own matrix and shows wrong colours, so the bridge converts the layer to
//! RGB on the CPU.
use crate::display::proto::fourcc::NV12;

pub(super) fn direct(format: u32, modifier: u64, advertised: &[(u32, u64)], bt601: bool) -> bool {
    advertised.contains(&(format, modifier)) && (format != NV12 || bt601)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::proto::fourcc::*;

    #[test]
    fn negotiates_format_and_modifier_together() {
        let formats = [(ABGR8888, 0), (ABGR2101010, 0), (ABGR16161616F, 42)];
        assert!(direct(ABGR8888, 0, &formats, false));
        assert!(direct(ABGR2101010, 0, &formats, false));
        assert!(!direct(ABGR16161616F, 0, &formats, false)); // falls back to SDR shm
        assert!(direct(ABGR16161616F, 42, &formats, false));
        assert!(!direct(ABGR2101010, 42, &formats, false));
        assert!(!direct(ABGR8888, 0, &[], false)); // no linux-dmabuf
    }

    #[test]
    fn nv12_imports_when_the_compositor_takes_bt601() {
        let formats = [(NV12, 0)];
        assert!(direct(NV12, 0, &formats, true));
        // Without the matrix, the bridge converts the layer on the CPU.
        assert!(!direct(NV12, 0, &formats, false));
        assert!(!direct(NV12, 0, &[], true));
    }
}
