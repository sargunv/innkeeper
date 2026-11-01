//! HAP metadata for OpenHarmony profiles. A HAP is a zip whose `module.json`
//! describes the bundle and one module. Labels such as `$string:app_name`
//! resolve through `resources.index`, which OpenHarmony's restool writes. No
//! OpenHarmony SDK or session is needed. Everything read is untrusted, so
//! front ends validate it before use.

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
    /// `module.requestPermissions` names, sorted and deduplicated.
    pub permissions: Vec<String>,
    /// ABIs with libraries under `libs/`, sorted.
    pub native_code: Vec<String>,
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
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Module {
    name: String,
    #[serde(default)]
    request_permissions: Vec<Permission>,
}

#[derive(Deserialize)]
struct Permission {
    name: String,
}

/// Reads a HAP's metadata.
pub fn read(hap: &Path) -> Result<HapInfo> {
    let f = File::open(hap).with_context(|| format!("open {}", hap.display()))?;
    let mut zip = zip::ZipArchive::new(f).with_context(|| format!("read zip {}", hap.display()))?;
    let files: Vec<String> = zip
        .file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .collect();
    let mut entry = |name: &str, cap: u64| -> Result<Option<Vec<u8>>> {
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
    };
    let Some(module) = entry("module.json", MAX_MODULE_JSON)? else {
        if files.iter().any(|f| f == "config.json") {
            bail!(
                "{} uses the FA model (config.json), which isn't supported",
                hap.display()
            );
        }
        bail!("{} has no module.json", hap.display());
    };
    let index = entry("resources.index", MAX_INDEX)?;
    parse(&module, index.as_deref(), &files)
        .with_context(|| format!("read the module.json of {}", hap.display()))
}

/// Does what [`read`] does, given a HAP's `module.json`, `resources.index`
/// and file names.
fn parse(module: &[u8], index: Option<&[u8]>, files: &[String]) -> Result<HapInfo> {
    let m: ModuleJson = serde_json::from_slice(module)?;
    let label = if m.app.label.starts_with('$') {
        m.app
            .label_id
            .zip(index)
            .and_then(|(id, index)| resource_string(index, id))
            .unwrap_or_default()
    } else {
        m.app.label
    };
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
        permissions: permissions.into_iter().collect(),
        native_code: native_code.into_iter().collect(),
    })
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        b.get(at..at.checked_add(2)?)?.try_into().ok()?,
    ))
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        b.get(at..at.checked_add(4)?)?.try_into().ok()?,
    ))
}

/// Returns resource `id`'s value in the default configuration, from a
/// `resources.index` that restool wrote. Both layouts start with a 128-byte
/// version string, the file size and the number of configurations, followed
/// by the configurations. Each configuration is `KEYS`, a 32-bit field, its
/// number of qualifiers and the qualifiers. The default configuration has no
/// qualifiers.
fn resource_string(index: &[u8], id: u32) -> Option<String> {
    if index.starts_with(b"RestoolV2") {
        v2_string(index, id)
    } else if index.starts_with(b"Restool ") {
        v1_string(index, id)
    } else {
        None
    }
}

/// The 32-bit fields of each default configuration in `index`.
fn default_configs(index: &[u8], configs_at: usize) -> Option<Vec<u32>> {
    let mut defaults = vec![];
    let mut at = configs_at;
    for _ in 0..u32_at(index, 132)? {
        if index.get(at..at.checked_add(4)?)? != b"KEYS" {
            return None;
        }
        let (field, qualifiers) = (u32_at(index, at + 4)?, u32_at(index, at + 8)?);
        if qualifiers == 0 {
            defaults.push(field);
        }
        at = at.checked_add(12)?.checked_add(qualifiers as usize * 8)?;
    }
    Some(defaults)
}

/// The value at `at`: a 16-bit length and UTF-8 bytes, which may end in NUL.
fn value_at(index: &[u8], at: usize) -> Option<String> {
    let len = u16_at(index, at)? as usize;
    let bytes = index.get(at + 2..at + 2 + len)?;
    let bytes = bytes.strip_suffix(b"\0").unwrap_or(bytes);
    String::from_utf8(bytes.to_vec()).ok()
}

/// Restool's first layout (`Restool 6.0.0.001`). A configuration's field is
/// the offset of its id set, which is `IDSS`, a count and (id, item offset)
/// pairs. An item is its size, type and id, then its value and its name.
fn v1_string(index: &[u8], id: u32) -> Option<String> {
    for set in default_configs(index, 136)? {
        let set = set as usize;
        if index.get(set..set.checked_add(4)?)? != b"IDSS" {
            return None;
        }
        for i in 0..u32_at(index, set + 4)? as usize {
            let pair = set.checked_add(8)?.checked_add(i.checked_mul(8)?)?;
            if u32_at(index, pair)? == id {
                return value_at(index, (u32_at(index, pair + 4)? as usize).checked_add(12)?);
            }
        }
    }
    None
}

