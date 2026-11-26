//! A dialog window's crop (WINDOW_DIALOG), trimmed to what the dialog draws.
//!
//! A dialog's window frame can include transparent insets around its
//! background, such as Material dialogs' 24 dp sides and 80 dp top and
//! bottom. Android's dim layer or nothing fills them, so they show as a dark
//! or see-through margin. When the crop's whole border is one translucent
//! colour, the crop shrinks to the pixels that differ from it. An opaque
//! border is the dialog's own background, which stays.
use super::super::proto::{BufferSpec, fourcc};
use wayland_client::protocol::{wl_shm, wl_subsurface, wl_surface};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

/// Larger channel differences count as content, which keeps the edges of
/// shadows.
const TOLERANCE: u8 = 8;

/// Trims `crop` in the frame in `fd`, or returns it unchanged if the frame
/// can't be read.
pub(super) fn trim(fd: &std::os::fd::OwnedFd, spec: &BufferSpec, crop: [u32; 4]) -> [u32; 4] {
    use std::os::fd::AsRawFd;
    if spec.modifier != 0 || !matches!(spec.fourcc, fourcc::ABGR8888 | fourcc::ARGB8888) {
        return crop;
    }
    let len = spec.bytes() as usize;
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            fd.as_raw_fd(),
            0,
        )
    };
    if ptr == libc::MAP_FAILED {
        return crop;
    }
    let source = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
    let trimmed = trim_pixels(source, spec, crop);
    unsafe {
        libc::munmap(ptr, len);
    }
    trimmed
}

/// `source` holds 4-byte pixels with alpha last, `spec.stride` bytes per row
/// from `spec.offset`.
fn trim_pixels(source: &[u8], spec: &BufferSpec, crop: [u32; 4]) -> [u32; 4] {
    let [x, y, w, h] = crop;
    if w == 0 || h == 0 || x + w > spec.width || y + h > spec.height {
        return crop;
    }
    let pixel = |px: u32, py: u32| -> Option<[u8; 4]> {
        let at = spec.offset as usize + py as usize * spec.stride as usize + px as usize * 4;
        source.get(at..at + 4).map(|p| [p[0], p[1], p[2], p[3]])
    };
    let Some(margin) = pixel(x, y) else {
        return crop;
    };
    if margin[3] == 255 {
        return crop;
    }
    let differs =
        |px, py| pixel(px, py).is_none_or(|p| (0..4).any(|i| p[i].abs_diff(margin[i]) > TOLERANCE));
    let border = (x..x + w)
        .flat_map(|px| [(px, y), (px, y + h - 1)])
        .chain((y..y + h).flat_map(|py| [(x, py), (x + w - 1, py)]));
    if border.into_iter().any(|(px, py)| differs(px, py)) {
        return crop;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    for py in y..y + h {
        for px in x..x + w {
            if differs(px, py) {
                x0 = x0.min(px);
                y0 = y0.min(py);
                x1 = x1.max(px + 1);
                y1 = y1.max(py + 1);
            }
        }
    }
    if x0 == u32::MAX {
        return crop;
    }
    [x0, y0, x1 - x0, y1 - y0]
}

/// A dim layer under a translucent activity that fills its window, so the
/// desktop doesn't show through the activity's transparent pixels.
pub(super) struct Backdrop {
    surface: wl_surface::WlSurface,
    subsurface: wl_subsurface::WlSubsurface,
    viewport: WpViewport,
    _buffer: sctk::shm::slot::Buffer,
    _pool: sctk::shm::slot::SlotPool,
}

/// Android's own dim for dialogs, premultiplied black at 60 %.
const BACKDROP: [u8; 4] = [0, 0, 0, 153];

impl Backdrop {
    pub(super) fn new(
        st: &super::State,
        parent: &wl_surface::WlSurface,
        size: (u32, u32),
    ) -> Option<Backdrop> {
        let viewporter = st.viewporter.as_ref()?;
        let mut pool = sctk::shm::slot::SlotPool::new(4, &st.shm).ok()?;
        let (buffer, canvas) = pool.create_buffer(1, 1, 4, wl_shm::Format::Argb8888).ok()?;
        canvas[..4].copy_from_slice(&BACKDROP);
        let (subsurface, surface) = st.subcompositor.create_subsurface(parent.clone(), &st.qh);
        subsurface.set_desync();
        subsurface.place_below(parent);
        if let Ok(empty) = sctk::compositor::Region::new(&*st.compositor) {
            surface.set_input_region(Some(empty.wl_region()));
        }
        let viewport = viewporter.get_viewport(&surface, &st.qh, super::Unit);
        buffer.attach_to(&surface).ok()?;
        surface.damage_buffer(0, 0, 1, 1);
        let b = Backdrop {
            surface,
            subsurface,
            viewport,
            _buffer: buffer,
            _pool: pool,
        };
        b.resize(size);
        Some(b)
    }

    pub(super) fn resize(&self, (w, h): (u32, u32)) {
        self.viewport
            .set_destination(w.max(1) as i32, h.max(1) as i32);
        self.surface.commit();
    }

    pub(super) fn destroy(self) {
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIM: [u8; 4] = [0, 0, 0, 153];
    const CARD: [u8; 4] = [230, 225, 240, 255];

    fn frame(w: u32, h: u32, card: Option<[u32; 4]>) -> (Vec<u8>, BufferSpec) {
        let spec = BufferSpec {
            buffer: 1,
            display: 1,
            width: w,
            height: h,
            fourcc: fourcc::ABGR8888,
            stride: w * 4 + 16,
            offset: 0,
            modifier: 0,
        };
        let mut px = vec![0u8; (spec.stride * h) as usize];
        for y in 0..h {
            for x in 0..w {
                let inside = card.is_some_and(|[cx, cy, cw, ch]| {
                    x >= cx && x < cx + cw && y >= cy && y < cy + ch
                });
                let at = (y * spec.stride + x * 4) as usize;
                px[at..at + 4].copy_from_slice(if inside { &CARD } else { &DIM });
            }
        }
        (px, spec)
    }

    #[test]
    fn a_dim_margin_around_the_card_is_trimmed() {
        // PackageInstaller: a 576x391 frame with 24 and 80 px insets.
        let (px, spec) = frame(1280, 800, Some([376, 284, 528, 231]));
        assert_eq!(
            trim_pixels(&px, &spec, [352, 204, 576, 391]),
            [376, 284, 528, 231]
        );
    }

    #[test]
    fn an_opaque_or_mixed_border_stays() {
        // The card fills the frame.
        let (px, spec) = frame(1280, 800, Some([352, 204, 576, 391]));
        assert_eq!(
            trim_pixels(&px, &spec, [352, 204, 576, 391]),
            [352, 204, 576, 391]
        );
        // The card reaches one edge of the crop.
        let (px, spec) = frame(1280, 800, Some([352, 284, 528, 231]));
        assert_eq!(
            trim_pixels(&px, &spec, [352, 204, 576, 391]),
            [352, 204, 576, 391]
        );
    }

    #[test]
    fn nothing_but_margin_or_an_unreadable_crop_stays() {
        let (px, spec) = frame(64, 64, None);
        assert_eq!(trim_pixels(&px, &spec, [8, 8, 32, 32]), [8, 8, 32, 32]);
        assert_eq!(trim_pixels(&px, &spec, [40, 40, 32, 32]), [40, 40, 32, 32]);
        assert_eq!(
            trim_pixels(&px[..100], &spec, [8, 8, 32, 32]),
            [8, 8, 32, 32]
        );
    }
}
