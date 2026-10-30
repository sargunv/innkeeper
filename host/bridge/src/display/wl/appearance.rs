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

const APPEARANCE: &str = "org.freedesktop.appearance";
const WM: &str = "org.gnome.desktop.wm.preferences";

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Appearance {
    dark: bool,
    accent: Option<[f64; 3]>,
    layout: Option<String>,
}

impl Appearance {
    fn apply(&mut self, ns: &str, key: &str, value: &zbus::zvariant::Value<'_>) {
        use zbus::zvariant::Value;
        if let Value::Value(inner) = value {
            return self.apply(ns, key, inner);
        }
        match (ns, key) {
            (APPEARANCE, "color-scheme") => {
                if let Ok(value) = u32::try_from(value) {
                    self.dark = value == 1;
                }
            }
            (APPEARANCE, "accent-color") => {
                self.accent = value
                    .try_clone()
                    .ok()
                    .and_then(|v| <(f64, f64, f64)>::try_from(v).ok())
                    .map(|(r, g, b)| [r, g, b])
                    .filter(|rgb| rgb.iter().all(|v| v.is_finite() && (0. ..=1.).contains(v)));
            }
            (WM, "button-layout") => {
                if let Ok(layout) = <&str>::try_from(value) {
                    self.layout = Some(layout.into());
                }
            }
            _ => (),
        }
    }

    pub fn config(&self, argb: u32) -> FrameConfig {
        let mut theme = if self.dark {
            ColorTheme::dark()
        } else {
            ColorTheme::light()
        };
        let accent = self.accent.unwrap_or([0.208, 0.518, 0.894]);
        for map in [&mut theme.active, &mut theme.inactive] {
            let mut bg = rgb(map.headerbar);
            if argb != 0 {
                let caption = [
                    ((argb >> 16) & 255) as f64 / 255.,
                    ((argb >> 8) & 255) as f64 / 255.,
                    (argb & 255) as f64 / 255.,
                ];
                bg = caption_tint(bg, caption);
            }
            let foreground = if contrast(bg, [0.; 3]) > contrast(bg, [1.; 3]) {
                [0.; 3]
            } else {
                [1.; 3]
            };
            let state = |amount| {
                let candidate = mix(bg, accent, amount);
                if contrast(candidate, foreground) >= 4.5 {
                    color(candidate)
                } else {
                    color(bg)
                }
            };
            map.headerbar = color(bg);
            map.font_color = color(foreground);
            map.button_icon = color(foreground);
            map.button_idle = color(mix(bg, foreground, 0.08));
            map.button_hover = state(0.16);
            map.button_pressed = state(0.28);
        }
        let mut config = FrameConfig::new(theme);
        config.button_layout = self.layout.clone();
        config
    }
}