/// Restool's V2 layout (`RestoolV2 6.1.0.003`). The header also holds the
/// offset of the data section, and a configuration's field is its index. The
/// data section is `DATA`, its size and its number of resources. Each
/// resource is its id, its size, its number of values, and a (configuration
/// index, value offset) pair per value.
fn v2_string(index: &[u8], id: u32) -> Option<String> {
    let data = u32_at(index, 136)? as usize;
    let defaults = default_configs(index, 140)?;
    if index.get(data..data.checked_add(4)?)? != b"DATA" {
        return None;
    }
    let count = u32_at(index, data + 8)?;
    let mut at = data + 12;
    for _ in 0..count {
        let size = u32_at(index, at + 4)? as usize;
        if size < 12 {
            return None;
        }
        if u32_at(index, at)? == id {
            let values = u32_at(index, at + 8)? as usize;
            for v in 0..values.min((size - 12) / 8) {
                let pair = at + 12 + v * 8;
                if !defaults.contains(&u32_at(index, pair)?) {
                    continue;
                }
                return value_at(index, u32_at(index, pair + 4)? as usize);
            }
            return None;
        }
        at = at.checked_add(size)?;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a V2 `resources.index` with configurations `(index, qualifier
    /// count)` and resources `(id, [(configuration, value)])`.
    fn index(configs: &[(u32, u32)], resources: &[(u32, &[(u32, &str)])]) -> Vec<u8> {
        let mut b = b"RestoolV2 6.1.0.003".to_vec();
        b.resize(128, 0);
        b.extend([0u8; 12]);
        for (i, q) in configs {
            b.extend(b"KEYS");
            b.extend(i.to_le_bytes());
            b.extend(q.to_le_bytes());
            for _ in 0..*q {
                b.extend(0u32.to_le_bytes());
                b.extend(u32::from_le_bytes(*b"ne\0\0").to_le_bytes());
            }
        }
        let data = b.len();
        let entries: usize = resources.iter().map(|(_, v)| 12 + v.len() * 8).sum();
        let mut value_at = data + 12 + entries;
        let (mut entry_bytes, mut values) = (vec![], vec![]);
        for (id, vs) in resources {
            entry_bytes.extend(id.to_le_bytes());
            entry_bytes.extend((12 + vs.len() as u32 * 8).to_le_bytes());
            entry_bytes.extend((vs.len() as u32).to_le_bytes());
            for (config, value) in *vs {
                entry_bytes.extend(config.to_le_bytes());
                entry_bytes.extend((value_at as u32).to_le_bytes());
                values.extend((value.len() as u16).to_le_bytes());
                values.extend(value.as_bytes());
                value_at += 2 + value.len();
            }
        }
        b.extend(b"DATA");
        b.extend(((12 + entries) as u32).to_le_bytes());
        b.extend((resources.len() as u32).to_le_bytes());
        b.extend(entry_bytes);
        b.extend(values);
        let size = b.len() as u32;
        b[128..132].copy_from_slice(&size.to_le_bytes());
        b[132..136].copy_from_slice(&(configs.len() as u32).to_le_bytes());
        b[136..140].copy_from_slice(&(data as u32).to_le_bytes());
        b
    }

    /// Builds a V1 `resources.index` with a qualified configuration and the
    /// default configuration, each with resources `(id, value)`.
    fn index_v1(qualified: &[(u32, &str)], default: &[(u32, &str)]) -> Vec<u8> {
        let mut b = b"Restool 6.0.0.001".to_vec();
        b.resize(128, 0);
        b.extend(0u32.to_le_bytes());
        b.extend(2u32.to_le_bytes());
        let sets_at = b.len() + 20 + 12;
        let qualified_set = sets_at;
        let default_set = qualified_set + 8 + qualified.len() * 8;
        b.extend(b"KEYS");
        b.extend((qualified_set as u32).to_le_bytes());
        b.extend(1u32.to_le_bytes());
        b.extend([6, 0, 0, 0, 1, 0, 0, 0]);
        b.extend(b"KEYS");
        b.extend((default_set as u32).to_le_bytes());
        b.extend(0u32.to_le_bytes());
        let mut item_at = default_set + 8 + default.len() * 8;
        let mut items = vec![];
        for set in [qualified, default] {
            b.extend(b"IDSS");
            b.extend((set.len() as u32).to_le_bytes());
            for (id, value) in set {
                b.extend(id.to_le_bytes());
                b.extend((item_at as u32).to_le_bytes());
                let mut item = vec![];
                item.extend(9u32.to_le_bytes());
                item.extend(id.to_le_bytes());
                item.extend((value.len() as u16 + 1).to_le_bytes());
                item.extend(value.as_bytes());
                item.extend([0, 1, 0, b'n']);
                items.extend((item.len() as u32 + 4).to_le_bytes());
                items.extend(item);
                item_at = default_set + 8 + default.len() * 8 + items.len();
            }
        }
        b.extend(items);
        b
    }

    #[test]
    fn v1_index_default_label() {
        let idx = index_v1(
            &[(16777216, "Dunkel")],
            &[(16777217, "other"), (16777216, "FlorisBoard")],
        );
        let h = parse(MODULE.as_bytes(), Some(&idx), &[]).unwrap();
        assert_eq!(h.label, "FlorisBoard");
        assert_eq!(resource_string(&idx, 16777217).as_deref(), Some("other"));
        assert_eq!(resource_string(&idx, 5), None);
        for cut in [130, 140, 160, idx.len() - 4] {
            assert_eq!(resource_string(&idx[..cut], 16777216), None, "cut {cut}");
        }
    }

    const MODULE: &str = r#"{
        "app": {"bundleName": "dev.example.app", "versionCode": 1000002,
                "versionName": "1.0.2", "label": "$string:app_name", "labelId": 16777216},
        "module": {"name": "entry", "type": "entry",
                   "requestPermissions": [{"name": "ohos.permission.INTERNET"},
                                          {"name": "ohos.permission.CAMERA"},
                                          {"name": "ohos.permission.INTERNET"}]}
    }"#;

    #[test]
    fn module_json_and_default_label() {
        let idx = index(
            &[(3, 2), (0, 0)],
            &[
                (16777217, &[(0, "other")]),
                (16777216, &[(3, "Rechner"), (0, "Calculator")]),
            ],
        );
        let files = [
            "module.json",
            "libs/x86_64/libapp.so",
            "libs/arm64-v8a/libapp.so",
            "libs/x86_64/",
            "libs/readme",
        ]
        .map(String::from);
        let h = parse(MODULE.as_bytes(), Some(&idx), &files).unwrap();
        assert_eq!(
            h,
            HapInfo {
                bundle: "dev.example.app".into(),
                module: "entry".into(),
                version_code: "1000002".into(),
                version_name: "1.0.2".into(),
                label: "Calculator".into(),
                permissions: vec![
                    "ohos.permission.CAMERA".into(),
                    "ohos.permission.INTERNET".into()
                ],
                native_code: vec!["arm64-v8a".into(), "x86_64".into()],
            }
        );
    }

    #[test]
    fn labels_without_a_default_value_or_index_are_empty() {
        let localized = index(&[(3, 1)], &[(16777216, &[(3, "Rechner")])]);
        let missing = index(&[(0, 0)], &[(1, &[(0, "x")])]);
        for idx in [Some(localized.as_slice()), Some(missing.as_slice()), None] {
            assert_eq!(parse(MODULE.as_bytes(), idx, &[]).unwrap().label, "");
        }
        let literal = MODULE.replace("$string:app_name", "Plain");
        assert_eq!(parse(literal.as_bytes(), None, &[]).unwrap().label, "Plain");
    }

    #[test]
    fn malformed_input_is_an_error_or_no_label() {
        assert!(parse(b"{}", None, &[]).is_err());
        assert!(parse(b"not json", None, &[]).is_err());
        let idx = index(&[(0, 0)], &[(16777216, &[(0, "Calculator")])]);
        for cut in [0, 100, 135, 141, 160, idx.len() - 3] {
            assert_eq!(resource_string(&idx[..cut], 16777216), None, "cut {cut}");
        }
        let mut other = idx.clone();
        other[..9].copy_from_slice(b"Unknown 1");
        assert_eq!(resource_string(&other, 16777216), None);
        let mut huge = idx.clone();
        let data = u32_at(&idx, 136).unwrap() as usize;
        huge[data + 8..data + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(resource_string(&huge, 1), None);
        assert_eq!(
            resource_string(&idx, 16777216).as_deref(),
            Some("Calculator")
        );
    }

    #[test]
    fn fa_model_haps_are_refused() {
        let dir = crate::test_dir("hap-fa");
        let file = dir.join("old.hap");
        let mut zip = zip::ZipWriter::new(File::create(&file).unwrap());
        zip.start_file("config.json", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.finish().unwrap();
        let e = read(&file).unwrap_err().to_string();
        assert!(e.contains("FA model"), "{e}");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
