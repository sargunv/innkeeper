use crate::{parts::PartId, theme};
use std::collections::{btree_map, BTreeMap};
use tiny_skia::{Pixmap, PixmapMut, PixmapRef, Point, PremultipliedColorU8};

// Adwaita window.csd: 14px blur / 5px spread at 15%, 5px / 2px at
// 10%, and a 1px outline at 5%. CSS blur uses half its radius as sigma.
pub const SHADOW_SIZE: u32 = 43;

fn shadow(pixel_dist: f32, scale: u32, active: bool) -> f32 {
    let d = pixel_dist / scale as f32;
    // Normal tail approximation for the Gaussian blur of a straight edge.
    let tail = |x: f32| {
        let t = 1. / (1. + 0.231_641_9 * x.abs());
        let p = t
            * (0.319_381_54
                + t * (-0.356_563_78 + t * (1.781_477_9 + t * (-1.821_255_9 + t * 1.330_274_5))));
        let p = (-x * x / 2.).exp() * 0.398_942_3 * p;
        if x >= 0. {
            p
        } else {
            1. - p
        }
    };
    let outline = if d < 1. { 0.05 } else { 0. };
    if active {
        1. - (1. - 0.15 * tail((d - 5.) / 7.)) * (1. - 0.10 * tail((d - 2.) / 2.5)) * (1. - outline)
    } else {
        1. - (1. - 0.08 * tail((d - 5.) / 5.)) * (1. - outline)
    }
}

#[derive(Debug)]
struct RenderedShadow {
    side: Pixmap,
    edges: Pixmap,
}

impl RenderedShadow {
    fn new(scale: u32, active: bool) -> Option<RenderedShadow> {
        let shadow_size = SHADOW_SIZE * scale;
        let corner_radius = theme::CORNER_RADIUS * scale;

        let mut side = Pixmap::new(shadow_size, 1)?;
        for x in 0..side.width() as usize {
            let shadow = shadow(x as f32 + 0.5, scale, active);
            let alpha = (shadow * u8::MAX as f32).round() as u8;

            let Some(color) = PremultipliedColorU8::from_rgba(0, 0, 0, alpha) else {
                continue;
            };

            if let Some(pixel) = side.pixels_mut().get_mut(x) {
                *pixel = color;
            }
        }

        let edges_size = (corner_radius + shadow_size) * 2;
        let mut edges = Pixmap::new(edges_size, edges_size)?;
        let edges_middle = Point::from_xy(edges_size as f32 / 2.0, edges_size as f32 / 2.0);
        for y in 0..edges_size as usize {
            let y_pos = y as f32 + 0.5;
            for x in 0..edges_size as usize {
                let dist = edges_middle.distance(Point::from_xy(x as f32 + 0.5, y_pos))
                    - corner_radius as f32;

                let shadow = shadow(dist, scale, active);
                let alpha = (shadow * u8::MAX as f32).round() as u8;

                let Some(color) = PremultipliedColorU8::from_rgba(0, 0, 0, alpha) else {
                    continue;
                };

                if let Some(pixel) = edges.pixels_mut().get_mut(y * edges_size as usize + x) {
                    *pixel = color;
                }
            }
        }

        Some(RenderedShadow { side, edges })
    }

    fn side_draw(
        &self,
        flipped: bool,
        rotated: bool,
        stack: usize,
        dst_pixmap: &mut PixmapMut,
        dst_left: usize,
        dst_top: usize,
    ) {
        fn iter_copy<'a>(
            src: impl Iterator<Item = &'a PremultipliedColorU8>,
            dst: impl Iterator<Item = &'a mut PremultipliedColorU8>,
        ) {
            src.zip(dst).for_each(|(src, dst)| *dst = *src)
        }

