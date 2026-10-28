//! The resource table (`resources.arsc`): each resource's values per
//! configuration (`ResTable_config`). Only what an install needs is decoded:
//! a value's locale, density and SDK level, and whether any other qualifier
//! (night mode, screen size, ...) is set.

use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};

use super::chunk::{Chunk, STRING_POOL, StringPool, Value, u8_at, u16_at, u32_at};
use super::locale::Locale;

const TABLE: u16 = 0x0002;
const PACKAGE: u16 = 0x0200;
const TYPE: u16 = 0x0201;

const NO_ENTRY: u32 = 0xffff_ffff;
const FLAG_SPARSE: u8 = 0x01;
const FLAG_OFFSET16: u8 = 0x02;
const ENTRY_COMPLEX: u16 = 0x0001;
const ENTRY_COMPACT: u16 = 0x0008;

/// A resource configuration, reduced to what we select by.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Config {
    pub locale: Locale,
    /// `ACONFIGURATION_DENSITY_*`: 0 default, 160 mdpi, 0xfffe anydpi,
    /// 0xffff nodpi.
    pub density: u16,
    /// The `-vN` qualifier.
    pub sdk: u16,
    /// Any other qualifier is set.
    pub other: bool,
}

/// A two-letter code, or a three-letter one packed into two bytes
/// (`ResTable_config::unpackLanguageOrRegion`); `base` is `a` or `0`.
fn unpack(b: [u8; 2], base: u8) -> String {
    if b[0] & 0x80 != 0 {
        let first = b[1] & 0x1f;
        let second = ((b[1] & 0xe0) >> 5) | ((b[0] & 0x03) << 3);
        let third = (b[0] & 0x7c) >> 2;
        [first, second, third]
            .iter()
            .map(|c| (c + base) as char)
            .collect()
    } else {
        b.iter()
            .take_while(|c| **c != 0)
            .map(|c| *c as char)
            .collect()
    }
}

fn ascii(b: &[u8]) -> String {
    b.iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as char)
        .collect()
}

impl Config {
    /// Parses a `ResTable_config`, which grew over Android releases: fields
    /// past its `size` are zero.
    pub fn parse(b: &[u8]) -> Result<Self> {
        let size = (u32_at(b, 0)? as usize).min(b.len());
        let mut c = [0u8; 64];
        c[..size.min(64)].copy_from_slice(&b[..size.min(64)]);
        let u16 = |o: usize| u16::from_le_bytes([c[o], c[o + 1]]);
        let locale = Locale {
            lang: unpack([c[8], c[9]], b'a'),
            // A script Android computed (`localeScriptWasComputed`) wasn't
            // named by the APK.
            script: if c[52] != 0 {
                String::new()
            } else {
                ascii(&c[36..40])
            },
            region: unpack([c[10], c[11]], b'0'),
            variant: ascii(&c[40..48]),
        };
        // mcc, mnc; orientation through inputFlags (except density) and
        // grammatical gender; screen sizes; minor version; screen layout, UI
        // mode, dp sizes; screenLayout2 and colour mode.
        let other = c[4..8].iter().any(|b| *b != 0)
            || [12, 13, 16, 17, 18, 19].iter().any(|o| c[*o] != 0)
            || c[20..24].iter().any(|b| *b != 0)
            || u16(26) != 0
            || c[28..36].iter().any(|b| *b != 0)
            || c[48..50].iter().any(|b| *b != 0);
        Ok(Self {
            locale,
            density: u16(14),
            sdk: u16(24),
            other,
        })
    }
}

/// One `ResTable_type` chunk: the entries of one type in one configuration.
struct TypeChunk<'a> {
    /// The type byte of resource IDs (`0x7fTT0000`).
    id: u8,
    config: Config,
    chunk: Chunk<'a>,
}

struct Package<'a> {
    id: u8,
    types: Vec<TypeChunk<'a>>,
}

