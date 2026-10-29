//! TaskDescription colours and bounded task icons.
use crate::ipc::blob::{Doc, Image};
use crate::ipc::wire::DecodeError;
use sctk_adwaita::{
    FrameConfig,
    theme::{Color, ColorTheme},
};

use crate::input::proto::limits::MAX_ICON_SIDE;

pub fn decode_icon(bytes: &[u8]) -> Result<Option<Image>, DecodeError> {
    let mut doc = Doc::new(bytes);
    let image = doc.image(MAX_ICON_SIDE)?;
    doc.end()?;
    Ok(image)
}

/// The protocol requires a square, premultiplied ARGB8888 shm buffer. Centre
/// rectangular Android icons in transparent padding without stretching them.
pub fn icon_pixels(image: &Image) -> (i32, Vec<u8>) {
    let side = image.width.max(image.height) as usize;
    let mut pixels = vec![0; side * side * 4];
    let (x, y) = (
        (side - image.width as usize) / 2,
        (side - image.height as usize) / 2,
    );
    for (i, rgba) in image.rgba.as_chunks::<4>().0.iter().enumerate() {
        let a = rgba[3] as u32;
        let channel = |v: u8| (v as u32 * a + 127) / 255;
        let argb =
            (a << 24) | (channel(rgba[0]) << 16) | (channel(rgba[1]) << 8) | channel(rgba[2]);
        let offset = ((y + i / image.width as usize) * side + x + i % image.width as usize) * 4;
        pixels[offset..offset + 4].copy_from_slice(&argb.to_ne_bytes());
    }
    (side as i32, pixels)
}

pub fn caption_config(base: &ColorTheme, argb: u32) -> FrameConfig {
    let mut theme = base.clone();
    if argb != 0 {
        let (r, g, b) = ((argb >> 16) as u8, (argb >> 8) as u8, argb as u8);
        // Choose the higher-contrast black or white foreground in linear sRGB.
        let linear = |v: u8| {
            let v = v as f64 / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        let light = 0.2126 * linear(r) + 0.7152 * linear(g) + 0.0722 * linear(b) > 0.179;
        let foreground = if light { Color::BLACK } else { Color::WHITE };
        for map in [&mut theme.active, &mut theme.inactive] {
            // SCTK-Adwaita 0.12 copies tiny-skia's RGBA bytes directly into
            // ARGB8888 shm. Supply BGR channels for our little-endian target.
            map.headerbar = Color::from_rgba8(b, g, r, 255);
            map.font_color = foreground;
        }
    }
    FrameConfig::new(theme)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{blob::DocWriter, wire::Writer};

    #[test]
    fn icons_decode_with_a_cap_and_map_to_square_premultiplied_argb() {
        let image = Image {
            width: 1,
            height: 3,
            rgba: vec![200, 100, 50, 128, 255, 0, 0, 255, 0, 0, 0, 0],
        };
        let mut w = Writer::default();
        w.image(Some(&image));
        assert_eq!(decode_icon(&w.0).unwrap(), Some(image.clone()));
        let (side, pixels) = icon_pixels(&image);
        assert_eq!(side, 3);
        assert_eq!(&pixels[..4], &[0; 4]);
        assert_eq!(&pixels[4..8], &0x80643219u32.to_ne_bytes());
        assert_eq!(decode_icon(&[0; 4]).unwrap(), None);
        assert!(decode_icon(&w.0[..w.0.len() - 1]).is_err());
        w.0.push(0);
        assert!(decode_icon(&w.0).is_err());
        assert!(decode_icon(&[1, 1, 1, 0]).is_err()); // width 257
    }

    #[test]
    fn caption_uses_primary_rgb_and_a_readable_foreground_or_resets() {
        let base = ColorTheme::light();
        let blue = caption_config(&base, 0xff1976d2);
        // The renderer copies these channels in RGBA order to native ARGB shm.
        let pixel = blue.theme.active.headerbar.to_color_u8();
        assert_eq!(
            u32::from_ne_bytes([pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()]),
            0xff1976d2
        );
        assert_eq!(blue.theme.inactive.headerbar, blue.theme.active.headerbar);
        assert_eq!(blue.theme.active.font_color, Color::WHITE);
        assert_eq!(blue.theme.active.button_icon, base.active.button_icon);
        assert_eq!(
            caption_config(&base, 0xffffff00).theme.active.font_color,
            Color::BLACK
        );
        assert_eq!(
            caption_config(&base, 0).theme.active.headerbar,
            base.active.headerbar
        );
    }
}