// tiny-skia's RGBA bytes are copied into native ARGB8888 shm on little-endian hosts.
fn color(rgb: [f64; 3]) -> Color {
    Color::from_rgba8(
        (rgb[2] * 255.).round() as u8,
        (rgb[1] * 255.).round() as u8,
        (rgb[0] * 255.).round() as u8,
        255,
    )
}
fn rgb(color: Color) -> [f64; 3] {
    [
        color.blue() as f64,
        color.green() as f64,
        color.red() as f64,
    ]
}
fn mix(a: [f64; 3], b: [f64; 3], amount: f64) -> [f64; 3] {
    std::array::from_fn(|i| a[i] * (1. - amount) + b[i] * amount)
}
fn caption_tint(base: [f64; 3], caption: [f64; 3]) -> [f64; 3] {
    // Keep every sRGB channel within three 8-bit levels of Adwaita. Bounding
    // channels keeps both lightness and saturation subtle, even for opposite
    // schemes or saturated captions; scaling the blend preserves the hue.
    let distance = base
        .into_iter()
        .zip(caption)
        .map(|(a, b)| (a - b).abs())
        .fold(0., f64::max);
    mix(base, caption, 0.18_f64.min((3. / 255.) / distance))
}
fn luminance(rgb: [f64; 3]) -> f64 {
    let linear = |v: f64| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2])
}
fn contrast(a: [f64; 3], b: [f64; 3]) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// One portal subscription per bridge process, shared by composer connections.
/// Read on a worker so an unavailable portal never blocks Wayland dispatch.
pub fn subscribe() -> calloop::channel::Channel<Appearance> {
    use std::sync::{Arc, Mutex, OnceLock};
    type Subscribers = (Appearance, Vec<calloop::channel::Sender<Appearance>>);
    static SETTINGS: OnceLock<Arc<Mutex<Subscribers>>> = OnceLock::new();
    let shared = SETTINGS.get_or_init(|| {
        let shared: Arc<Mutex<Subscribers>> =
            Arc::new(Mutex::new((Appearance::default(), Vec::new())));
        let worker = shared.clone();
        std::thread::spawn(move || {
            let watch = || -> zbus::Result<()> {
                use std::collections::HashMap;
                use zbus::zvariant::OwnedValue;
                let conn = zbus::blocking::Connection::session()?;
                let proxy = zbus::blocking::Proxy::new(
                    &conn,
                    "org.freedesktop.portal.Desktop",
                    "/org/freedesktop/portal/desktop",
                    "org.freedesktop.portal.Settings",
                )?;
                let changes = proxy.receive_signal("SettingChanged")?;
                let all: HashMap<String, HashMap<String, OwnedValue>> =
                    proxy.call("ReadAll", &(vec![APPEARANCE, WM],))?;
                let publish = |updates: Vec<(String, String, OwnedValue)>| {
                    let mut shared = worker.lock().unwrap();
                    let (current, subscribers) = &mut *shared;
                    let before = current.clone();
                    for (ns, key, value) in updates {
                        current.apply(&ns, &key, &value);
                    }
                    if *current != before {
                        subscribers.retain(|s| s.send(current.clone()).is_ok());
                    }
                };
                publish(
                    all.into_iter()
                        .flat_map(|(ns, values)| {
                            values
                                .into_iter()
                                .map(move |(key, value)| (ns.clone(), key, value))
                        })
                        .collect(),
                );
                for msg in changes {
                    if let Ok(update) = msg.body().deserialize::<(String, String, OwnedValue)>() {
                        publish(vec![update]);
                    }
                }
                Ok(())
            };
            if let Err(e) = watch() {
                crate::warn!("frame: portal settings: {e}");
            }
        });
        shared
    });
    let (tx, rx) = calloop::channel::channel();
    let mut shared = shared.lock().unwrap();
    let _ = tx.send(shared.0.clone());
    shared.1.push(tx);
    rx
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
    fn caption_tint_stays_close_to_adwaita_in_both_schemes() {
        for dark in [false, true] {
            let appearance = Appearance {
                dark,
                ..Appearance::default()
            };
            let base = appearance.config(0).theme;
            // Dark toolbar on light, light toolbar on dark, and saturated hues.
            for caption in [
                0xff1c1b1f, 0xff000000, 0xffffffff, 0xffff0000, 0xff00ff00, 0xff0000ff, 0xffffff00,
                0xffff00ff,
            ] {
                let tinted = appearance.config(caption).theme;
                for (base, tinted) in [
                    (base.active.headerbar, tinted.active.headerbar),
                    (base.inactive.headerbar, tinted.inactive.headerbar),
                ] {
                    let (base, tinted) = (rgb(base), rgb(tinted));
                    for (base, tinted) in base.into_iter().zip(tinted) {
                        assert!(
                            (base - tinted).abs() <= 3. / 255. + 1e-7,
                            "dark={dark} caption={caption:x}: {base} -> {tinted}"
                        );
                    }
                    assert_ne!(base, tinted, "retain a subtle caption tint");
                    if caption == 0xffff0000 {
                        assert!(tinted[0] > tinted[1] && tinted[0] > tinted[2]);
                    }
                }
            }
        }
    }

    #[test]
    fn theme_tints_and_all_button_states_keep_text_readable() {
        for dark in [false, true] {
            for caption in [
                0, 0xff1976d2, 0xffffff00, 0xff000000, 0xffffffff, 0xff808080,
            ] {
                for accent in [
                    None,
                    Some([0., 0., 0.]),
                    Some([1., 1., 1.]),
                    Some([1., 0., 0.]),
                    Some([0., 0., 1.]),
                ] {
                    let a = Appearance {
                        dark,
                        accent,
                        layout: None,
                    };
                    let config = a.config(caption);
                    for map in [config.theme.active, config.theme.inactive] {
                        for bg in [
                            map.headerbar,
                            map.button_idle,
                            map.button_hover,
                            map.button_pressed,
                        ] {
                            assert!(
                                contrast(rgb(map.font_color), rgb(bg)) >= 4.5,
                                "{a:?} {caption:x}"
                            );
                        }
                        assert_eq!(map.font_color, map.button_icon);
                    }
                }
            }
        }
        assert_eq!(color([1., 0., 0.]).to_color_u8().blue(), 255);
        assert_eq!(contrast([0.; 3], [1.; 3]), 21.);
    }

    #[test]
    fn typed_portal_settings_accept_variants_and_reject_malformed_accents() {
        use zbus::zvariant::Value;
        let mut a = Appearance::default();
        a.apply(
            APPEARANCE,
            "color-scheme",
            &Value::Value(Box::new(Value::from(1u32))),
        );
        assert!(a.dark);
        a.apply(APPEARANCE, "color-scheme", &Value::from("dark"));
        assert!(a.dark);
        a.apply(APPEARANCE, "color-scheme", &Value::from(99u32));
        assert!(!a.dark);
        a.apply(WM, "button-layout", &Value::from("close:"));
        assert_eq!(a.config(0).button_layout.as_deref(), Some("close:"));
        a.apply(APPEARANCE, "accent-color", &Value::from((0.2, 0.5, 0.8)));
        assert_eq!(a.accent, Some([0.2, 0.5, 0.8]));
        for value in [
            Value::from((f64::NAN, 0., 0.)),
            Value::from((0., 1.1, 0.)),
            Value::from((0., -0.1, 0.)),
            Value::from("red"),
        ] {
            a.apply(APPEARANCE, "accent-color", &value);
            assert_eq!(a.accent, None);
        }
        let before = a.clone();
        a.apply("unknown", "color-scheme", &Value::from(1u32));
        assert_eq!(a, before);
    }
}
