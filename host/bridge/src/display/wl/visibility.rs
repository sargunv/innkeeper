//! A callback is a visibility observation only while one is outstanding.
//! Keep probing static windows too: otherwise stopping Android would prevent
//! the next callback, and a compositor without suspended could never restore it.
use std::time::{Duration, Instant};

pub(super) const PROBE_INTERVAL: Duration = Duration::from_millis(250);
const HIDDEN_AFTER: Duration = Duration::from_secs(1);

pub(super) struct Visibility {
    pub visible: bool,
    suspended: bool,
    pending: Option<Instant>,
    last_probe: Instant,
}

impl Visibility {
    pub fn new(now: Instant) -> Self {
        Self {
            visible: true,
            suspended: false,
            pending: None,
            last_probe: now,
        }
    }

    pub fn configure(&mut self, suspended: bool, now: Instant) {
        if self.suspended && !suspended {
            // Give an old outstanding callback a fresh deadline on restore.
            if self.pending.is_some() {
                self.pending = Some(now);
            }
            self.visible = true;
        }
        self.suspended = suspended;
        if suspended {
            self.visible = false;
        }
    }

    pub fn callback(&mut self) {
        self.pending = None;
        self.visible = !self.suspended;
    }

    pub fn requested(&mut self, now: Instant) {
        self.pending = Some(now);
        self.last_probe = now;
    }

    pub fn tick(&mut self, now: Instant) -> bool {
        if self
            .pending
            .is_some_and(|at| now.duration_since(at) >= HIDDEN_AFTER)
        {
            self.visible = false;
        }
        self.pending.is_none() && now.duration_since(self.last_probe) >= PROBE_INTERVAL
    }
}

/// An SDR snapshot, also used when linux-dmabuf cannot import a linear buffer.
pub(super) fn snapshot(
    fd: &std::os::fd::OwnedFd,
    spec: &super::BufferSpec,
    shm: &sctk::shm::Shm,
) -> anyhow::Result<sctk::shm::slot::Buffer> {
    use std::os::fd::AsRawFd;
    anyhow::ensure!(spec.modifier == 0, "cannot snapshot a non-linear buffer");
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
    anyhow::ensure!(
        ptr != libc::MAP_FAILED,
        "map dma-buf: {}",
        std::io::Error::last_os_error()
    );
    let result = (|| {
        let stride = spec.width as usize * 4;
        let mut pool = sctk::shm::slot::SlotPool::new(stride * spec.height as usize, shm)?;
        let (buffer, canvas) = pool.create_buffer(
            spec.width as i32,
            spec.height as i32,
            stride as i32,
            wayland_client::protocol::wl_shm::Format::Argb8888,
        )?;
        let source = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) };
        copy_pixels(source, canvas, spec);
        Ok(buffer)
    })();
    unsafe {
        libc::munmap(ptr, len);
    }
    result
}

