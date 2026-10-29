//! Android's hardware cursor plane is a Wayland cursor surface. HWC3 has no
//! style/hotspot metadata: subtract the plane origin from the last pointer
//! position sent to that display. Ordinary frames never include these pixels.
use super::{State, Unit};
use rustix::fs::{MemfdFlags, SealFlags};
use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::protocol::{wl_buffer, wl_shm, wl_shm_pool};
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use std::io::Write;
use std::os::fd::{AsFd, OwnedFd};

#[derive(Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub x: i32,
    pub y: i32,
    pub pixels: Vec<u8>, // premultiplied B,G,R,A (wl_shm ARGB8888)
}

fn hotspot(position: (i32, i32), origin: (i32, i32), width: u32, height: u32) -> (i32, i32) {
    (
        (position.0 - origin.0).clamp(0, width.saturating_sub(1) as i32),
        (position.1 - origin.1).clamp(0, height.saturating_sub(1) as i32),
    )
}

impl State {
    pub(super) fn cursor(&mut self, window: u32, image: Image) {
        self.cursors.insert(window, image);
        self.restore_cursor(window);
    }
    pub(crate) fn restore_cursor(&mut self, window: u32) {
        if self.seat.pointer_window() != Some(window) {
            return;
        }
        let Some(image) = self.cursors.get(&window) else {
            return;
        };
        if image.pixels.is_empty() {
            self.seat.set_android_cursor(window, None, 0, 0);
            return;
        }
        let pos = self
            .hub
            .pointer_positions
            .lock()
            .unwrap()
            .get(&window)
            .copied()
            .unwrap_or((image.x, image.y));
        let (hx, hy) = hotspot(pos, (image.x, image.y), image.width, image.height);
        let scale = self.scale120(window) as f64 / 120.;
        let logical_w = (image.width as f64 / scale).round().max(1.) as i32;
        let logical_h = (image.height as f64 / scale).round().max(1.) as i32;
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
        self.seat.set_android_cursor(
            window,
            Some(surface),
            (hx as f64 / scale).round() as i32,
            (hy as f64 / scale).round() as i32,
        );
        surface.attach(Some(&buffer), 0, 0);
        surface.damage_buffer(0, 0, image.width as i32, image.height as i32);
        surface.commit();
    }
}
// wl_shm compositors may map a pool with PROT_WRITE; F_SEAL_WRITE would
// reject that mapping. This copy is private to Wayland, sized once and immutable
// from our side until wl_buffer.release.
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
    fn plane_origin_gives_the_bitmap_hotspot() {
        assert_eq!(hotspot((100, 80), (94, 68), 32, 32), (6, 12));
        assert_eq!(hotspot((100, 80), (101, 0), 32, 32), (0, 31));
    }
}
