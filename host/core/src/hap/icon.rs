//! Renders a HAP's app icon on the host. The icon is a media resource, which
//! is a PNG, JPEG, WebP or SVG file, or a layered icon. A layered icon is a
//! JSON file that names a background and a foreground image. OpenHarmony
//! draws the foreground over the background, both at the full size of the
//! icon, and cuts the result with its icon mask. The mask is part of the
//! image (see [`icon_mask`]), so the caller passes it in.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use base64::Engine;
use resvg::{tiny_skia, usvg};
use serde::Deserialize;

use super::index;

/// Largest file that the renderer reads from a HAP.
const MAX_FILE: u64 = 16 << 20;

/// Where the system resources keep the mask, in their HAP.
const MASK: &str = "resources/base/media/ohos_icon_mask.png";

/// The system resources' HAP, which holds the icon mask, in OpenHarmony's
/// root.
pub const SYSTEM_RESOURCES: &str = "/system/app/ohos.global.systemres/SystemResources.hap";

/// Reads OpenHarmony's icon mask, a PNG, from the system resources' HAP.
pub fn icon_mask(system_resources: &Path) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(File::open(system_resources).ok()?).ok()?;
    entry(&mut zip, MASK)
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
struct Layered {
    layered_image: Layers,
}

#[derive(Deserialize)]
struct Layers {
    background: Option<String>,
    foreground: Option<String>,
}

fn entry(zip: &mut zip::ZipArchive<File>, path: &str) -> Option<Vec<u8>> {
    let file = zip.by_name(path).ok()?;
    let mut buf = vec![];
    file.take(MAX_FILE + 1).read_to_end(&mut buf).ok()?;
    (buf.len() as u64 <= MAX_FILE).then_some(buf)
}

/// Renders the icon whose media resource is `id` at each of `sizes`, as
/// square PNGs. `mask` is OpenHarmony's icon mask as a PNG, which cuts a
/// layered icon. Without a mask, a layered icon stays square. Returns
/// nothing if the icon can't be read.
pub fn render(
    zip: &mut zip::ZipArchive<File>,
    index: &[u8],
    id: u32,
    mask: Option<&[u8]>,
    sizes: &[u32],
) -> Vec<(u32, Vec<u8>)> {
    let Some((path, bytes)) = media(zip, index, id) else {
        return vec![];
    };
    let layers = if path.ends_with(".json") {
        let Ok(l) = serde_json::from_slice::<Layered>(&bytes) else {
            return vec![];
        };
        let mut layer = |r: Option<String>| -> Option<Option<(String, Vec<u8>)>> {
            let Some(r) = r else { return Some(None) };
            let id = r.strip_prefix("$media:")?.parse().ok()?;
            Some(Some(media(zip, index, id)?))
        };
        let (Some(bg), Some(fg)) = (
            layer(l.layered_image.background),
            layer(l.layered_image.foreground),
        ) else {
            return vec![];
        };
        if bg.is_none() && fg.is_none() {
            return vec![];
        }
        Some((bg, fg))
    } else {
        None
    };
    let mut out = vec![];
    for &size in sizes {
        let pixmap = match &layers {
            None => image(&path, &bytes, size),
            Some((bg, fg)) => layered(bg.as_ref(), fg.as_ref(), mask, size),
        };
        match pixmap.and_then(|p| p.encode_png().ok()) {
            Some(png) => out.push((size, png)),
            None => return vec![],
        }
    }
    out
}

/// The file of media resource `id`: its path in the HAP and its bytes. A
/// resource can have a value per screen density, and the densest default
/// one wins.
fn media(zip: &mut zip::ZipArchive<File>, index: &[u8], id: u32) -> Option<(String, Vec<u8>)> {
    let values = index::values(index, id);
    let best = values
        .iter()
        .filter_map(|v| Some((index::density(&v.qualifiers)?, &v.value)))
        .max_by_key(|(d, _)| *d)
        .map(|(_, v)| v)
        .or(values.first().map(|v| &v.value))?;
    // Values start with the module's name, such as
    // `entry/resources/base/media/icon.png`.
    let (_, path) = best.split_once('/')?;
    if !path.starts_with("resources/") {
        return None;
    }
    Some((path.to_string(), entry(zip, path)?))
}

fn options() -> usvg::Options<'static> {
    let mut o = usvg::Options::default();
    // An SVG in a HAP may only embed images as data URLs.
    o.image_href_resolver.resolve_string = Box::new(|_, _| None);
    o
}

/// Draws image file `path`, holding `bytes`, centred in a square of `size`.
fn image(path: &str, bytes: &[u8], size: u32) -> Option<tiny_skia::Pixmap> {
    let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
    let tree = if ext == "svg" {
        usvg::Tree::from_data(bytes, &options()).ok()?
    } else {
        let mime = match ext.as_str() {
            "png" => "png",
            "jpg" | "jpeg" => "jpeg",
            "webp" => "webp",
            "gif" => "gif",
            _ => return None,
        };
        let svg = format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="{size}" height="{size}"><image width="{size}" height="{size}" xlink:href="data:image/{mime};base64,{}"/></svg>"#,
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        usvg::Tree::from_str(&svg, &options()).ok()?
    };
    let (w, h) = (tree.size().width(), tree.size().height());
    let scale = size as f32 / w.max(h);
    let transform = tiny_skia::Transform::from_translate(
        (size as f32 - w * scale) / 2.,
        (size as f32 - h * scale) / 2.,
    )
    .pre_scale(scale, scale);
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(pixmap)
}

/// Draws a layered icon: the foreground over the background, cut by `mask`.
fn layered(
    bg: Option<&(String, Vec<u8>)>,
    fg: Option<&(String, Vec<u8>)>,
    mask: Option<&[u8]>,
    size: u32,
) -> Option<tiny_skia::Pixmap> {
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    for (path, bytes) in [bg, fg].into_iter().flatten() {
        let layer = image(path, bytes, size)?;
        pixmap.draw_pixmap(
            0,
            0,
            layer.as_ref(),
            &tiny_skia::PixmapPaint::default(),
            tiny_skia::Transform::identity(),
            None,
        );
    }
    if let Some(mask) = mask {
        let mask = image("mask.png", mask, size)?;
        pixmap.apply_mask(&tiny_skia::Mask::from_pixmap(
            mask.as_ref(),
            tiny_skia::MaskType::Alpha,
        ));
    }
    Some(pixmap)
}
