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

/// All supported dma-bufs are linear, four bytes per pixel. Copy only when
/// hiding; the rendering path keeps its zero-copy imports.
pub(super) fn snapshot(
    fd: &std::os::fd::OwnedFd,
    spec: &super::BufferSpec,
    shm: &sctk::shm::Shm,
) -> anyhow::Result<sctk::shm::slot::Buffer> {
    use std::os::fd::AsRawFd;
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
    for (row, dst) in canvas.chunks_exact_mut(stride).enumerate() {
        let offset = spec.offset as usize + row * spec.stride as usize;
        dst.copy_from_slice(&source[offset..offset + stride]);
        for pixel in dst.as_chunks_mut::<4>().0 {
            if matches!(spec.fourcc, fourcc::XBGR8888 | fourcc::ABGR8888) {
                pixel.swap(0, 2);
            }
            if matches!(spec.fourcc, fourcc::XRGB8888 | fourcc::XBGR8888) {
                pixel[3] = 255;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
