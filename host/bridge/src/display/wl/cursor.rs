//! Shows a guest's hardware cursor as a Wayland cursor surface. OpenHarmony's
//! composer gives each image's hotspot. HWC3 has no style or hotspot
//! metadata, so for Android each bitmap's hotspot is learned as the pointer
//! position minus the layer origin. A hotspot is learned only on a stationary
//! frame and then cached per bitmap. Restoring a cursor uses the cached
//! hotspot and does not subtract positions, because the pointer and the layer
//! move independently.
use super::{State, Unit};
use crate::input::PointerPosition;
use rustix::fs::{MemfdFlags, SealFlags};
use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::protocol::{wl_buffer, wl_shm, wl_shm_pool};
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};

#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    /// The hotspot in the image, when the composer knows it.
    pub hotspot: Option<(i32, i32)>,
    pub pixels: Vec<u8>, // premultiplied B,G,R,A (wl_shm ARGB8888)
}

fn hotspot(position: (i32, i32), origin: (i32, i32), width: u32, height: u32) -> (i32, i32) {
    (
        (position.0 - origin.0).clamp(0, width.saturating_sub(1) as i32),
        (position.1 - origin.1).clamp(0, height.saturating_sub(1) as i32),
    )
}

/// The surface units per display pixel for a cursor, from the window's
/// units per pixel across and down. A cursor image has the density of the
/// display's frames, so it scales with them. A frame whose shape lags the
/// window's, as in a rotation, stretches across and down by different
/// amounts. The cursor keeps its shape and scales by the mean of the two.
fn cursor_units((ux, uy): (f64, f64)) -> f64 {
    (ux * uy).sqrt()
}

/// Converts an image's size and hotspot from display pixels to surface
/// units.
fn in_surface(
    (width, height): (u32, u32),
    (hx, hy): (i32, i32),
    units: f64,
) -> ((i32, i32), (i32, i32)) {
    let unit = |v: i64| (v as f64 * units).round() as i32;
    (
        (unit(width.into()).max(1), unit(height.into()).max(1)),
        (unit(hx.into()), unit(hy.into())),
    )
}

pub(super) struct Cursor {
    image: Image,
    bitmap: u64,
    hotspots: HashMap<u64, (i32, i32)>,
    hotspot: (i32, i32),
    previous: Option<(PointerPosition, (i32, i32), u64)>,
    /// The surface units per display pixel that the shown image has.
    units: Option<f64>,
}
impl Cursor {
    fn new(image: Image) -> Self {
        let mut cursor = Self {
            image: image.clone(),
            bitmap: 0,
            hotspots: HashMap::new(),
            hotspot: (0, 0),
            previous: None,
            units: None,
        };
        cursor.update(image);
        cursor
    }
    fn update(&mut self, image: Image) {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        (image.width, image.height, &image.pixels).hash(&mut hash);
        self.bitmap = hash.finish();
        self.hotspot = match image.hotspot {
            Some(h) => hotspot(h, (0, 0), image.width, image.height),
            None => self
                .hotspots
                .get(&self.bitmap)
                .copied()
                .unwrap_or(self.hotspot),
        };
        self.image = image;
    }
    fn frame(&mut self, pointer: Option<PointerPosition>) -> bool {
        let mut changed = false;
        if let Some(p) = pointer
            && self.previous == Some((p, (self.image.x, self.image.y), self.bitmap))
            && !self.image.pixels.is_empty()
            && self.image.hotspot.is_none()
            && !self.hotspots.contains_key(&self.bitmap)
        {
            self.hotspot = hotspot(
                p.position,
                (self.image.x, self.image.y),
                self.image.width,
                self.image.height,
            );
            self.hotspots.insert(self.bitmap, self.hotspot);
            changed = true;
        }
        self.previous = pointer.map(|p| (p, (self.image.x, self.image.y), self.bitmap));
        changed
    }
}

