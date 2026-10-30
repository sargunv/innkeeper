//! Import only an advertised format/modifier pair. wl_shm ARGB8888 is the
//! mandatory baseline fallback; never relabel the source's pixel layout.
pub(super) fn direct(format: u32, modifier: u64, advertised: &[(u32, u64)]) -> bool {
    advertised.contains(&(format, modifier))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::proto::fourcc::*;

    #[test]
    fn negotiates_format_and_modifier_together() {
        let formats = [(ABGR8888, 0), (ABGR2101010, 0), (ABGR16161616F, 42)];
        assert!(direct(ABGR8888, 0, &formats));
        assert!(direct(ABGR2101010, 0, &formats));
        assert!(!direct(ABGR16161616F, 0, &formats)); // falls back to SDR shm
        assert!(direct(ABGR16161616F, 42, &formats));
        assert!(!direct(ABGR2101010, 42, &formats));
        assert!(!direct(ABGR8888, 0, &[])); // no linux-dmabuf
    }
}
