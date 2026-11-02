//! A display's layers (LAYERS), each shown in a subsurface of its window.
//!
//! The subsurfaces are desynchronized, so a new buffer shows as soon as its
//! subsurface commits. Their positions and stacking apply with the window
//! surface's next commit. Each has an empty input region, so input goes to
//! the window surface under it.
use std::sync::atomic::{AtomicBool, Ordering};

use sctk::compositor::Region;
use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::{wl_subsurface::WlSubsurface, wl_surface::WlSurface};
use sctk::reexports::client::{Connection, Proxy, QueueHandle, WEnum};
use sctk::shell::WaylandSurface;
use wayland_protocols::wp::color_representation::v1::client::{
    wp_color_representation_manager_v1::{self as manager, WpColorRepresentationManagerV1},
    wp_color_representation_surface_v1::{Coefficients, Range, WpColorRepresentationSurfaceV1},
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;

use super::frames::{Action, Frames};
use super::{State, Unit, pixel_ratio, visibility};
use crate::display::proto::Layer;

/// wp_color_representation_manager_v1, which tells the compositor how to
/// convert a layer's YUV to RGB. The decoder's buffers carry no colour
/// metadata, so layers are BT.601 limited range, as on the GPU path, where
/// Mesa imports them.
pub(crate) struct ColorRepresentation {
    manager: WpColorRepresentationManagerV1,
    bt601: std::sync::Arc<AtomicBool>,
}

impl ColorRepresentation {
    pub(super) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Option<Self> {
        let bt601 = std::sync::Arc::new(AtomicBool::new(false));
        let manager = globals.bind(qh, 1..=1, Supported(bt601.clone())).ok()?;
        Some(ColorRepresentation { manager, bt601 })
    }

    /// Whether the compositor takes BT.601 limited range. It says so in
    /// events that arrive before the first roundtrip ends.
    pub(super) fn bt601(&self) -> bool {
        self.bt601.load(Ordering::Relaxed)
    }
}

/// Records whether the compositor supports BT.601 limited range.
struct Supported(std::sync::Arc<AtomicBool>);
impl Dispatch2<WpColorRepresentationManagerV1, State> for Supported {
    fn event(
        &self,
        _: &mut State,
        _: &WpColorRepresentationManagerV1,
        e: manager::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let manager::Event::SupportedCoefficientsAndRanges {
            coefficients: WEnum::Value(Coefficients::Bt601),
            range: WEnum::Value(Range::Limited),
        } = e
        {
            self.0.store(true, Ordering::Relaxed);
        }
    }
}
impl Dispatch2<WpColorRepresentationSurfaceV1, State> for Unit {
    fn event(
        &self,
        _: &mut State,
        _: &WpColorRepresentationSurfaceV1,
        _: <WpColorRepresentationSurfaceV1 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

pub(super) struct Sub {
    surface: WlSurface,
    subsurface: WlSubsurface,
    viewport: WpViewport,
    layer: Layer,
    /// The buffer attached now.
    shown: Option<u32>,
    /// The buffer to attach once its import completes.
    want: Option<u32>,
    /// A host-owned copy of the buffer, attached in its place while the
    /// window is hidden or when the compositor can't import it or take its
    /// matrix.
    frozen: Option<sctk::shm::slot::Buffer>,
    /// The layer's colour representation while it shows a YUV buffer. A
    /// copy is RGB, which the BT.601 matrix doesn't apply to.
    color: Option<WpColorRepresentationSurfaceV1>,
}

impl Sub {
    /// Attaches the copy `copy` in place of the layer's buffer.
    fn attach_copy(&mut self, copy: sctk::shm::slot::Buffer) {
        if let Some(c) = self.color.take() {
            c.destroy();
        }
        self.surface.attach(Some(copy.wl_buffer()), 0, 0);
        self.frozen = Some(copy);
        self.shown = None;
    }

    fn destroy(self) {
        if let Some(c) = self.color {
            c.destroy();
        }
        self.surface.attach(None, 0, 0);
        self.surface.commit();
        self.viewport.destroy();
        self.subsurface.destroy();
        self.surface.destroy();
    }
}

/// Whether `crop` lies within a buffer of `width` by `height`.
fn crop_fits(crop: [u32; 4], width: u32, height: u32) -> bool {
    let [x, y, w, h] = crop.map(u64::from);
    x + w <= u64::from(width) && y + h <= u64::from(height)
}

/// Converts a rectangle in display pixels to the window surface's logical
/// coordinates, given the display pixels per logical unit. The edges round
/// to the nearest unit, so neighbouring rectangles stay adjacent.
fn logical(rect: [i32; 4], (rx, ry): (f64, f64)) -> [i32; 4] {
    let [x, y, w, h] = rect.map(f64::from);
    let x0 = (x / rx).round() as i32;
    let y0 = (y / ry).round() as i32;
    let x1 = ((x + w) / rx).round() as i32;
    let y1 = ((y + h) / ry).round() as i32;
    [x0, y0, (x1 - x0).max(1), (y1 - y0).max(1)]
}

impl State {
    /// Shows `layers` in window `id`, bottom to top. The first `below` go
    /// under the window's frame.
    pub(super) fn layers(&mut self, id: u32, below: u32, layers: Vec<Layer>) {
        let (Some(viewporter), Some(w)) = (self.viewporter.as_ref(), self.windows.get_mut(&id))
        else {
            // Without wp_viewporter, the bridge can't scale a layer.
            for l in &layers {
                self.release(l.buffer, false);
            }
            return;
        };
        let visible = w.configured && w.visibility.visible;
        let mut releases = Vec::new();
        while w.layers.len() > layers.len() {
            w.layers.pop().unwrap().destroy();
        }
        let restack = w.layers.len() != layers.len() || w.below != below;
        while w.layers.len() < layers.len() {
            let (subsurface, surface) = self
                .subcompositor
                .create_subsurface(w.window.wl_surface().clone(), &self.qh);
            subsurface.set_desync();
            if let Ok(empty) = Region::new(&*self.compositor) {
                surface.set_input_region(Some(empty.wl_region()));
            }
            let viewport = viewporter.get_viewport(&surface, &self.qh, Unit);
            w.layers.push(Sub {
                surface,
                subsurface,
                viewport,
                layer: layers[w.layers.len()],
                shown: None,
                want: None,
                frozen: None,
                color: None,
            });
        }
        if restack {
            let parent = w.window.wl_surface();
            let below = below as usize;
            for i in (0..below).rev() {
                let sibling = w.layers.get(i + 1).filter(|_| i + 1 < below);
                let sibling = sibling.map_or(parent, |s| &s.surface);
                w.layers[i].subsurface.place_below(sibling);
            }
            for i in below..w.layers.len() {
                let sibling = if i == below {
                    parent
                } else {
                    &w.layers[i - 1].surface
                };
                w.layers[i].subsurface.place_above(sibling);
            }
            w.below = below as u32;
        }
        let mut show = Vec::new();
        for (i, l) in layers.into_iter().enumerate() {
            let sub = &mut w.layers[i];
            let old = std::mem::replace(&mut sub.layer, l);
            let buffer = self.buffers.get_mut(&l.buffer);
            let fits = buffer
                .as_ref()
                .is_some_and(|b| crop_fits(l.crop, b.width, b.height));
            if sub.shown == Some(l.buffer) {
                // The buffer stays shown, and its crop may change.
                if fits && old.crop != l.crop {
                    set_source(&sub.viewport, l.crop);
                    sub.surface.commit();
                }
                continue;
            }
            if sub.want == Some(l.buffer) {
                continue;
            }
            let imported = buffer
                .as_ref()
                .is_some_and(|b| b.wl.is_some() || b.fallback);
            let frames = buffer.map(|b| {
                b.layer = true;
                &mut b.frames
            });
            match Frames::submit(frames, imported, visible && fits, [0; 4]) {
                Action::Release { invalid } => releases.push((l.buffer, invalid)),
                Action::Wait => sub.want = Some(l.buffer),
                Action::Present(_) => show.push((i, l.buffer)),
            }
        }
        for (buffer, invalid) in releases {
            self.release(buffer, invalid);
        }
        for (i, buffer) in show {
            self.show_layer(id, i, buffer);
        }
        self.place_layers(id);
        if let Some(w) = self.windows.get(&id) {
            w.window.commit();
        }
    }

    /// Attaches `buffer` to layer `i` of window `id`.
    fn show_layer(&mut self, id: u32, i: usize, buffer: u32) {
        let (Some(w), Some(b)) = (self.windows.get_mut(&id), self.buffers.get_mut(&buffer)) else {
            return;
        };
        let Some(sub) = w.layers.get_mut(i) else {
            return;
        };
        sub.want = None;
        let mut releases = 0;
        if b.fallback {
            match visibility::snapshot(&b.source, &b.spec, &self.shm) {
                Ok(copy) => sub.attach_copy(copy),
                Err(e) => crate::warn!("display: copy of layer buffer {buffer}: {e}"),
            }
            // The copy is shown, so the buffer is the composer's again.
            releases = b.frames.release();
        } else {
            if sub.color.is_none()
                && let Some(c) = &self.color_representation
            {
                let color = c.manager.get_surface(&sub.surface, &self.qh, Unit);
                color.set_coefficients_and_range(Coefficients::Bt601, Range::Limited);
                sub.color = Some(color);
            }
            sub.surface.attach(b.wl.as_ref(), 0, 0);
            sub.frozen = None;
            sub.shown = Some(buffer);
        }
        set_source(&sub.viewport, sub.layer.crop);
        sub.surface
            .damage_buffer(0, 0, b.width as i32, b.height as i32);
        sub.surface.commit();
        for _ in 0..releases {
            self.release(buffer, false);
        }
    }

    /// Called when the import of layer buffer `buffer` completes or fails.
    pub(super) fn layer_imported(&mut self, id: u32, buffer: u32) {
        let slot = self
            .windows
            .get(&id)
            .and_then(|w| w.layers.iter().position(|s| s.want == Some(buffer)));
        match slot {
            Some(i) => self.show_layer(id, i, buffer),
            // A later LAYERS replaced it before it showed.
            None => {
                let count = self
                    .buffers
                    .get_mut(&buffer)
                    .map_or(0, |b| b.frames.release());
                for _ in 0..count {
                    self.release(buffer, false);
                }
            }
        }
    }

    /// Positions and sizes window `id`'s layers for its size and its frame's
    /// size. The window surface's next commit applies the positions.
    pub(super) fn place_layers(&self, id: u32) {
        let Some(w) = self.windows.get(&id) else {
            return;
        };
        let Some(ratio) = pixel_ratio(w) else {
            return;
        };
        for sub in &w.layers {
            let [x, y, width, height] = logical(sub.layer.rect, ratio);
            sub.subsurface.set_position(x, y);
            sub.viewport.set_destination(width, height);
            sub.surface.commit();
        }
    }

    /// Replaces each layer's buffer with a copy, so the compositor releases
    /// the buffers while the window is hidden.
    pub(super) fn freeze_layers(&mut self, id: u32) {
        let Some(w) = self.windows.get_mut(&id) else {
            return;
        };
        for sub in &mut w.layers {
            let Some(b) = sub.shown.and_then(|b| self.buffers.get(&b)) else {
                continue;
            };
            match visibility::snapshot(&b.source, &b.spec, &self.shm) {
                Ok(copy) => {
                    sub.attach_copy(copy);
                    sub.surface
                        .damage_buffer(0, 0, b.width as i32, b.height as i32);
                    sub.surface.commit();
                }
                Err(e) => crate::warn!("display: copy of hidden layer: {e}"),
            }
        }
    }

    /// Forgets `buffer` in window layers that show or await it.
    pub(super) fn forget_layer_buffer(&mut self, buffer: u32) {
        for sub in self.windows.values_mut().flat_map(|w| &mut w.layers) {
            if sub.shown == Some(buffer) {
                sub.shown = None;
            }
            if sub.want == Some(buffer) {
                sub.want = None;
            }
        }
    }
}

/// Destroys a window's layers.
pub(super) fn destroy(layers: Vec<Sub>) {
    for sub in layers {
        sub.destroy();
    }
}

fn set_source(viewport: &WpViewport, crop: [u32; 4]) {
    let [x, y, w, h] = crop.map(f64::from);
    viewport.set_source(x, y, w, h);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crops_must_lie_within_the_buffer() {
        assert!(crop_fits([0, 0, 320, 240], 320, 240));
        assert!(crop_fits([10, 20, 310, 220], 320, 240));
        assert!(!crop_fits([10, 0, 320, 240], 320, 240));
        assert!(!crop_fits([0, 1, 320, 240], 320, 240));
        assert!(!crop_fits([u32::MAX, 0, 2, 1], 320, 240));
    }

    #[test]
    fn layers_scale_to_the_window() {
        // The probe's video on a 720x1280 panel in a 360x640 window.
        assert_eq!(
            logical([36, 558, 648, 486], (2.0, 2.0)),
            [18, 279, 324, 243]
        );
        // At 1.5 display pixels per unit, the edges round, and the far edge
        // stays where the next layer's near edge would be.
        assert_eq!(logical([1, 1, 2, 2], (1.5, 1.5)), [1, 1, 1, 1]);
        assert_eq!(logical([3, 0, 3, 3], (1.5, 1.5)), [2, 0, 2, 2]);
        // A layer smaller than a unit still shows.
        assert_eq!(logical([0, 0, 1, 1], (4.0, 4.0)), [0, 0, 1, 1]);
        // A layer partly off the display keeps its offset.
        assert_eq!(logical([-20, 0, 40, 40], (2.0, 2.0)), [-10, 0, 20, 20]);
    }
}