pub struct Table<'a> {
    strings: StringPool<'a>,
    packages: Vec<Package<'a>>,
}

impl<'a> Table<'a> {
    pub fn parse(b: &'a [u8]) -> Result<Self> {
        let t = Chunk::at(b).context("resource table")?;
        if t.ty != TABLE {
            bail!("not a resource table (chunk type {:#06x})", t.ty);
        }
        let mut strings = None;
        let mut packages = vec![];
        for c in t.children() {
            let c = c?;
            match c.ty {
                STRING_POOL if strings.is_none() => strings = Some(StringPool::new(c)?),
                PACKAGE => packages.push(package(c)?),
                _ => {}
            }
        }
        Ok(Self {
            strings: strings.context("resource table without a string pool")?,
            packages,
        })
    }

    /// Every locale any resource has a value for (the default included).
    pub fn locales(&self) -> BTreeSet<&Locale> {
        self.packages
            .iter()
            .flat_map(|p| &p.types)
            .map(|t| &t.config.locale)
            .collect()
    }

    /// Every configuration's value of resource `id`.
    pub fn values(&self, id: u32) -> Vec<(&Config, Value)> {
        let (p, t, e) = ((id >> 24) as u8, (id >> 16) as u8, (id & 0xffff) as usize);
        let mut out = vec![];
        for pkg in self.packages.iter().filter(|x| x.id == p) {
            for ty in pkg.types.iter().filter(|x| x.id == t) {
                // A malformed entry is skipped, like a missing one.
                if let Ok(Some(v)) = self.entry(ty, e) {
                    out.push((&ty.config, v));
                }
            }
        }
        out
    }

    fn entry(&self, ty: &TypeChunk, i: usize) -> Result<Option<Value>> {
        let h = ty.chunk.header;
        let flags = u8_at(h, 9)?;
        let count = u32_at(h, 12)? as usize;
        let entries = u32_at(h, 16)? as usize;
        let offsets = ty.chunk.body;
        let off = if flags & FLAG_SPARSE != 0 {
            // (index, offset / 4) pairs sorted by index.
            let mut found = None;
            for k in 0..count {
                let idx = u16_at(offsets, k * 4)? as usize;
                if idx == i {
                    found = Some(u16_at(offsets, k * 4 + 2)? as u32 * 4);
                    break;
                }
                if idx > i {
                    break;
                }
            }
            match found {
                Some(o) => o,
                None => return Ok(None),
            }
        } else if i >= count {
            return Ok(None);
        } else if flags & FLAG_OFFSET16 != 0 {
            match u16_at(offsets, i * 2)? {
                0xffff => return Ok(None),
                o => o as u32 * 4,
            }
        } else {
            match u32_at(offsets, i * 4)? {
                NO_ENTRY => return Ok(None),
                o => o,
            }
        };
        let d = ty.chunk.data;
        let at = entries + off as usize;
        let size = u16_at(d, at)? as usize;
        let eflags = u16_at(d, at + 2)?;
        let (vty, data) = if eflags & ENTRY_COMPACT != 0 {
            ((eflags >> 8) as u8, u32_at(d, at + 4)?)
        } else if eflags & ENTRY_COMPLEX != 0 {
            // Styles, arrays, plurals: nothing an install reads.
            return Ok(Some(Value::Other));
        } else {
            (u8_at(d, at + size + 3)?, u32_at(d, at + size + 4)?)
        };
        Value::new(vty, data, &self.strings).map(Some)
    }
}

fn package(c: Chunk) -> Result<Package> {
    let id = u32_at(c.header, 8)? as u8;
    // Shared libraries offset their type IDs (`typeIdOffset`).
    let type_offset = if c.header.len() >= 288 {
        u32_at(c.header, 284)? as u8
    } else {
        0
    };
    let mut types = vec![];
    for t in c.children() {
        let t = t?;
        if t.ty != TYPE {
            continue;
        }
        let config = Config::parse(t.header.get(20..).context("short resource type header")?)?;
        types.push(TypeChunk {
            id: u8_at(t.header, 8)?.wrapping_add(type_offset),
            config,
            chunk: t,
        });
    }
    Ok(Package { id, types })
}

