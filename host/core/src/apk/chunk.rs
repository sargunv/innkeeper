//! The building blocks of Android's binary resource formats: chunks
//! (`ResChunk_header`), string pools (`ResStringPool`) and typed values
//! (`Res_value`), as laid out in AOSP's
//! `frameworks/base/libs/androidfw/include/androidfw/ResourceTypes.h`.
//! Every read is bounds-checked: a malformed file is an error, never a panic.

use anyhow::{Context, Result, bail};

pub const STRING_POOL: u16 = 0x0001;

pub fn u8_at(b: &[u8], o: usize) -> Result<u8> {
    b.get(o).copied().context("truncated resource data")
}

pub fn u16_at(b: &[u8], o: usize) -> Result<u16> {
    let s = b.get(o..o + 2).context("truncated resource data")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}

pub fn u32_at(b: &[u8], o: usize) -> Result<u32> {
    let s = b.get(o..o + 4).context("truncated resource data")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

/// One chunk: its type, its header (including the 8-byte common part) and
/// everything after the header.
#[derive(Clone, Copy)]
pub struct Chunk<'a> {
    pub ty: u16,
    pub header: &'a [u8],
    pub body: &'a [u8],
    /// The whole chunk (header and body); offsets inside a chunk count from
    /// its start.
    pub data: &'a [u8],
}

impl<'a> Chunk<'a> {
    /// The chunk at the start of `b`.
    pub fn at(b: &'a [u8]) -> Result<Self> {
        let ty = u16_at(b, 0)?;
        let header_size = u16_at(b, 2)? as usize;
        let size = u32_at(b, 4)? as usize;
        if header_size < 8 || header_size > size || size > b.len() {
            bail!("bad resource chunk (type {ty:#06x}, header {header_size}, size {size})");
        }
        Ok(Self {
            ty,
            header: &b[..header_size],
            body: &b[header_size..size],
            data: &b[..size],
        })
    }

    /// The chunks that follow one another in `body`.
    pub fn children(&self) -> Children<'a> {
        Children(self.body)
    }
}

pub struct Children<'a>(&'a [u8]);

impl<'a> Iterator for Children<'a> {
    type Item = Result<Chunk<'a>>;

    fn next(&mut self) -> Option<Self::Item> {
        // Trailing bytes shorter than a chunk header are padding.
        if self.0.len() < 8 {
            return None;
        }
        match Chunk::at(self.0) {
            Ok(c) => {
                self.0 = &self.0[c.data.len()..];
                Some(Ok(c))
            }
            Err(e) => {
                self.0 = &[];
                Some(Err(e))
            }
        }
    }
}

/// A string pool chunk. Strings are decoded on demand: a resource table's
/// pool can hold hundreds of thousands.
pub struct StringPool<'a> {
    data: &'a [u8],
    count: usize,
    utf8: bool,
    strings_start: usize,
    offsets: usize,
}

impl<'a> StringPool<'a> {
    pub fn new(c: Chunk<'a>) -> Result<Self> {
        if c.ty != STRING_POOL {
            bail!("expected a string pool, found chunk type {:#06x}", c.ty);
        }
        let count = u32_at(c.header, 8)? as usize;
        let flags = u32_at(c.header, 16)?;
        let strings_start = u32_at(c.header, 20)? as usize;
        let offsets = c.header.len();
        if offsets + count * 4 > c.data.len() {
            bail!("string pool offsets run past the chunk");
        }
        Ok(Self {
            data: c.data,
            count,
            utf8: flags & 0x100 != 0,
            strings_start,
            offsets,
        })
    }

    pub fn get(&self, i: u32) -> Result<String> {
        let i = i as usize;
        if i >= self.count {
            bail!("string index {i} out of range ({})", self.count);
        }
        let mut o = self.strings_start + u32_at(self.data, self.offsets + i * 4)? as usize;
        if self.utf8 {
            // UTF-16 length, then UTF-8 byte length; each 1 or 2 bytes.
            let mut len8 = || -> Result<usize> {
                let a = u8_at(self.data, o)? as usize;
                o += 1;
                if a & 0x80 == 0 {
                    return Ok(a);
                }
                let b = u8_at(self.data, o)? as usize;
                o += 1;
                Ok(((a & 0x7f) << 8) | b)
            };
            len8()?;
            let n = len8()?;
            let s = self
                .data
                .get(o..o + n)
                .context("string runs past its pool")?;
            Ok(String::from_utf8_lossy(s).into_owned())
        } else {
            // Length in UTF-16 units: 1 or 2 u16s.
            let a = u16_at(self.data, o)? as usize;
            o += 2;
            let n = if a & 0x8000 == 0 {
                a
            } else {
                let b = u16_at(self.data, o)? as usize;
                o += 2;
                ((a & 0x7fff) << 16) | b
            };
            let s = self
                .data
                .get(o..o + n * 2)
                .context("string runs past its pool")?;
            let units: Vec<u16> = s
                .as_chunks::<2>()
                .0
                .iter()
                .map(|u| u16::from_le_bytes(*u))
                .collect();
            Ok(String::from_utf16_lossy(&units))
        }
    }
}