fn copy_pixels(source: &[u8], canvas: &mut [u8], spec: &super::BufferSpec) {
    use super::super::proto::fourcc;
    let stride = spec.width as usize * 4;
    let bpp = fourcc::bytes_per_pixel(spec.fourcc).unwrap() as usize;
    for (row, dst) in canvas.chunks_exact_mut(stride).enumerate() {
        let offset = spec.offset as usize + row * spec.stride as usize;
        for (x, pixel) in dst.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let src = &source[offset + x * bpp..][..bpp];
            *pixel = match spec.fourcc {
                fourcc::ABGR16161616F => {
                    let channel = |i: usize| {
                        let h = u16::from_le_bytes([src[i * 2], src[i * 2 + 1]]);
                        let sign = if h & 0x8000 == 0 { 1.0 } else { -1.0 };
                        let exponent = (h >> 10) & 31;
                        let mantissa = (h & 1023) as f32;
                        let value = match exponent {
                            0 => mantissa * 2f32.powi(-24),
                            31 => {
                                if mantissa == 0.0 {
                                    f32::INFINITY
                                } else {
                                    0.0
                                }
                            }
                            _ => (1.0 + mantissa / 1024.0) * 2f32.powi(exponent as i32 - 15),
                        };
                        (sign * value * 255.0).round().clamp(0.0, 255.0) as u8
                    };
                    [channel(2), channel(1), channel(0), channel(3)]
                }
                fourcc::ABGR2101010 => {
                    let v = u32::from_le_bytes(src.try_into().unwrap());
                    let c = |shift: u32| (((v >> shift) & 1023u32) * 255 + 511) / 1023;
                    [c(20) as u8, c(10) as u8, c(0) as u8, ((v >> 30) * 85) as u8]
                }
                fourcc::ABGR8888 => [src[2], src[1], src[0], src[3]],
                fourcc::XBGR8888 => [src[2], src[1], src[0], 255],
                fourcc::XRGB8888 => [src[0], src[1], src[2], 255],
                _ => src.try_into().unwrap(),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_formats_downconvert_to_the_same_sdr_pixel() {
        use super::super::super::proto::{BufferSpec, fourcc};
        let mut spec = BufferSpec {
            buffer: 1,
            display: 1,
            width: 1,
            height: 1,
            fourcc: fourcc::ABGR16161616F,
            stride: 8,
            offset: 0,
            modifier: 0,
        };
        let mut pixel = [0; 4];
        // RGBA half-floats: 1, .5, 0, 1.
        copy_pixels(&[0, 0x3c, 0, 0x38, 0, 0, 0, 0x3c], &mut pixel, &spec);
        assert_eq!(pixel, [0, 128, 255, 255]);
        spec.fourcc = fourcc::ABGR2101010;
        spec.stride = 4;
        copy_pixels(
            &(1023u32 | (512 << 10) | (3 << 30)).to_le_bytes(),
            &mut pixel,
            &spec,
        );
        assert_eq!(pixel, [0, 128, 255, 255]);
        // Negative, over-range, NaN and infinity remain bounded in SDR.
        spec.fourcc = fourcc::ABGR16161616F;
        copy_pixels(&[0, 0xbc, 0, 0x40, 1, 0x7c, 0, 0x7c], &mut pixel, &spec);
        assert_eq!(pixel, [0, 255, 0, 255]);
    }

    #[test]
    fn snapshot_preserves_colour_and_alpha_without_copying_padding() {
        use super::super::super::proto::{BufferSpec, fourcc};
        let mut spec = BufferSpec {
            buffer: 1,
            display: 1,
            width: 1,
            height: 2,
            fourcc: fourcc::ABGR8888,
            stride: 8,
            offset: 4,
            modifier: 0,
        };
        let source = [
            99, 99, 99, 99, 1, 2, 3, 4, 99, 99, 99, 99, 5, 6, 7, 8, 99, 99, 99, 99,
        ];
        let mut pixels = [0; 8];
        copy_pixels(&source, &mut pixels, &spec);
        assert_eq!(pixels, [3, 2, 1, 4, 7, 6, 5, 8]);
        spec.fourcc = fourcc::XBGR8888;
        copy_pixels(&source, &mut pixels, &spec);
        assert_eq!(pixels, [3, 2, 1, 255, 7, 6, 5, 255]);
        spec.fourcc = fourcc::ARGB8888;
        copy_pixels(&source, &mut pixels, &spec);
        assert_eq!(pixels, [1, 2, 3, 4, 5, 6, 7, 8]);
        spec.fourcc = fourcc::XRGB8888;
        copy_pixels(&source, &mut pixels, &spec);
        assert_eq!(pixels, [1, 2, 3, 255, 5, 6, 7, 255]);
    }

    #[test]
    fn static_window_is_probed_and_restores_from_the_pending_callback() {
        let now = Instant::now();
        let mut v = Visibility::new(now);
        assert!(v.tick(now + HIDDEN_AFTER)); // no outstanding request cannot hide it
        assert!(v.visible);
        v.requested(now);
        assert!(!v.tick(now + PROBE_INTERVAL));
        assert!(v.visible);
        assert!(!v.tick(now + HIDDEN_AFTER));
        assert!(!v.visible);
        v.callback();
        assert!(v.visible);
        assert!(v.tick(now + HIDDEN_AFTER));
    }

    #[test]
    fn suspended_wins_over_late_callbacks_and_restore_gets_a_new_deadline() {
        let now = Instant::now();
        let mut v = Visibility::new(now);
        v.configure(true, now);
        v.callback();
        assert!(!v.visible);
        v.requested(now);
        v.configure(false, now + HIDDEN_AFTER);
        v.tick(now + HIDDEN_AFTER);
        assert!(v.visible);
        v.tick(now + HIDDEN_AFTER * 2);
        assert!(!v.visible);
        v.callback();
        assert!(v.visible);
    }
}