#[cfg(test)]
pub mod build {
    //! A writer for hand-made resource tables in tests.

    use super::super::chunk::build::{chunk, pool};

    /// A `ResTable_config` with a locale (two-letter language, optional
    /// region), density and SDK level.
    pub fn config(lang: &str, region: &str, density: u16, sdk: u16) -> Vec<u8> {
        let mut c = vec![0u8; 64];
        c[0..4].copy_from_slice(&64u32.to_le_bytes());
        c[8..8 + lang.len()].copy_from_slice(lang.as_bytes());
        c[10..10 + region.len()].copy_from_slice(region.as_bytes());
        c[14..16].copy_from_slice(&density.to_le_bytes());
        c[24..26].copy_from_slice(&sdk.to_le_bytes());
        c
    }

    /// Value entries: (entry index, type, data).
    pub enum Layout {
        Dense,
        Offset16,
        Sparse,
        Compact,
    }

    /// A type chunk of type `id` in `config`.
    pub fn type_chunk(
        id: u8,
        config: &[u8],
        entries: &[(u16, u8, u32)],
        layout: Layout,
    ) -> Vec<u8> {
        let count = entries.iter().map(|e| e.0 + 1).max().unwrap_or(0) as usize;
        let mut data = vec![];
        let mut offs: Vec<Option<u32>> = vec![None; count];
        for (i, ty, v) in entries {
            offs[*i as usize] = Some(data.len() as u32);
            if let Layout::Compact = layout {
                data.extend(0u16.to_le_bytes());
                data.extend((0x0008u16 | ((*ty as u16) << 8)).to_le_bytes());
                data.extend(v.to_le_bytes());
            } else {
                data.extend([8, 0, 0, 0, 0, 0, 0, 0]);
                data.extend([8, 0, 0, *ty]);
                data.extend(v.to_le_bytes());
            }
        }
        let (flags, table): (u8, Vec<u8>) = match layout {
            Layout::Dense | Layout::Compact => (
                0,
                offs.iter()
                    .flat_map(|o| o.unwrap_or(u32::MAX).to_le_bytes())
                    .collect(),
            ),
            Layout::Offset16 => (
                2,
                offs.iter()
                    .flat_map(|o| o.map_or(0xffff, |o| (o / 4) as u16).to_le_bytes())
                    .collect(),
            ),
            Layout::Sparse => (
                1,
                offs.iter()
                    .enumerate()
                    .filter_map(|(i, o)| {
                        o.map(|o| {
                            [(i as u16).to_le_bytes(), ((o / 4) as u16).to_le_bytes()].concat()
                        })
                    })
                    .flatten()
                    .collect(),
            ),
        };
        let n = if let Layout::Sparse = layout {
            entries.len()
        } else {
            count
        };
        let mut h = vec![id, flags, 0, 0];
        h.extend((n as u32).to_le_bytes());
        let header_len = 8 + 12 + config.len();
        h.extend(((header_len + table.len()) as u32).to_le_bytes());
        h.extend(config);
        chunk(0x0201, &h, &[table, data].concat())
    }

    pub fn table(strings: &[&str], package_id: u8, types: &[Vec<u8>]) -> Vec<u8> {
        let mut ph = vec![package_id, 0, 0, 0];
        ph.extend([0u8; 256]);
        ph.extend([0u8; 20]);
        let mut body = pool(&["string", "mipmap"], true);
        body.extend(pool(&["k"], true));
        types.iter().for_each(|t| body.extend(t));
        let pkg = chunk(0x0200, &ph, &body);
        let mut t = pool(strings, true);
        t.extend(pkg);
        chunk(0x0002, &1u32.to_le_bytes(), &t)
    }
}

