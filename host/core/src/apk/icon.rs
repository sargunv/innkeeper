//! Static adaptive drawable previews. Android's paths use SVG path syntax;
//! resvg supplies antialiasing, clipping and bitmap composition.
use std::fs::File;
use std::io::Read;
use std::path::Path;

use super::{Res, Table, Value, xml};
use base64::Engine;

/// A host-side preview, including adaptive-only launcher icons. Failure leaves
/// the caller free to use its generic icon; Android remains the final renderer.
pub fn render(apk: &Path, icons: &[(u32, String)]) -> Option<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(File::open(apk).ok()?).ok()?;
    let table = entry(&mut zip, "resources.arsc");
    let res = Res(table.as_deref().and_then(|b| Table::parse(b).ok()));
    let mut renderer = Renderer { zip, res };
    // Prefer an adaptive preview, then the largest legacy bitmap.
    for (_, path) in icons.iter().filter(|(_, p)| p.ends_with(".xml")) {
        if let Some(svg) = renderer.file(path, 0) {
            let svg = format!(
                r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" width="192" height="192" viewBox="0 0 108 108">{svg}</svg>"#
            );
            if let Ok(tree) = resvg::usvg::Tree::from_str(&svg, &Default::default()) {
                let mut pixmap = resvg::tiny_skia::Pixmap::new(192, 192)?;
                resvg::render(
                    &tree,
                    resvg::tiny_skia::Transform::identity(),
                    &mut pixmap.as_mut(),
                );
                return pixmap.encode_png().ok();
            }
        }
    }
    let mut bitmaps: Vec<_> = icons
        .iter()
        .filter(|(_, p)| p.ends_with(".png") || p.ends_with(".webp"))
        .collect();
    bitmaps.sort_by_key(|(density, _)| std::cmp::Reverse(*density));
    bitmaps
        .into_iter()
        .find_map(|(_, p)| entry(&mut renderer.zip, p))
}

fn entry(zip: &mut zip::ZipArchive<File>, path: &str) -> Option<Vec<u8>> {
    let file = zip.by_name(path).ok()?;
    let mut buf = vec![];
    file.take((16 << 20) + 1).read_to_end(&mut buf).ok()?;
    (buf.len() <= 16 << 20).then_some(buf)
}

struct Renderer<'a> {
    zip: zip::ZipArchive<File>,
    res: Res<'a>,
}

impl Renderer<'_> {
    fn drawable(&mut self, value: &Value, depth: usize) -> Option<String> {
        if depth > 8 {
            return None;
        }
        match self.res.value(value) {
            Value::Color(c) => Some(format!(
                r#"<rect width="108" height="108" fill="{}" fill-opacity="{}"/>"#,
                color(c),
                alpha(c)
            )),
            Value::Str(p) => self.file(&p, depth + 1),
            _ => None,
        }
    }

    fn file(&mut self, path: &str, depth: usize) -> Option<String> {
        if depth > 8 {
            return None;
        }
        let bytes = entry(&mut self.zip, path)?;
        if path.ends_with(".png") || path.ends_with(".webp") {
            let mime = if path.ends_with(".png") {
                "png"
            } else {
                "webp"
            };
            return Some(format!(
                r#"<image width="108" height="108" preserveAspectRatio="none" xlink:href="data:image/{mime};base64,{}"/>"#,
                base64::engine::general_purpose::STANDARD.encode(bytes)
            ));
        }
        let root = xml::parse(&bytes).ok()?;
        match root.name.as_str() {
            "adaptive-icon" => {
                let mut layers = String::new();
                for name in ["background", "foreground"] {
                    let layer = root.find(name).next()?;
                    layers.push_str(&self.drawable(layer.attr("drawable")?, depth + 1)?);
                }
                // The 108dp layers include an 18dp overscan on every edge.
                Some(format!(
                    r#"<svg viewBox="18 18 72 72" width="108" height="108"><defs><clipPath id="mask"><circle cx="54" cy="54" r="36"/></clipPath></defs><g clip-path="url(#mask)">{layers}</g></svg>"#
                ))
            }
            "vector" => {
                // Theme colours/gradients are not statically resolvable. Refuse
                // them rather than silently producing a misleading preview.
                if root.attr("tint").is_some() {
                    return None;
                }
                let w = self.number(&root, "viewportWidth", 0.)?;
                let h = self.number(&root, "viewportHeight", 0.)?;
                if w <= 0. || h <= 0. {
                    return None;
                }
                let body = self.children(&root)?;
                Some(format!(
                    r#"<svg viewBox="0 0 {w} {h}" width="108" height="108" opacity="{}">{body}</svg>"#,
                    self.number(&root, "alpha", 1.)?
                ))
            }
            _ => None,
        }
    }

    fn number(&self, e: &xml::Element, name: &str, default: f32) -> Option<f32> {
        let Some(v) = e.attr(name) else {
            return Some(default);
        };
        let n = match self.res.value(v) {
            Value::Float(bits) => f32::from_bits(bits),
            Value::Int(i) => i as i32 as f32,
            Value::Str(s) => s.parse().ok()?,
            _ => return None,
        };
        n.is_finite().then_some(n)
    }

    fn paint(&self, e: &xml::Element, name: &str) -> Option<(String, f32)> {
        match e.attr(name).map(|v| self.res.value(v)) {
            None => Some(("none".into(), 1.)),
            Some(Value::Color(c)) => Some((color(c), alpha(c))),
            _ => None,
        }
    }

    fn children(&self, root: &xml::Element) -> Option<String> {
        let mut out = String::new();
        for e in &root.children {
            match e.name.as_str() {
                "group" => {
                    let px = self.number(e, "pivotX", 0.)?;
                    let py = self.number(e, "pivotY", 0.)?;
                    out.push_str(&format!(r#"<g transform="translate({} {}) rotate({} {px} {py}) translate({px} {py}) scale({} {}) translate({} {})">{}</g>"#,
                        self.number(e,"translateX",0.)?,self.number(e,"translateY",0.)?,self.number(e,"rotation",0.)?,self.number(e,"scaleX",1.)?,self.number(e,"scaleY",1.)?,-px,-py,self.children(e)?));
                }
                "path" => {
                    if !e.children.is_empty()
                        || ["trimPathStart", "trimPathEnd", "trimPathOffset"]
                            .iter()
                            .any(|n| e.attr(n).is_some())
                    {
                        return None;
                    }
                    let data = self.res.text(e.attr("pathData")?)?;
                    let (fill, fa) = self.paint(e, "fillColor")?;
                    let (stroke, sa) = self.paint(e, "strokeColor")?;
                    let rule = if self.number(e, "fillType", 0.)? == 1. {
                        "evenodd"
                    } else {
                        "nonzero"
                    };
                    let cap = match self.number(e, "strokeLineCap", 0.)? as i32 {
                        0 => "butt",
                        1 => "round",
                        2 => "square",
                        _ => return None,
                    };
                    let join = match self.number(e, "strokeLineJoin", 0.)? as i32 {
                        0 => "miter",
                        1 => "round",
                        2 => "bevel",
                        _ => return None,
                    };
                    out.push_str(&format!(r#"<path d="{}" fill="{fill}" fill-opacity="{}" fill-rule="{rule}" stroke="{stroke}" stroke-opacity="{}" stroke-width="{}" stroke-linecap="{cap}" stroke-linejoin="{join}" stroke-miterlimit="{}"/>"#,
                        escape(&data),fa*self.number(e,"fillAlpha",1.)?,sa*self.number(e,"strokeAlpha",1.)?,self.number(e,"strokeWidth",0.)?,self.number(e,"strokeMiterLimit",4.)?));
                }
                _ => return None,
            }
        }
        Some(out)
    }
}

