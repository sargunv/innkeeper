//! Framing and untrusted-input helpers shared by every service.
//!
//! One `SOCK_SEQPACKET` packet is one message: an 8-byte header (`u16 type`,
//! `u16 flags` (must be 0), `u32 tag`) and a little-endian body of at most
//! `MAX_MSG - HDR` bytes. Requests get exactly one REPLY with the same tag;
//! the bridge may also send events (types `EVENT_MIN..=EVENT_MAX`, tag 0).
//!
//! Decoding goes through [`Reader`], which never trusts a length or count
//! from the peer: every read is bounds-checked, names are bounded UTF-8
//! without control characters, and [`Reader::end`] rejects trailing bytes.
//! Everything here is pure, so it can be fuzzed directly.

/// Largest message either side sends. Longer packets end the connection.
pub const MAX_MSG: usize = 512;
/// Longest UTF-8 name or label, in bytes.
pub const MAX_NAME: usize = 96;
/// Header size.
pub const HDR: usize = 8;
/// Reply type (requests use `0x0001..EVENT_MIN`).
pub const REPLY: u16 = 0x8000;
/// Unsolicited bridge-to-client messages (tag 0).
pub const EVENT_MIN: u16 = 0x4000;
pub const EVENT_MAX: u16 = 0x7fff;

/// Errno values used in reply statuses (Linux numbering, which both sides
/// share; spelled out so decoding needs no libc).
pub mod errno {
    pub const EPERM: i32 = 1;
    pub const ENOENT: i32 = 2;
    pub const EIO: i32 = 5;
    pub const EACCES: i32 = 13;
    pub const EAGAIN: i32 = 11;
    pub const ENOMEM: i32 = 12;
    pub const EBUSY: i32 = 16;
    pub const EEXIST: i32 = 17;
    pub const EXDEV: i32 = 18;
    pub const ENOTDIR: i32 = 20;
    pub const EINVAL: i32 = 22;
    pub const ENAMETOOLONG: i32 = 36;
    pub const ENOSYS: i32 = 38;
    pub const EPROTO: i32 = 71;
    pub const ECANCELED: i32 = 125;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    /// Shorter than its type requires, or shorter than a header.
    Short,
    /// Longer than its type allows, or longer than MAX_MSG.
    Long,
    /// Reserved header flags were set.
    Flags,
    /// Unknown message type (forward compatibility: answer ENOSYS).
    UnknownType(u16),
    /// A field was out of range or malformed.
    Invalid(&'static str),
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for DecodeError {}

impl DecodeError {
    /// The status a reply carries for this error.
    pub fn status(&self) -> i32 {
        match self {
            DecodeError::UnknownType(_) => -errno::ENOSYS,
            _ => -errno::EINVAL,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub ty: u16,
    pub tag: u32,
}

/// Parses and checks a header; the body is `&buf[HDR..]`.
pub fn decode_header(buf: &[u8]) -> Result<Header, DecodeError> {
    if buf.len() < HDR {
        return Err(DecodeError::Short);
    }
    if buf.len() > MAX_MSG {
        return Err(DecodeError::Long);
    }
    let mut r = Reader::new(&buf[..HDR]);
    let (ty, flags, tag) = (r.u16()?, r.u16()?, r.u32()?);
    if flags != 0 {
        return Err(DecodeError::Flags);
    }
    Ok(Header { ty, tag })
}

/// Bounds-checked little-endian reader over an untrusted body.
pub struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Reader { b }
    }
    pub fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.b.len() < n {
            return Err(DecodeError::Short);
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }
    fn arr<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        Ok(self.take(N)?.try_into().expect("take returned N bytes"))
    }
    pub fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.arr::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.arr()?))
    }
    pub fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.arr()?))
    }
    pub fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_le_bytes(self.arr()?))
    }
    pub fn u64(&mut self) -> Result<u64, DecodeError> {
        Ok(u64::from_le_bytes(self.arr()?))
    }
    pub fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_le_bytes(self.arr()?))
    }
    /// A `u8` that must be 0 or 1.
    pub fn bool(&mut self) -> Result<bool, DecodeError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(DecodeError::Invalid("bool")),
        }
    }
    /// An f32 in `[0, 1]` (NaN and infinities rejected).
    pub fn unit_f32(&mut self) -> Result<f32, DecodeError> {
        let v = self.f32()?;
        if v.is_finite() && (0.0..=1.0).contains(&v) {
            Ok(v)
        } else {
            Err(DecodeError::Invalid("value outside [0, 1]"))
        }
    }
    /// A `u32` in `range`.
    pub fn u32_in(
        &mut self,
        range: std::ops::RangeInclusive<u32>,
        what: &'static str,
    ) -> Result<u32, DecodeError> {
        let v = self.u32()?;
        if range.contains(&v) {
            Ok(v)
        } else {
            Err(DecodeError::Invalid(what))
        }
    }
    /// `u16 len` then `len` bytes of UTF-8, at most MAX_NAME, without NUL or
    /// other control characters (names end up in desktop UI and C strings).
    pub fn name(&mut self) -> Result<String, DecodeError> {
        let n = self.u16()? as usize;
        if n > MAX_NAME {
            return Err(DecodeError::Invalid("name too long"));
        }
        let s = std::str::from_utf8(self.take(n)?)
            .map_err(|_| DecodeError::Invalid("name not UTF-8"))?;
        if s.chars().any(char::is_control) {
            return Err(DecodeError::Invalid("control character in name"));
        }
        Ok(s.to_owned())
    }
    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }
    /// Bytes left to read.
    pub fn remaining(&self) -> usize {
        self.b.len()
    }
    /// Fails unless the whole body was consumed.
    pub fn end(&self) -> Result<(), DecodeError> {
        if self.b.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::Long)
        }
    }
}

