//! Resource lookups in a `resources.index` that OpenHarmony's restool wrote.
//! Both layouts start with a 128-byte version string, the file size and the
//! number of configurations, followed by the configurations. Each
//! configuration is `KEYS`, a 32-bit field, its number of qualifiers and the
//! qualifiers. A qualifier is a key type and a value. The default
//! configuration has no qualifiers.

/// A qualifier's key type for a language, such as `zh`.
const LANGUAGE: u32 = 0;
/// A qualifier's key type for a region, such as `CN`.
const REGION: u32 = 1;
/// A qualifier's key type for a screen density in dpi, such as 480.
const DENSITY: u32 = 2;
/// A qualifier's key type for a script, such as `Latn`.
const SCRIPT: u32 = 5;

/// One value of a resource and the qualifiers of its configuration, as (key
/// type, value) pairs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Value {
    pub qualifiers: Vec<(u32, u32)>,
    pub value: String,
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

/// Returns every value of resource `id`. A malformed index has none.
pub fn values(index: &[u8], id: u32) -> Vec<Value> {
    let found = if index.starts_with(b"RestoolV2") {
        v2_values(index, id)
    } else if index.starts_with(b"Restool ") {
        v1_values(index, id)
    } else {
        None
    };
    found.unwrap_or_default()
}

/// Resource `id`'s value in the default configuration.
#[cfg(test)]
fn default_string(index: &[u8], id: u32) -> Option<String> {
    values(index, id)
        .into_iter()
        .find(|v| v.qualifiers.is_empty())
        .map(|v| v.value)
}

/// The BCP 47 tag of a configuration that only has a language, and maybe a
/// script and a region, such as `sr-Latn` or `pt-BR`.
pub fn locale(qualifiers: &[(u32, u32)]) -> Option<String> {
    let part = |key: u32| -> Option<Option<String>> {
        let Some((_, v)) = qualifiers.iter().find(|(k, _)| *k == key) else {
            return Some(None);
        };
        let bytes = v.to_be_bytes();
        let s: String = bytes
            .iter()
            .skip_while(|b| **b == 0)
            .map(|b| *b as char)
            .collect();
        (!s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric())).then_some(Some(s))
    };
    if qualifiers
        .iter()
        .any(|(k, _)| ![LANGUAGE, SCRIPT, REGION].contains(k))
    {
        return None;
    }
    let mut tag = part(LANGUAGE)??;
    for key in [SCRIPT, REGION] {
        if let Some(p) = part(key)? {
            tag.push('-');
            tag.push_str(&p);
        }
    }
    Some(tag)
}

/// The density in dpi of a configuration that has a density and nothing
/// else, or 0 for the default configuration.
pub fn density(qualifiers: &[(u32, u32)]) -> Option<u32> {
    match qualifiers {
        [] => Some(0),
        [(DENSITY, dpi)] => Some(*dpi),
        _ => None,
    }
}

/// A configuration's field and qualifiers.
type Config = (u32, Vec<(u32, u32)>);

/// The configurations that start at `at`.
fn configs(index: &[u8], at: usize) -> Option<Vec<Config>> {
    let mut out = vec![];
    let mut at = at;
    for _ in 0..u32_at(index, 132)? {
        if index.get(at..at.checked_add(4)?)? != b"KEYS" {
            return None;
        }
        let (field, count) = (u32_at(index, at + 4)?, u32_at(index, at + 8)? as usize);
        let qualifiers = (0..count)
            .map(|i| {
                let q = at.checked_add(12)?.checked_add(i.checked_mul(8)?)?;
                Some((u32_at(index, q)?, u32_at(index, q + 4)?))
            })
            .collect::<Option<Vec<_>>>()?;
        out.push((field, qualifiers));
        at = at.checked_add(12)?.checked_add(count.checked_mul(8)?)?;
    }
    Some(out)
}

/// The value at `at`: a 16-bit length and UTF-8 bytes, which may end in NUL.
fn value_at(index: &[u8], at: usize) -> Option<String> {
    let len = u16_at(index, at)? as usize;
    let bytes = index.get(at.checked_add(2)?..at.checked_add(2 + len)?)?;
    let bytes = bytes.strip_suffix(b"\0").unwrap_or(bytes);
    String::from_utf8(bytes.to_vec()).ok()
}

/// Restool's first layout (`Restool 6.0.0.001`). A configuration's field is
/// the offset of its id set, which is `IDSS`, a count and (id, item offset)
/// pairs. An item is its size, type and id, then its value and its name.
fn v1_values(index: &[u8], id: u32) -> Option<Vec<Value>> {
    let mut out = vec![];
    for (set, qualifiers) in configs(index, 136)? {
        let set = set as usize;
        if index.get(set..set.checked_add(4)?)? != b"IDSS" {
            return None;
        }
        for i in 0..u32_at(index, set + 4)? as usize {
            let pair = set.checked_add(8)?.checked_add(i.checked_mul(8)?)?;
            if u32_at(index, pair)? == id {
                let item = (u32_at(index, pair + 4)? as usize).checked_add(12)?;
                out.push(Value {
                    qualifiers: qualifiers.clone(),
                    value: value_at(index, item)?,
                });
                break;
            }
        }
    }
    Some(out)
}