fn color(c: u32) -> String {
    format!("#{:06x}", c & 0xffffff)
}
fn alpha(c: u32) -> f32 {
    (c >> 24) as f32 / 255.
}
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

#[cfg(test)]
mod tests {
    use super::super::{
        table::build::*,
        xml::build::{Node, V, document},
    };
    use super::*;
    use std::io::Write;

    #[test]
    fn adaptive_only_apk_renders_layers_and_mask_without_android() {
        let dir = crate::test_dir("adaptive-icon");
        let apk = dir.join("fixture.apk");
        let mut zip = zip::ZipWriter::new(File::create(&apk).unwrap());
        let adaptive = document(&Node(
            "adaptive-icon",
            vec![],
            vec![
                Node("background", vec![("drawable", V::R(0x7f010001))], vec![]),
                Node("foreground", vec![("drawable", V::R(0x7f010002))], vec![]),
            ],
        ));
        let vector = document(&Node(
            "vector",
            vec![("viewportWidth", V::I(108)), ("viewportHeight", V::I(108))],
            vec![Node(
                "path",
                vec![
                    ("fillColor", V::R(0x7f010003)),
                    ("pathData", V::S("M42,42h24v24h-24z")),
                ],
                vec![],
            )],
        ));
        // A separate resource supplies the foreground colour.
        let table = super::super::table::build::table(
            &["res/icon.xml", "res/front.xml"],
            0x7f,
            &[type_chunk(
                1,
                &config("", "", 0, 0),
                &[
                    (0, 3, 0),
                    (1, 0x1c, 0xff204080),
                    (2, 3, 1),
                    (3, 0x1c, 0xffff8000),
                ],
                Layout::Dense,
            )],
        );
        for (name, bytes) in [
            ("resources.arsc", table),
            ("res/icon.xml", adaptive),
            ("res/front.xml", vector),
        ] {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(&bytes).unwrap();
        }
        zip.finish().unwrap();
        let png = render(&apk, &[(65534, "res/icon.xml".into())]).unwrap();
        let pixels = resvg::tiny_skia::Pixmap::decode_png(&png).unwrap();
        assert_eq!(pixels.pixel(0, 0).unwrap().alpha(), 0);
        let center = pixels.pixel(96, 96).unwrap();
        assert_eq!((center.red(), center.green(), center.blue()), (255, 128, 0));
        let background = pixels.pixel(96, 25).unwrap();
        assert_eq!(
            (background.red(), background.green(), background.blue()),
            (32, 64, 128)
        );
        assert!(render(&apk, &[(0, "res/missing.xml".into())]).is_none());
    }
}
