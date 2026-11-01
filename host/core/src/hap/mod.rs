//! HAP metadata for OpenHarmony profiles. A HAP is a zip whose `module.json`
//! describes the bundle and one module. Labels such as `$string:app_name`
//! and icons such as `$media:icon` resolve through `resources.index`, which
//! OpenHarmony's restool writes ([`index`]). No OpenHarmony SDK or session is
//! needed. Everything read is untrusted, so front ends validate it before
//! use.

pub mod icon;
mod index;

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

const MAX_MODULE_JSON: u64 = 4 << 20;
const MAX_INDEX: u64 = 64 << 20;

/// What an install needs from one HAP.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HapInfo {
    /// `app.bundleName`.
    pub bundle: String,
    /// `module.name`, such as `entry`.
    pub module: String,
    pub version_code: String,
    pub version_name: String,
    /// The app's label in the default locale, or empty if the HAP has none
    /// that can be read.
    pub label: String,
    /// The app's other labels as (BCP 47 tag, label), such as ("pt-BR",
    /// "Calculadora"), sorted by tag.
    pub labels: Vec<(String, String)>,
    /// `module.requestPermissions` names, sorted and deduplicated.
    pub permissions: Vec<String>,
    /// ABIs with libraries under `libs/`, sorted.
    pub native_code: Vec<String>,
    /// Custom URI schemes of exported abilities that open links from a
    /// browser, validated, sorted and deduplicated.
    pub schemes: Vec<String>,
}