/// A typed value (`Res_value`), with strings looked up in their pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Str(String),
    /// A resource ID (`@0x7f120001`), resolved through the resource table.
    Ref(u32),
    /// Decimal or hex integer, including enums and flags.
    Int(u32),
    Bool(bool),
    /// IEEE float bits, kept exact for equality.
    Float(u32),
    /// Android ARGB colour.
    Color(u32),
    /// Anything else (floats, dimensions, colours, theme attributes).
    Other,
}

impl Value {
    /// `ty` and `data` of a `Res_value`; `STRING` data indexes `pool`.
    pub fn new(ty: u8, data: u32, pool: &StringPool) -> Result<Self> {
        Ok(match ty {
            0x01 | 0x07 => Value::Ref(data),
            0x03 => Value::Str(pool.get(data)?),
            0x10 | 0x11 => Value::Int(data),
            0x12 => Value::Bool(data != 0),
            0x04 => Value::Float(data),
            0x1c..=0x1f => Value::Color(data),
            _ => Value::Other,
        })
    }
}

#[cfg(test)]
pub mod build {
    //! Writers for hand-made chunks in tests.

    pub fn chunk(ty: u16, header: &[u8], body: &[u8]) -> Vec<u8> {
        let hs = 8 + header.len();
        let mut v = vec![];
        v.extend(ty.to_le_bytes());
        v.extend((hs as u16).to_le_bytes());
        v.extend(((hs + body.len()) as u32).to_le_bytes());
        v.extend(header);
        v.extend(body);
        v
    }

    /// A string pool, UTF-8 or UTF-16.
    pub fn pool(strings: &[&str], utf8: bool) -> Vec<u8> {
        let mut data = vec![];
        let mut offsets = vec![];
        for s in strings {
            offsets.extend((data.len() as u32).to_le_bytes());
            if utf8 {
                for n in [s.encode_utf16().count(), s.len()] {
                    if n > 0x7f {
                        data.extend([0x80 | (n >> 8) as u8, n as u8]);
                    } else {
                        data.push(n as u8);
                    }
                }
                data.extend(s.as_bytes());
                data.push(0);
            } else {
                let u: Vec<u16> = s.encode_utf16().collect();
                if u.len() > 0x7fff {
                    data.extend((0x8000 | (u.len() >> 16) as u16).to_le_bytes());
                }
                data.extend((u.len() as u16).to_le_bytes());
                u.iter().for_each(|c| data.extend(c.to_le_bytes()));
                data.extend([0, 0]);
            }
        }
        while data.len() % 4 != 0 {
            data.push(0);
        }
        let mut h = vec![];
        h.extend((strings.len() as u32).to_le_bytes());
        h.extend(0u32.to_le_bytes());
        h.extend((if utf8 { 0x100u32 } else { 0 }).to_le_bytes());
        h.extend((28 + offsets.len() as u32).to_le_bytes());
        h.extend(0u32.to_le_bytes());
        offsets.extend(data);
        chunk(super::STRING_POOL, &h, &offsets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pools() {
        let long = "x".repeat(300);
        for utf8 in [true, false] {
            let b = build::pool(&["", "héllo", "日本語", &long], utf8);
            let p = StringPool::new(Chunk::at(&b).unwrap()).unwrap();
            assert_eq!(p.get(0).unwrap(), "");
            assert_eq!(p.get(1).unwrap(), "héllo");
            assert_eq!(p.get(2).unwrap(), "日本語");
            assert_eq!(p.get(3).unwrap(), long);
            assert!(p.get(4).is_err());
        }
    }

    #[test]
    fn malformed() {
        assert!(Chunk::at(&[1, 0, 8, 0]).is_err());
        // Size past the end of the buffer.
        assert!(Chunk::at(&[1, 0, 8, 0, 64, 0, 0, 0]).is_err());
        // Header larger than the chunk.
        assert!(Chunk::at(&[1, 0, 16, 0, 8, 0, 0, 0]).is_err());
        let mut b = build::pool(&["abc"], true);
        // The string's byte length (after the header and one offset) now
        // runs past the pool.
        b[28 + 4 + 1] = 0x7f;
        let p = StringPool::new(Chunk::at(&b).unwrap()).unwrap();
        assert!(p.get(0).is_err());
    }
}