        let dst_width = dst_pixmap.width() as usize;
        let dst_pixels = dst_pixmap.pixels_mut();
        match (flipped, rotated) {
            (false, false) => (0..stack).for_each(|i| {
                let dst = dst_pixels
                    .iter_mut()
                    .skip((dst_top + i) * dst_width + dst_left);
                iter_copy(self.side.pixels().iter(), dst);
            }),
            (false, true) => (0..stack).for_each(|i| {
                let dst = dst_pixels
                    .iter_mut()
                    .skip(dst_top * dst_width + dst_left + i)
                    .step_by(dst_width);
                iter_copy(self.side.pixels().iter(), dst);
            }),
            (true, false) => (0..stack).for_each(|i| {
                let dst = dst_pixels
                    .iter_mut()
                    .skip((dst_top + i) * dst_width + dst_left);
                iter_copy(self.side.pixels().iter().rev(), dst);
            }),
            (true, true) => (0..stack).for_each(|i| {
                let dst = dst_pixels
                    .iter_mut()
                    .skip(dst_top * dst_width + dst_left + i)
                    .step_by(dst_width);
                iter_copy(self.side.pixels().iter().rev(), dst);
            }),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn edges_draw(
        &self,
        src_x_offset: isize,
        src_y_offset: isize,
        dst_pixmap: &mut PixmapMut,
        dst_rect_left: usize,
        dst_rect_top: usize,
        dst_rect_width: usize,
        dst_rect_height: usize,
    ) {
        let src_width = self.edges.width() as usize;
        let src_pixels = self.edges.pixels();
        let dst_width = dst_pixmap.width() as usize;
        let dst_pixels = dst_pixmap.pixels_mut();
        for y in 0..dst_rect_height {
            let dst_y = dst_rect_top + y;
            let src_y = y as isize + src_y_offset;
            if src_y < 0 {
                continue;
            }

            let src_y = src_y as usize;
            for x in 0..dst_rect_width {
                let dst_x = dst_rect_left + x;
                let src_x = x as isize + src_x_offset;
                if src_x < 0 {
                    continue;
                }

                let src = src_pixels.get(src_y * src_width + src_x as usize);
                let dst = dst_pixels.get_mut(dst_y * dst_width + dst_x);
                if let (Some(src), Some(dst)) = (src, dst) {
                    *dst = *src;
                }
            }
        }
    }

    fn draw(&self, dst_pixmap: &mut PixmapMut, scale: u32, part_idx: PartId, hide_border: bool) {
        let shadow_size = (SHADOW_SIZE * scale) as usize;
        let border_size = (theme::border_size(hide_border) * scale) as usize;
        let corner_radius = (theme::CORNER_RADIUS * scale) as usize;

        debug_assert!(corner_radius > border_size);
        if corner_radius <= border_size {
            return;
        }

        let dst_width = dst_pixmap.width() as usize;
        let dst_height = dst_pixmap.height() as usize;
        let edges_half = self.edges.width() as usize / 2;
        match part_idx {
            PartId::Top => {
                let left_edge_width = edges_half;
                let right_edge_width = edges_half;
                let side_width = dst_width
                    .saturating_sub(left_edge_width)
                    .saturating_sub(right_edge_width);

                self.edges_draw(
                    0,
                    -(border_size as isize),
                    dst_pixmap,
                    0,
                    0,
                    left_edge_width,
                    dst_height,
                );

                self.side_draw(
                    true,
                    true,
                    side_width,
                    dst_pixmap,
                    left_edge_width,
                    border_size,
                );

                self.edges_draw(
                    edges_half as isize,
                    -(border_size as isize),
                    dst_pixmap,
                    left_edge_width + side_width,
                    0,
                    right_edge_width,
                    dst_height,
                );
            }
            PartId::Left => {
                let top_edge_height = corner_radius;
                let bottom_edge_height = corner_radius - border_size;
                let side_height = dst_height
                    .saturating_sub(top_edge_height)
                    .saturating_sub(bottom_edge_height);

                self.edges_draw(
                    0,
                    shadow_size as isize,
                    dst_pixmap,
                    0,
                    0,
                    dst_width.saturating_sub(border_size),
                    top_edge_height,
                );

                self.side_draw(true, false, side_height, dst_pixmap, 0, top_edge_height);

                self.edges_draw(
                    0,
                    edges_half as isize,
                    dst_pixmap,
                    0,
                    top_edge_height + side_height,
                    dst_width.saturating_sub(border_size),
                    bottom_edge_height,
                );
            }
            PartId::Right => {
                let top_edge_height = corner_radius;
                let bottom_edge_height = corner_radius - border_size;
                let side_height = dst_height
                    .saturating_sub(top_edge_height)
                    .saturating_sub(bottom_edge_height);

                self.edges_draw(
                    edges_half as isize + corner_radius as isize,
                    shadow_size as isize,
                    dst_pixmap,
                    border_size,
                    0,
                    dst_width.saturating_sub(border_size),
                    top_edge_height,
                );

                self.side_draw(
                    false,
                    false,
                    side_height,
                    dst_pixmap,
                    border_size,
                    top_edge_height,
                );

                self.edges_draw(
                    edges_half as isize + corner_radius as isize,
                    edges_half as isize,
                    dst_pixmap,
                    border_size,
                    top_edge_height + side_height,
                    dst_width.saturating_sub(border_size),
                    bottom_edge_height,
                );
            }
            PartId::Bottom => {
                let left_edge_width = edges_half;
                let right_edge_width = edges_half;
                let side_width = dst_width
                    .saturating_sub(left_edge_width)
                    .saturating_sub(right_edge_width);

                self.edges_draw(
                    0,
                    edges_half as isize + (corner_radius - border_size) as isize,
                    dst_pixmap,
                    0,
                    0,
                    left_edge_width,
                    dst_height,
                );

                self.side_draw(
                    false,
                    true,
                    side_width,
                    dst_pixmap,
                    left_edge_width,
                    border_size,
                );

                self.edges_draw(
                    edges_half as isize,
                    edges_half as isize + (corner_radius - border_size) as isize,
                    dst_pixmap,
                    left_edge_width + side_width,
                    0,
                    right_edge_width,
                    dst_height,
                );
            }
            PartId::Header => {
                self.edges_draw(
                    shadow_size as isize,
                    shadow_size as isize,
                    dst_pixmap,
                    0,
                    0,
                    corner_radius,
                    corner_radius,
                );

                self.edges_draw(
                    edges_half as isize,
                    shadow_size as isize,
                    dst_pixmap,
                    dst_width.saturating_sub(corner_radius),
                    0,
                    corner_radius,
                    corner_radius,
                );
            }
        }
    }
}

#[derive(Debug)]
struct CachedPart {
    pixmap: Pixmap,
    scale: u32,
    active: bool,
    hide_border: bool,
}

impl CachedPart {
    fn matches(
        &self,
        dst_pixmap: &PixmapRef,
        dst_scale: u32,
        dst_active: bool,
        hide_border: bool,
    ) -> bool {
        self.pixmap.width() == dst_pixmap.width()
            && self.pixmap.height() == dst_pixmap.height()
            && self.scale == dst_scale
            && self.active == dst_active
            && self.hide_border == hide_border
    }
}

#[derive(Default, Debug)]
pub struct Shadow {
    part_cache: [Option<CachedPart>; PartId::COUNT],
    // (scale, active) -> RenderedShadow
    rendered: BTreeMap<(u32, bool), RenderedShadow>,
}

impl Shadow {
    pub fn draw(
        &mut self,
        pixmap: &mut PixmapMut,
        scale: u32,
        active: bool,
        part_idx: PartId,
        hide_border: bool,
    ) {
        let Some(cache) = self.part_cache.get_mut(part_idx as usize) else {
            return;
        };

        if let Some(cache_value) = cache {
            if !cache_value.matches(&pixmap.as_ref(), scale, active, hide_border) {
                *cache = None;
            }
        }

        if cache.is_none() {
            let rendered = match self.rendered.entry((scale, active)) {
                btree_map::Entry::Occupied(entry) => entry.into_mut(),
                btree_map::Entry::Vacant(entry) => match RenderedShadow::new(scale, active) {
                    Some(v) => entry.insert(v),
                    None => return,
                },
            };

            let Some(mut pixmap) = Pixmap::new(pixmap.width(), pixmap.height()) else {
                return;
            };

            rendered.draw(&mut pixmap.as_mut(), scale, part_idx, hide_border);

            *cache = Some(CachedPart {
                pixmap,
                scale,
                active,
                hide_border,
            })
        }

        if let Some(cache) = cache.as_ref() {
            let src_data = cache.pixmap.data();
            if let Some(pixmap) = pixmap.data_mut().get_mut(..src_data.len()) {
                pixmap.copy_from_slice(src_data);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adwaita_shadow_fades_and_scales_in_logical_pixels() {
        for active in [false, true] {
            let values: Vec<_> = (0..=43).map(|x| shadow(x as f32, 1, active)).collect();
            assert!(values.iter().all(|v| (0. ..=1.).contains(v)));
            assert!(values
                .iter()
                .zip(values.iter().skip(1))
                .all(|(a, b)| a >= b));
            assert!(shadow(43., 1, active) < 0.001);
            assert_eq!(shadow(10., 1, active), shadow(20., 2, active));
        }
        assert!(shadow(5., 1, true) > shadow(5., 1, false));
    }
}