#[derive(Deserialize)]
struct ModuleJson {
    app: App,
    module: Module,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct App {
    bundle_name: String,
    #[serde(default)]
    version_code: Option<serde_json::Number>,
    #[serde(default)]
    version_name: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    label_id: Option<u32>,
    #[serde(default)]
    icon_id: Option<u32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Module {
    name: String,
    #[serde(default)]
    request_permissions: Vec<Permission>,
    #[serde(default)]
    abilities: Vec<Ability>,
}

#[derive(Deserialize)]
struct Permission {
    name: String,
}

#[derive(Deserialize)]
struct Ability {
    #[serde(default)]
    exported: bool,
    #[serde(default)]
    skills: Vec<Skill>,
}

#[derive(Deserialize)]
struct Skill {
    #[serde(default)]
    actions: Vec<String>,
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    uris: Vec<SkillUri>,
}

#[derive(Deserialize)]
struct SkillUri {
    #[serde(default)]
    scheme: String,
}

/// The want that opens a link, as [`crate::ohos::open_uri`] sends it.
const VIEW: &str = "ohos.want.action.viewData";
const BROWSABLE: &str = "entity.system.browsable";

/// A HAP's zip and its `module.json` and `resources.index`.
struct Hap {
    zip: zip::ZipArchive<File>,
    files: Vec<String>,
    module: Vec<u8>,
    index: Option<Vec<u8>>,
}

fn zip_entry(zip: &mut zip::ZipArchive<File>, name: &str, cap: u64) -> Result<Option<Vec<u8>>> {
    let e = match zip.by_name(name) {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("read {name}")),
    };
    let mut buf = vec![];
    e.take(cap + 1)
        .read_to_end(&mut buf)
        .with_context(|| format!("read {name}"))?;
    if buf.len() as u64 > cap {
        bail!("{name} is larger than {} MiB", cap >> 20);
    }
    Ok(Some(buf))
}

fn open(hap: &Path) -> Result<Hap> {
    let f = File::open(hap).with_context(|| format!("open {}", hap.display()))?;
    let mut zip = zip::ZipArchive::new(f).with_context(|| format!("read zip {}", hap.display()))?;
    let files: Vec<String> = zip
        .file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .collect();
    let Some(module) = zip_entry(&mut zip, "module.json", MAX_MODULE_JSON)? else {
        if files.iter().any(|f| f == "config.json") {
            bail!(
                "{} uses the FA model (config.json), which isn't supported",
                hap.display()
            );
        }
        bail!("{} has no module.json", hap.display());
    };
    let index = zip_entry(&mut zip, "resources.index", MAX_INDEX)?;
    Ok(Hap {
        zip,
        files,
        module,
        index,
    })
}

/// Reads a HAP's metadata.
pub fn read(hap: &Path) -> Result<HapInfo> {
    let h = open(hap)?;
    parse(&h.module, h.index.as_deref(), &h.files)
        .with_context(|| format!("read the module.json of {}", hap.display()))
}

/// Renders a HAP's app icon at each of `sizes`, as square PNGs. `mask` is
/// OpenHarmony's icon mask ([`icon::icon_mask`]), which cuts a layered icon.
/// Returns nothing if the HAP has no icon that can be rendered.
pub fn icons(hap: &Path, mask: Option<&[u8]>, sizes: &[u32]) -> Vec<(u32, Vec<u8>)> {
    let Ok(mut h) = open(hap) else {
        return vec![];
    };
    let Ok(m) = serde_json::from_slice::<ModuleJson>(&h.module) else {
        return vec![];
    };
    match (m.app.icon_id, h.index.as_deref()) {
        (Some(id), Some(index)) => icon::render(&mut h.zip, index, id, mask, sizes),
        _ => vec![],
    }
}

/// Does what [`read`] does, given a HAP's `module.json`, `resources.index`
/// and file names.
fn parse(module: &[u8], index: Option<&[u8]>, files: &[String]) -> Result<HapInfo> {
    let m: ModuleJson = serde_json::from_slice(module)?;
    let (label, labels) = if m.app.label.starts_with('$') {
        let values = m
            .app
            .label_id
            .zip(index)
            .map(|(id, index)| index::values(index, id))
            .unwrap_or_default();
        let label = values
            .iter()
            .find(|v| v.qualifiers.is_empty())
            .map(|v| v.value.clone())
            .unwrap_or_default();
        let labels: std::collections::BTreeMap<String, String> = values
            .into_iter()
            .filter_map(|v| Some((index::locale(&v.qualifiers)?, v.value)))
            .collect();
        (label, labels.into_iter().collect())
    } else {
        (m.app.label, vec![])
    };
    let schemes: BTreeSet<String> = m
        .module
        .abilities
        .iter()
        .filter(|a| a.exported)
        .flat_map(|a| &a.skills)
        .filter(|s| {
            s.actions.iter().any(|a| a == VIEW) && s.entities.iter().any(|e| e == BROWSABLE)
        })
        .flat_map(|s| &s.uris)
        .map(|u| u.scheme.to_ascii_lowercase())
        .filter(|s| crate::validate::custom_scheme(s))
        .collect();
    let permissions: BTreeSet<String> = m
        .module
        .request_permissions
        .into_iter()
        .map(|p| p.name)
        .collect();
    let native_code: BTreeSet<String> = files
        .iter()
        .filter_map(|f| f.strip_prefix("libs/")?.split_once('/'))
        .filter(|(abi, rest)| !abi.is_empty() && !rest.is_empty())
        .map(|(abi, _)| abi.to_string())
        .collect();
    Ok(HapInfo {
        bundle: m.app.bundle_name,
        module: m.module.name,
        version_code: m
            .app
            .version_code
            .map(|v| v.to_string())
            .unwrap_or_default(),
        version_name: m.app.version_name,
        label,
        labels,
        permissions: permissions.into_iter().collect(),
        native_code: native_code.into_iter().collect(),
        schemes: schemes.into_iter().collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::index::build::{index, letters};
    use super::*;
    use std::io::Write;

    const MODULE: &str = r#"{
        "app": {"bundleName": "dev.example.app", "versionCode": 1000002,
                "versionName": "1.0.2", "label": "$string:app_name", "labelId": 16777216,
                "icon": "$media:icon", "iconId": 16777300},
        "module": {"name": "entry", "type": "entry",
                   "requestPermissions": [{"name": "ohos.permission.INTERNET"},
                                          {"name": "ohos.permission.CAMERA"},
                                          {"name": "ohos.permission.INTERNET"}],
                   "abilities": [
                     {"name": "Main", "exported": true, "skills": [
                       {"entities": ["entity.system.home"], "actions": ["action.system.home"]},
                       {"entities": ["entity.system.browsable"], "actions": ["ohos.want.action.viewData"],
                        "uris": [{"scheme": "Calc"}, {"scheme": "https", "host": "example.org"}, {"host": "x"}]}]},
                     {"name": "Hidden", "skills": [
                       {"entities": ["entity.system.browsable"], "actions": ["ohos.want.action.viewData"],
                        "uris": [{"scheme": "hidden"}]}]},
                     {"name": "NotBrowsable", "exported": true, "skills": [
                       {"actions": ["ohos.want.action.viewData"], "uris": [{"scheme": "plain"}]}]}]}
    }"#;

    const DE: &[(u32, u32)] = &[(0, u32::from_be_bytes([0, 0, b'd', b'e']))];

    fn label_index() -> Vec<u8> {
        let zh_cn: &[(u32, u32)] = &[(0, letters("zh")), (1, letters("CN"))];
        index(
            &[(3, DE), (0, &[]), (4, zh_cn), (5, &[(6, 1)])],
            &[
                (16777217, &[(0, "other")]),
                (
                    16777216,
                    &[
                        (3, "Rechner"),
                        (0, "Calculator"),
                        (4, "计算器"),
                        (5, "Dark"),
                    ],
                ),
            ],
        )
    }

    #[test]
    fn module_json_labels_and_schemes() {
        let files = [
            "module.json",
            "libs/x86_64/libapp.so",
            "libs/arm64-v8a/libapp.so",
            "libs/x86_64/",
            "libs/readme",
        ]
        .map(String::from);
        let h = parse(MODULE.as_bytes(), Some(&label_index()), &files).unwrap();
        assert_eq!(
            h,
            HapInfo {
                bundle: "dev.example.app".into(),
                module: "entry".into(),
                version_code: "1000002".into(),
                version_name: "1.0.2".into(),
                label: "Calculator".into(),
                labels: vec![
                    ("de".into(), "Rechner".into()),
                    ("zh-CN".into(), "计算器".into())
                ],
                permissions: vec![
                    "ohos.permission.CAMERA".into(),
                    "ohos.permission.INTERNET".into()
                ],
                native_code: vec!["arm64-v8a".into(), "x86_64".into()],
                schemes: vec!["calc".into()],
            }
        );
    }

    #[test]
    fn labels_without_a_default_value_or_index_are_empty() {
        let localized = index(&[(3, DE)], &[(16777216, &[(3, "Rechner")])]);
        let missing = index(&[(0, &[])], &[(1, &[(0, "x")])]);
        for idx in [Some(localized.as_slice()), Some(missing.as_slice()), None] {
            assert_eq!(parse(MODULE.as_bytes(), idx, &[]).unwrap().label, "");
        }
        let literal = MODULE.replace("$string:app_name", "Plain");
        let h = parse(literal.as_bytes(), Some(&label_index()), &[]).unwrap();
        assert_eq!((h.label.as_str(), h.labels.len()), ("Plain", 0));
    }

    #[test]
    fn malformed_module_json_is_an_error() {
        assert!(parse(b"{}", None, &[]).is_err());
        assert!(parse(b"not json", None, &[]).is_err());
    }

    fn write_hap(name: &str, files: &[(&str, Vec<u8>)]) -> std::path::PathBuf {
        let dir = crate::test_dir(name);
        let file = dir.join("app.hap");
        let mut zip = zip::ZipWriter::new(File::create(&file).unwrap());
        for (name, bytes) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(bytes).unwrap();
        }
        zip.finish().unwrap();
        file
    }

    #[test]
    fn fa_model_haps_are_refused() {
        let file = write_hap("hap-fa", &[("config.json", vec![])]);
        let e = read(&file).unwrap_err().to_string();
        assert!(e.contains("FA model"), "{e}");
        std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }

    /// A PNG of one colour.
    fn png(size: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut p = resvg::tiny_skia::Pixmap::new(size, size).unwrap();
        p.fill(resvg::tiny_skia::Color::from_rgba8(
            rgba[0], rgba[1], rgba[2], rgba[3],
        ));
        p.encode_png().unwrap()
    }

    fn rgba(png: &[u8], x: u32, y: u32) -> [u8; 4] {
        let p = resvg::tiny_skia::Pixmap::decode_png(png).unwrap();
        let c = p.pixel(x, y).unwrap().demultiply();
        [c.red(), c.green(), c.blue(), c.alpha()]
    }

    /// A HAP whose icon, resource 16777300, has `values` as (configuration,
    /// path) with configuration 0 the default, and the media `files`.
    fn icon_hap(
        name: &str,
        values: &[(u32, &str)],
        files: &[(&str, Vec<u8>)],
    ) -> std::path::PathBuf {
        let idx = index(
            &[(0, &[]), (1, &[(2, 160)]), (2, &[(2, 480)]), (3, &[(6, 1)])],
            &[
                (16777300, values),
                (16777301, &[(0, "entry/resources/base/media/bg.png")]),
                (16777302, &[(0, "entry/resources/base/media/fg.svg")]),
            ],
        );
        let mut all = vec![
            ("module.json", MODULE.as_bytes().to_vec()),
            ("resources.index", idx),
        ];
        all.extend(files.iter().map(|(n, b)| (*n, b.clone())));
        write_hap(name, &all)
    }

    #[test]
    fn svg_and_bitmap_icons_render_at_each_size() {
        // A wide SVG is centred.
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 10"><rect width="20" height="10" fill="#ff0000"/></svg>"##;
        let hap = icon_hap(
            "hap-svg-icon",
            &[(0, "entry/resources/base/media/icon.svg")],
            &[("resources/base/media/icon.svg", svg.to_vec())],
        );
        let rendered = icons(&hap, None, &[48, 64]);
        assert_eq!(
            rendered.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            [48, 64]
        );
        for (size, png) in &rendered {
            assert_eq!(crate::xdg::png_size(png), Some((*size, *size)));
        }
        assert_eq!(rgba(&rendered[0].1, 24, 24), [255, 0, 0, 255]);
        assert_eq!(rgba(&rendered[0].1, 24, 2)[3], 0);
        std::fs::remove_dir_all(hap.parent().unwrap()).unwrap();

        // The densest default bitmap wins over a lower density and dark mode.
        let hap = icon_hap(
            "hap-png-icon",
            &[
                (1, "entry/resources/base/media/low.png"),
                (2, "entry/resources/base/media/high.png"),
                (3, "entry/resources/base/media/dark.png"),
            ],
            &[
                ("resources/base/media/low.png", png(8, [255, 0, 0, 255])),
                ("resources/base/media/high.png", png(8, [0, 255, 0, 255])),
                ("resources/base/media/dark.png", png(8, [0, 0, 0, 255])),
            ],
        );
        let rendered = icons(&hap, None, &[48]);
        assert_eq!(rgba(&rendered[0].1, 24, 24), [0, 255, 0, 255]);
        std::fs::remove_dir_all(hap.parent().unwrap()).unwrap();
    }

    #[test]
    fn layered_icons_take_the_mask() {
        let layered = br#"{"layered-image": {"background": "$media:16777301", "foreground": "$media:16777302"}}"#;
        // The foreground covers the middle half.
        let fg = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 4 4"><rect x="1" y="1" width="2" height="2" fill="#ff0000"/></svg>"##;
        let hap = icon_hap(
            "hap-layered-icon",
            &[(0, "entry/resources/base/media/icon.json")],
            &[
                ("resources/base/media/icon.json", layered.to_vec()),
                ("resources/base/media/bg.png", png(4, [0, 0, 255, 255])),
                ("resources/base/media/fg.svg", fg.to_vec()),
            ],
        );
        // The mask keeps all but the left quarter.
        let mut mask = resvg::tiny_skia::Pixmap::new(4, 4).unwrap();
        mask.fill_rect(
            resvg::tiny_skia::Rect::from_xywh(1., 0., 3., 4.).unwrap(),
            &resvg::tiny_skia::Paint::default(),
            resvg::tiny_skia::Transform::identity(),
            None,
        );
        let mask = mask.encode_png().unwrap();
        let masked = icons(&hap, Some(&mask), &[64]);
        let png = &masked[0].1;
        assert_eq!(rgba(png, 32, 32), [255, 0, 0, 255]);
        assert_eq!(rgba(png, 60, 4), [0, 0, 255, 255]);
        assert_eq!(rgba(png, 4, 4)[3], 0);
        let square = icons(&hap, None, &[64]);
        assert_eq!(rgba(&square[0].1, 4, 4), [0, 0, 255, 255]);
        std::fs::remove_dir_all(hap.parent().unwrap()).unwrap();
    }

    #[test]
    fn unreadable_icons_render_nothing() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="4" height="4"/>"#.to_vec();
        for (name, value, files) in [
            // The file is missing.
            (
                "hap-icon-missing",
                "entry/resources/base/media/icon.png",
                vec![],
            ),
            // The path leaves the resources.
            (
                "hap-icon-outside",
                "entry/libs/icon.svg",
                vec![("libs/icon.svg", svg.clone())],
            ),
            // The layered icon isn't JSON.
            (
                "hap-icon-bad-json",
                "entry/resources/base/media/icon.json",
                vec![("resources/base/media/icon.json", b"{".to_vec())],
            ),
            // A layer names no media resource.
            (
                "hap-icon-bad-layer",
                "entry/resources/base/media/icon.json",
                vec![(
                    "resources/base/media/icon.json",
                    br#"{"layered-image": {"foreground": "$string:x"}}"#.to_vec(),
                )],
            ),
            // The format isn't an image.
            (
                "hap-icon-text",
                "entry/resources/base/media/icon.txt",
                vec![("resources/base/media/icon.txt", b"x".to_vec())],
            ),
        ] {
            let hap = icon_hap(name, &[(0, value)], &files);
            assert!(icons(&hap, None, &[48]).is_empty(), "{name}");
            std::fs::remove_dir_all(hap.parent().unwrap()).unwrap();
        }
    }
}