#[cfg(test)]
mod tests {
    use super::build::*;
    use super::*;

    #[test]
    fn locales() {
        let tag = |c: &[u8]| Config::parse(c).unwrap().locale.tag();
        let mut c = config("de", "", 0, 0);
        assert_eq!(tag(&c), "de");
        c[10..12].copy_from_slice(b"AT");
        assert_eq!(tag(&c), "de-AT");
        // `ast` and `419`, packed.
        c[8..10].copy_from_slice(&[0xce, 0x40]);
        c[10..12].copy_from_slice(&[0xa4, 0x24]);
        assert_eq!(tag(&c), "ast-419");
        // b+sr+Latn: the script shows unless it was computed.
        let mut c = config("sr", "", 0, 0);
        c[36..40].copy_from_slice(b"Latn");
        assert_eq!(tag(&c), "sr-Latn");
        c[52] = 1;
        assert_eq!(tag(&c), "sr");
        let mut night = config("", "", 0, 0);
        assert!(!Config::parse(&night).unwrap().other);
        night[29] = 0x20;
        assert!(Config::parse(&night).unwrap().other);
        // An old, short config.
        let mut short = config("fr", "", 480, 4);
        short[0..4].copy_from_slice(&28u32.to_le_bytes());
        let s = Config::parse(&short).unwrap();
        assert_eq!((s.locale.tag().as_str(), s.density, s.sdk), ("fr", 480, 4));
    }

    #[test]
    fn layouts() {
        for layout in [
            Layout::Dense,
            Layout::Offset16,
            Layout::Sparse,
            Layout::Compact,
        ] {
            let b = table(
                &["Calc", "Rechner", "res/a.png"],
                0x7f,
                &[
                    type_chunk(
                        1,
                        &config("", "", 0, 0),
                        &[(0, 0x03, 0), (2, 0x12, 1)],
                        Layout::Dense,
                    ),
                    type_chunk(1, &config("de", "", 0, 0), &[(0, 0x03, 1)], layout),
                    type_chunk(
                        2,
                        &config("", "", 640, 0),
                        &[(0, 0x03, 2), (1, 0x01, 0x7f02_0000)],
                        Layout::Dense,
                    ),
                ],
            );
            let t = Table::parse(&b).unwrap();
            let v = t.values(0x7f01_0000);
            assert_eq!(v.len(), 2);
            assert_eq!(v[0].1, Value::Str("Calc".into()));
            assert_eq!(
                (v[1].0.locale.tag().as_str(), &v[1].1),
                ("de", &Value::Str("Rechner".into()))
            );
            assert_eq!(
                t.values(0x7f01_0002),
                [(&Config::default(), Value::Bool(true))]
            );
            assert!(t.values(0x7f01_0001).is_empty());
            assert!(t.values(0x7f01_0009).is_empty());
            assert!(t.values(0x0101_0000).is_empty());
            let icon = t.values(0x7f02_0000);
            assert_eq!(icon[0].0.density, 640);
            assert_eq!(icon[0].1, Value::Str("res/a.png".into()));
            assert_eq!(t.values(0x7f02_0001)[0].1, Value::Ref(0x7f02_0000));
        }
    }

    #[test]
    fn malformed() {
        let b = table(
            &["Calc"],
            0x7f,
            &[type_chunk(
                1,
                &config("", "", 0, 0),
                &[(0, 0x03, 0), (1, 0x03, 7)],
                Layout::Dense,
            )],
        );
        let t = Table::parse(&b).unwrap();
        // A string index out of range reads as no value.
        assert!(t.values(0x7f01_0001).is_empty());
        for n in 0..b.len() {
            let mut d = b[..n].to_vec();
            if d.len() >= 8 {
                d[4..8].copy_from_slice(&(n as u32).to_le_bytes());
            }
            // Never a panic; values from a truncated table may be missing.
            if let Ok(t) = Table::parse(&d) {
                t.values(0x7f01_0000);
            }
        }
    }
}