impl State {
    pub(super) fn cursor(&mut self, window: u32, image: Image) {
        match self.cursors.entry(window) {
            std::collections::hash_map::Entry::Occupied(mut e) => e.get_mut().update(image),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(Cursor::new(image));
            }
        }
        self.restore_cursor(window);
    }
    pub(super) fn cursor_frame(&mut self, window: u32) {
        let p = self
            .hub
            .pointer_positions
            .lock()
            .unwrap()
            .get(&window)
            .copied();
        if self.cursors.get_mut(&window).is_some_and(|c| c.frame(p)) {
            self.restore_cursor(window);
        }
    }
    /// Shows the cursor at the density of a new frame, whose shape may
    /// differ from the frame that the cursor was shown with.
    pub(super) fn cursor_density(&mut self, window: u32) {
        let units = cursor_units(self.display_to_surface(window, 1., 1.));
        if self
            .cursors
            .get(&window)
            .is_some_and(|c| c.units.is_some_and(|u| u != units))
        {
            self.restore_cursor(window);
        }
    }
    pub(crate) fn restore_cursor(&mut self, window: u32) {
        if self.seat.pointer_window() != Some(window) {
            return;
        }
        let units = cursor_units(self.display_to_surface(window, 1., 1.));
        let Some(cursor) = self.cursors.get_mut(&window) else {
            return;
        };
        cursor.units = Some(units);
        let cursor = &*cursor;
        let image = &cursor.image;
        if image.pixels.is_empty() {
            self.seat.set_android_cursor(window, None, 0, 0);
            return;
        }
        let ((logical_w, logical_h), (hx, hy)) =
            in_surface((image.width, image.height), cursor.hotspot, units);
        let Ok(fd) = shm_pixels(&image.pixels) else {
            return;
        };
        let pool = self.shm.wl_shm().create_pool(
            fd.as_fd(),
            image.pixels.len() as i32,
            &self.qh,
            CursorPool,
        );
        let buffer = pool.create_buffer(
            0,
            image.width as i32,
            image.height as i32,
            image.width as i32 * 4,
            wl_shm::Format::Argb8888,
            &self.qh,
            CursorBuffer,
        );
        pool.destroy();
        if self.cursor_surface.is_none() {
            let surface = self.compositor.create_surface(&self.qh);
            let viewport = self
                .viewporter
                .as_ref()
                .map(|v| v.get_viewport(&surface, &self.qh, Unit));
            self.cursor_surface = Some((surface, viewport));
        }
        let (surface, viewport) = self.cursor_surface.as_ref().unwrap();
        if let Some(v) = viewport {
            v.set_destination(logical_w, logical_h);
        }
        self.seat.set_android_cursor(window, Some(surface), hx, hy);
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, image.width as i32, image.height as i32);
        surface.commit();
    }
}
// Compositors may map a wl_shm pool with PROT_WRITE, which F_SEAL_WRITE would
// refuse, so the memfd is sealed only against resizing. This copy is private
// to Wayland, sized once, and not written by us before wl_buffer.release.
fn shm_pixels(pixels: &[u8]) -> std::io::Result<OwnedFd> {
    let fd = rustix::fs::memfd_create("cursor", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
    let mut file = std::fs::File::from(fd);
    file.write_all(pixels)?;
    rustix::fs::fcntl_add_seals(&file, SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL)?;
    Ok(file.into())
}
struct CursorPool;
impl Dispatch2<wl_shm_pool::WlShmPool, State> for CursorPool {
    fn event(
        &self,
        _: &mut State,
        _: &wl_shm_pool::WlShmPool,
        _: <wl_shm_pool::WlShmPool as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
struct CursorBuffer;
impl Dispatch2<wl_buffer::WlBuffer, State> for CursorBuffer {
    fn event(
        &self,
        _: &mut State,
        b: &wl_buffer::WlBuffer,
        _: <wl_buffer::WlBuffer as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        b.destroy();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cached_hotspot_survives_rapid_motion_and_bitmap_changes() {
        let image = Image {
            width: 32,
            height: 32,
            x: 94,
            y: 68,
            hotspot: None,
            pixels: vec![255; 32 * 32 * 4],
        };
        let mut cursor = Cursor::new(image.clone());
        let still = Some(PointerPosition {
            position: (100, 80),
            generation: 1,
        });
        assert!(!cursor.frame(still));
        assert_eq!(cursor.hotspot, (0, 0));
        assert!(cursor.frame(still));
        assert_eq!(cursor.hotspot, (6, 12));
        for n in 2..100 {
            cursor.update(Image {
                x: n * 10,
                y: n * 10,
                ..image.clone()
            });
            cursor.frame(Some(PointerPosition {
                position: (n * 20, n * 20),
                generation: n as u64,
            }));
            assert_eq!(cursor.hotspot, (6, 12));
        }
        // A new bitmap inherits the previous hotspot until a stationary frame.
        let custom = Image {
            x: 90,
            y: 70,
            pixels: vec![128; 32 * 32 * 4],
            ..image.clone()
        };
        cursor.update(custom);
        assert_eq!(cursor.hotspot, (6, 12));
        assert!(!cursor.frame(still));
        assert!(cursor.frame(still));
        assert_eq!(cursor.hotspot, (10, 10));
        cursor.update(image);
        assert_eq!(cursor.hotspot, (6, 12));
    }

    #[test]
    fn an_image_shows_at_the_frames_density() {
        // OpenHarmony's 720x1280 panel in a 360x640 window: the I-beam is
        // 55 panel pixels tall and 28 units on the host.
        let panel = cursor_units((0.5, 0.5));
        assert_eq!(in_surface((36, 55), (18, 27), panel), ((18, 28), (9, 14)));
        // Android's frame at the window's scale of 1.5.
        let scaled = cursor_units((1. / 1.5, 1. / 1.5));
        assert_eq!(in_surface((48, 48), (6, 9), scaled), ((32, 32), (4, 6)));
        // A 412x915 frame in a window that turned to 915x412 keeps the
        // cursor's shape.
        let turning = cursor_units((915. / 412., 412. / 915.));
        assert_eq!(in_surface((24, 24), (4, 3), turning), ((24, 24), (4, 3)));
        assert_eq!(in_surface((1, 1), (0, 0), 0.25), ((1, 1), (0, 0)));
    }

    #[test]
    fn a_given_hotspot_is_used_as_it_is() {
        let still = Some(PointerPosition {
            position: (100, 80),
            generation: 1,
        });
        let image = |hotspot| Image {
            width: 36,
            height: 55,
            x: 94,
            y: 68,
            hotspot: Some(hotspot),
            pixels: vec![255; 36 * 55 * 4],
        };
        let mut cursor = Cursor::new(image((0, 0)));
        assert_eq!(cursor.hotspot, (0, 0));
        // A stationary pointer elsewhere doesn't replace it.
        assert!(!cursor.frame(still));
        assert!(!cursor.frame(still));
        assert_eq!(cursor.hotspot, (0, 0));
        // An I-beam's hotspot is in its middle.
        cursor.update(image((18, 27)));
        assert_eq!(cursor.hotspot, (18, 27));
        // A hotspot outside the image is clamped into it.
        cursor.update(image((40, -3)));
        assert_eq!(cursor.hotspot, (35, 0));
    }
}