/// Little-endian message builder.
#[derive(Default)]
pub struct Writer(pub Vec<u8>);

impl Writer {
    pub fn message(ty: u16, tag: u32) -> Writer {
        let mut w = Writer(Vec::with_capacity(64));
        w.u16(ty).u16(0).u32(tag);
        w
    }
    pub fn u8(&mut self, v: u8) -> &mut Self {
        self.0.push(v);
        self
    }
    pub fn u16(&mut self, v: u16) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn i32(&mut self, v: i32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn u64(&mut self, v: u64) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    pub fn f32(&mut self, v: f32) -> &mut Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    /// Writes a name, truncated to MAX_NAME bytes on a char boundary.
    pub fn name(&mut self, s: &str) -> &mut Self {
        let mut n = s.len().min(MAX_NAME);
        while !s.is_char_boundary(n) {
            n -= 1;
        }
        self.u16(n as u16);
        self.0.extend_from_slice(&s.as_bytes()[..n]);
        self
    }
    pub fn into_vec(self) -> Vec<u8> {
        self.0
    }
}

/// A reply: REPLY header, `i32 status` (0 or -errno), then a body.
pub fn reply(tag: u32, status: i32, body: &[u8]) -> Vec<u8> {
    let mut w = Writer::message(REPLY, tag);
    w.i32(status);
    w.0.extend_from_slice(body);
    w.0
}

/// Splits a reply into `(tag, status, body)`.
pub fn decode_reply(buf: &[u8]) -> Option<(u32, i32, &[u8])> {
    let h = decode_header(buf).ok()?;
    if h.ty != REPLY {
        return None;
    }
    let status = Reader::new(&buf[HDR..]).i32().ok()?;
    Some((h.tag, status, &buf[HDR + 4..]))
}

/// Truncates `s` to at most `max` bytes on a char boundary.
pub fn truncate(s: &str, max: usize) -> &str {
    let mut n = s.len().min(max);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s[..n]
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// `wire-fixtures.txt`, which the system app's tests also check: each
    /// message built from its fields must match the hex, and read back.
    #[test]
    fn shared_fixtures() {
        fn int(v: &str) -> i64 {
            match v.strip_prefix("0x") {
                Some(h) => i64::from_str_radix(h, 16).unwrap(),
                None => v.parse().unwrap(),
            }
        }
        fn text(v: &str) -> &str {
            v.strip_prefix('"').unwrap().strip_suffix('"').unwrap()
        }
        let fixtures = include_str!("wire-fixtures.txt");
        let mut lines = fixtures
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'));
        let mut cases = 0;
        while let Some(fields) = lines.next() {
            let hex = lines.next().expect("a hex line after each message");
            let want: Vec<u8> = (0..hex.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
                .collect();
            let fields: Vec<(&str, &str)> = fields
                .split("; ")
                .map(|f| f.split_once(' ').unwrap_or((f, "")))
                .collect();
            let mut w = Writer::default();
            for &(op, v) in &fields {
                match op {
                    "message" => {
                        let (ty, tag) = v.split_once(' ').unwrap();
                        w.u16(int(ty) as u16).u16(0).u32(int(tag) as u32);
                    }
                    "u8" => _ = w.u8(int(v) as u8),
                    "u16" => _ = w.u16(int(v) as u16),
                    "u32" => _ = w.u32(int(v) as u32),
                    "i32" => _ = w.i32(int(v) as i32),
                    "u64" => _ = w.u64(int(v) as u64),
                    "f32" => _ = w.f32(v.parse().unwrap()),
                    "name" => _ = w.name(text(v)),
                    _ => panic!("unknown field {op}"),
                }
            }
            assert_eq!(w.0, want, "{fields:?}");

            let h = decode_header(&want).unwrap();
            let mut r = Reader::new(&want[HDR..]);
            for &(op, v) in &fields {
                match op {
                    "message" => assert_eq!(format!("0x{:04x} {}", h.ty, h.tag), v),
                    "u8" => assert_eq!(r.u8().unwrap() as i64, int(v)),
                    "u16" => assert_eq!(r.u16().unwrap() as i64, int(v)),
                    "u32" => assert_eq!(r.u32().unwrap() as i64, int(v)),
                    "i32" => assert_eq!(r.i32().unwrap() as i64, int(v)),
                    "u64" => assert_eq!(r.u64().unwrap() as i64, int(v)),
                    "f32" => assert_eq!(r.f32().unwrap(), v.parse::<f32>().unwrap()),
                    _ => assert_eq!(r.name().unwrap(), truncate(text(v), MAX_NAME)),
                }
            }
            r.end().unwrap();
            cases += 1;
        }
        assert!(cases > 0);
    }

    #[test]
    fn header_checks() {
        assert_eq!(
            decode_header(&[1, 0, 0, 0, 7, 0, 0, 0]),
            Ok(Header { ty: 1, tag: 7 })
        );
        assert_eq!(
            decode_header(&[1, 0, 1, 0, 7, 0, 0, 0]),
            Err(DecodeError::Flags)
        );
        assert_eq!(decode_header(&[1, 0, 0]), Err(DecodeError::Short));
        assert_eq!(decode_header(&[0; MAX_MSG + 1]), Err(DecodeError::Long));
    }

    #[test]
    fn names() {
        let mut w = Writer::default();
        w.name("é".repeat(100).as_str());
        let n = Reader::new(&w.0).name().unwrap();
        assert!(n.len() <= MAX_NAME && n.chars().all(|c| c == 'é'));
        for bad in [
            &b"\x03\x00a\nb"[..],
            b"\x02\x00\xff\xfe",
            b"\x05\x00ab",
            b"\x61\x00",
        ] {
            assert!(Reader::new(bad).name().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn values() {
        let mut w = Writer::default();
        w.f32(f32::NAN).f32(1.5).f32(0.5).u8(2).u32(9);
        let mut r = Reader::new(&w.0);
        assert!(r.unit_f32().is_err());
        assert!(r.unit_f32().is_err());
        assert_eq!(r.unit_f32(), Ok(0.5));
        assert!(r.bool().is_err());
        assert!(r.u32_in(1..=8, "x").is_err());
        assert!(r.end().is_ok());
        assert_eq!(
            decode_reply(&reply(3, -22, b"xy")),
            Some((3, -22, &b"xy"[..]))
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(5000))]
        #[test]
        fn reader_never_panics(b in proptest::collection::vec(any::<u8>(), 0..64)) {
            let mut r = Reader::new(&b);
            let _ = (r.name(), r.u64(), r.bool(), r.unit_f32(), r.end());
            let _ = decode_header(&b);
            let _ = decode_reply(&b);
        }
    }
}