/// Restool's V2 layout (`RestoolV2 6.1.0.003`). The header also holds the
/// offset of the data section, and a configuration's field is its index. The
/// data section is `DATA`, its size and its number of resources. Each
/// resource is its id, its size, its number of values, and a (configuration
/// index, value offset) pair per value.
fn v2_values(index: &[u8], id: u32) -> Option<Vec<Value>> {
    let data = u32_at(index, 136)? as usize;
    let configs = configs(index, 140)?;
    if index.get(data..data.checked_add(4)?)? != b"DATA" {
        return None;
    }
    let count = u32_at(index, data + 8)?;
    let mut at = data.checked_add(12)?;
    for _ in 0..count {
        let size = u32_at(index, at.checked_add(4)?)? as usize;
        if size < 12 {
            return None;
        }
        if u32_at(index, at)? == id {
            let n = u32_at(index, at + 8)? as usize;
            let mut out = vec![];
            for v in 0..n.min((size - 12) / 8) {
                let pair = at + 12 + v * 8;
                let config = u32_at(index, pair)?;
                let Some((_, qualifiers)) = configs.iter().find(|(f, _)| *f == config) else {
                    continue;
                };
                out.push(Value {
                    qualifiers: qualifiers.clone(),
                    value: value_at(index, u32_at(index, pair + 4)? as usize)?,
                });
            }
            return Some(out);
        }
        at = at.checked_add(size)?;
    }
    Some(vec![])
}

#[cfg(test)]
pub mod build {
    /// Packs a qualifier's letters, as restool does.
    pub fn letters(s: &str) -> u32 {
        s.bytes().fold(0, |v, b| (v << 8) | b as u32)
    }

    /// Builds a V2 `resources.index` with configurations `(index,
    /// qualifiers)` and resources `(id, [(configuration, value)])`.
    pub fn index(configs: &[(u32, &[(u32, u32)])], resources: &[(u32, &[(u32, &str)])]) -> Vec<u8> {
        let mut b = b"RestoolV2 6.1.0.003".to_vec();
        b.resize(128, 0);
        b.extend([0u8; 12]);
        for (i, qualifiers) in configs {
            b.extend(b"KEYS");
            b.extend(i.to_le_bytes());
            b.extend((qualifiers.len() as u32).to_le_bytes());
            for (k, v) in *qualifiers {
                b.extend(k.to_le_bytes());
                b.extend(v.to_le_bytes());
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
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

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
        b.extend(LANGUAGE.to_le_bytes());
        b.extend(letters("de").to_le_bytes());
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
    fn v1_values_have_their_qualifiers() {
        let idx = index_v1(
            &[(16777216, "Dunkel")],
            &[(16777217, "other"), (16777216, "FlorisBoard")],
        );
        assert_eq!(
            values(&idx, 16777216),
            [
                Value {
                    qualifiers: vec![(LANGUAGE, letters("de"))],
                    value: "Dunkel".into()
                },
                Value {
                    qualifiers: vec![],
                    value: "FlorisBoard".into()
                }
            ]
        );
        assert_eq!(default_string(&idx, 16777217).as_deref(), Some("other"));
        assert_eq!(default_string(&idx, 5), None);
        for cut in [130, 140, 160, idx.len() - 4] {
            assert_eq!(default_string(&idx[..cut], 16777216), None, "cut {cut}");
        }
    }

    #[test]
    fn v2_values_have_their_qualifiers() {
        let de: &[(u32, u32)] = &[(LANGUAGE, letters("de"))];
        let idx = index(
            &[(3, de), (0, &[])],
            &[
                (16777217, &[(0, "other")]),
                (16777216, &[(3, "Rechner"), (0, "Calculator"), (9, "lost")]),
            ],
        );
        let v = values(&idx, 16777216);
        assert_eq!(v.len(), 2, "a value for a missing configuration is skipped");
        assert_eq!(v[0].qualifiers, de);
        assert_eq!(
            default_string(&idx, 16777216).as_deref(),
            Some("Calculator")
        );
        assert!(values(&idx, 1).is_empty());
    }

    #[test]
    fn malformed_indexes_have_no_values() {
        let idx = index(&[(0, &[])], &[(16777216, &[(0, "Calculator")])]);
        for cut in [0, 100, 135, 141, 160, idx.len() - 3] {
            assert_eq!(default_string(&idx[..cut], 16777216), None, "cut {cut}");
        }
        let mut other = idx.clone();
        other[..9].copy_from_slice(b"Unknown 1");
        assert_eq!(default_string(&other, 16777216), None);
        let mut huge = idx.clone();
        let data = u32_at(&idx, 136).unwrap() as usize;
        huge[data + 8..data + 12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(default_string(&huge, 1), None);
    }

    #[test]
    fn locales_and_densities() {
        let l = |s: &str| (LANGUAGE, letters(s));
        assert_eq!(locale(&[l("de")]).as_deref(), Some("de"));
        assert_eq!(locale(&[l("fil")]).as_deref(), Some("fil"));
        assert_eq!(
            locale(&[l("zh"), (REGION, letters("CN"))]).as_deref(),
            Some("zh-CN")
        );
        assert_eq!(
            locale(&[(REGION, letters("RS")), l("sr"), (SCRIPT, letters("Latn"))]).as_deref(),
            Some("sr-Latn-RS")
        );
        // Dark mode, a device type, a region alone and bad letters aren't
        // locales.
        assert_eq!(locale(&[(6, 1)]), None);
        assert_eq!(locale(&[l("es"), (4, 6)]), None);
        assert_eq!(locale(&[(REGION, letters("CN"))]), None);
        assert_eq!(locale(&[(LANGUAGE, 0)]), None);
        assert_eq!(locale(&[(LANGUAGE, letters("a-"))]), None);
        assert_eq!(locale(&[]), None);
        assert_eq!(density(&[]), Some(0));
        assert_eq!(density(&[(DENSITY, 480)]), Some(480));
        assert_eq!(density(&[(DENSITY, 480), (6, 1)]), None);
        assert_eq!(density(&[l("de")]), None);
    }
}
