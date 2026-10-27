//! Wire protocol v1 between Android clients and the bridge (spec:
//! `docs/bridge.md`).
//!
//! One `SOCK_SEQPACKET` packet is one message: an 8-byte header
//! (`u16 type`, `u16 flags`, `u32 tag`) and a fixed little-endian payload.
//! Every request gets exactly one reply with the same tag. Everything here is
//! pure (no I/O), so it can be fuzzed directly; nothing in the decoder trusts
//! a length or count from the peer.

/// Protocol major version. A client with another major is refused.
pub const MAJOR: u16 = 1;
/// Protocol minor version (additive changes only).
pub const MINOR: u16 = 0;

/// Largest message either side sends. Longer packets are a protocol error.
pub const MAX_MSG: usize = 512;
/// Longest UTF-8 name or label, in bytes.
pub const MAX_NAME: usize = 96;
/// Header size.
pub const HDR: usize = 8;

/// Message types.
pub mod ty {
    pub const HELLO: u16 = 0x0001;
    pub const AUDIO_OPEN_OUTPUT: u16 = 0x0101;
    pub const AUDIO_CLOSE: u16 = 0x0102;
    pub const AUDIO_STANDBY: u16 = 0x0103;
    pub const AUDIO_SET_VOLUME: u16 = 0x0104;
    pub const AUDIO_SET_MASTER: u16 = 0x0105;
    pub const AUDIO_SET_METADATA: u16 = 0x0106;
    pub const REPLY: u16 = 0x8000;
}

/// Services a connection can bind to in HELLO.
pub mod service {
    pub const AUDIO: u32 = 1;
}

/// Sample formats (`OpenOutput.format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SampleFormat {
    S16Le = 1,
    F32Le = 2,
}

impl SampleFormat {
    pub fn bytes(self) -> u32 {
        match self {
            SampleFormat::S16Le => 2,
            SampleFormat::F32Le => 4,
        }
    }
}

/// Bounds on stream parameters.
pub mod limits {
    pub const MIN_RATE: u32 = 8000;
    pub const MAX_RATE: u32 = 192_000;
    pub const MAX_CHANNELS: u8 = 2;
    pub const MIN_PERIOD_FRAMES: u32 = 16;
    pub const MAX_PERIOD_FRAMES: u32 = 16384;
    pub const MIN_PERIODS: u32 = 2;
    pub const MAX_PERIODS: u32 = 16;
}

#[derive(Debug, Clone, PartialEq)]
pub struct OutputConfig {
    pub rate: u32,
    pub channels: u8,
    pub format: SampleFormat,
    pub period_frames: u32,
    pub periods: u32,
    pub name: String,
}

impl OutputConfig {
    pub fn frame_bytes(&self) -> u32 {
        self.channels as u32 * self.format.bytes()
    }
    pub fn capacity_frames(&self) -> u32 {
        // Both factors are bounded by decode(), so this cannot overflow.
        self.period_frames * self.periods
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    Hello {
        major: u16,
        minor: u16,
        service: u32,
        name: String,
    },
    OpenOutput(OutputConfig),
    Close {
        stream: u32,
    },
    Standby {
        stream: u32,
    },
    SetVolume {
        stream: u32,
        left: f32,
        right: f32,
    },
    SetMaster {
        volume: f32,
        muted: bool,
    },
    SetMetadata {
        stream: u32,
        usage: u32,
        label: String,
    },
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

impl DecodeError {
    /// The errno-style status a reply carries for this error.
    pub fn status(&self) -> i32 {
        match self {
            DecodeError::UnknownType(_) => -libc_errno::ENOSYS,
            _ => -libc_errno::EINVAL,
        }
    }
}

/// The errno values the protocol uses (Linux numbering, which both sides
/// share; spelled out so the decoder needs no libc).
pub mod libc_errno {
    pub const EPERM: i32 = 1;
    pub const ENOENT: i32 = 2;
    pub const EIO: i32 = 5;
    pub const EAGAIN: i32 = 11;
    pub const ENOMEM: i32 = 12;
    pub const EBUSY: i32 = 16;
    pub const EINVAL: i32 = 22;
    pub const ENOSYS: i32 = 38;
    pub const EPROTO: i32 = 71;
}

struct Rd<'a> {
    b: &'a [u8],
}

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], DecodeError> {
        if self.b.len() < n {
            return Err(DecodeError::Short);
        }
        let (h, t) = self.b.split_at(n);
        self.b = t;
        Ok(h)
    }
    fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn f32(&mut self) -> Result<f32, DecodeError> {
        Ok(f32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    /// `u16 len` then `len` bytes of UTF-8 without NUL or other control
    /// characters (they end up in desktop UI and C strings).
    fn name(&mut self) -> Result<String, DecodeError> {
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
    fn end(&self) -> Result<(), DecodeError> {
        if self.b.is_empty() {
            Ok(())
        } else {
            Err(DecodeError::Long)
        }
    }
}

/// Parses the header only: `(type, tag)`.
pub fn decode_header(buf: &[u8]) -> Result<(u16, u32), DecodeError> {
    if buf.len() < HDR {
        return Err(DecodeError::Short);
    }
    if buf.len() > MAX_MSG {
        return Err(DecodeError::Long);
    }
    let t = u16::from_le_bytes([buf[0], buf[1]]);
    let flags = u16::from_le_bytes([buf[2], buf[3]]);
    let tag = u32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    if flags != 0 {
        return Err(DecodeError::Flags);
    }
    Ok((t, tag))
}

fn volume(v: f32) -> Result<f32, DecodeError> {
    if v.is_finite() && (0.0..=1.0).contains(&v) {
        Ok(v)
    } else {
        Err(DecodeError::Invalid("volume outside [0, 1]"))
    }
}

/// Decodes one request message. On error the tag is returned too when the
/// header was readable, so the caller can answer it.
pub fn decode(buf: &[u8]) -> Result<(u32, Request), (Option<u32>, DecodeError)> {
    let (t, tag) = decode_header(buf).map_err(|e| (None, e))?;
    decode_body(t, &buf[HDR..])
        .map(|r| (tag, r))
        .map_err(|e| (Some(tag), e))
}

fn decode_body(t: u16, body: &[u8]) -> Result<Request, DecodeError> {
    use limits::*;
    let mut r = Rd { b: body };
    let req = match t {
        ty::HELLO => Request::Hello {
            major: r.u16()?,
            minor: r.u16()?,
            service: r.u32()?,
            name: r.name()?,
        },
        ty::AUDIO_OPEN_OUTPUT => {
            let rate = r.u32()?;
            let channels = r.u8()?;
            let format = match r.u8()? {
                1 => SampleFormat::S16Le,
                2 => SampleFormat::F32Le,
                _ => return Err(DecodeError::Invalid("format")),
            };
            if r.u16()? != 0 {
                return Err(DecodeError::Invalid("reserved"));
            }
            let period_frames = r.u32()?;
            let periods = r.u32()?;
            let name = r.name()?;
            if !(MIN_RATE..=MAX_RATE).contains(&rate) {
                return Err(DecodeError::Invalid("rate"));
            }
            if !(1..=MAX_CHANNELS).contains(&channels) {
                return Err(DecodeError::Invalid("channels"));
            }
            if !(MIN_PERIOD_FRAMES..=MAX_PERIOD_FRAMES).contains(&period_frames) {
                return Err(DecodeError::Invalid("period_frames"));
            }
            if !(MIN_PERIODS..=MAX_PERIODS).contains(&periods) {
                return Err(DecodeError::Invalid("periods"));
            }
            Request::OpenOutput(OutputConfig {
                rate,
                channels,
                format,
                period_frames,
                periods,
                name,
            })
        }
        ty::AUDIO_CLOSE => Request::Close { stream: r.u32()? },
        ty::AUDIO_STANDBY => Request::Standby { stream: r.u32()? },
        ty::AUDIO_SET_VOLUME => Request::SetVolume {
            stream: r.u32()?,
            left: volume(r.f32()?)?,
            right: volume(r.f32()?)?,
        },
        ty::AUDIO_SET_MASTER => Request::SetMaster {
            volume: volume(r.f32()?)?,
            muted: match r.u8()? {
                0 => false,
                1 => true,
                _ => return Err(DecodeError::Invalid("bool")),
            },
        },
        ty::AUDIO_SET_METADATA => Request::SetMetadata {
            stream: r.u32()?,
            usage: r.u32()?,
            label: r.name()?,
        },
        other => return Err(DecodeError::UnknownType(other)),
    };
    r.end()?;
    Ok(req)
}

/// Little-endian message builder.
#[derive(Default)]
pub struct Wr(pub Vec<u8>);

impl Wr {
    pub fn header(t: u16, tag: u32) -> Wr {
        let mut w = Wr(Vec::with_capacity(64));
        w.u16(t).u16(0).u32(tag);
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
}

/// Encodes a request (the client side; used by tests and as the reference
/// for the C client in `audio/hal/bridge_client.c`).
pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let w = match req {
        Request::Hello {
            major,
            minor,
            service,
            name,
        } => {
            let mut w = Wr::header(ty::HELLO, tag);
            w.u16(*major).u16(*minor).u32(*service).name(name);
            w
        }
        Request::OpenOutput(c) => {
            let mut w = Wr::header(ty::AUDIO_OPEN_OUTPUT, tag);
            w.u32(c.rate)
                .u8(c.channels)
                .u8(c.format as u8)
                .u16(0)
                .u32(c.period_frames)
                .u32(c.periods)
                .name(&c.name);
            w
        }
        Request::Close { stream } => {
            let mut w = Wr::header(ty::AUDIO_CLOSE, tag);
            w.u32(*stream);
            w
        }
        Request::Standby { stream } => {
            let mut w = Wr::header(ty::AUDIO_STANDBY, tag);
            w.u32(*stream);
            w
        }
        Request::SetVolume {
            stream,
            left,
            right,
        } => {
            let mut w = Wr::header(ty::AUDIO_SET_VOLUME, tag);
            w.u32(*stream).f32(*left).f32(*right);
            w
        }
        Request::SetMaster { volume, muted } => {
            let mut w = Wr::header(ty::AUDIO_SET_MASTER, tag);
            w.f32(*volume).u8(*muted as u8);
            w
        }
        Request::SetMetadata {
            stream,
            usage,
            label,
        } => {
            let mut w = Wr::header(ty::AUDIO_SET_METADATA, tag);
            w.u32(*stream).u32(*usage).name(label);
            w
        }
    };
    w.0
}

/// A reply: `REPLY` header, `i32 status` (0 or -errno), then a body.
pub fn reply(tag: u32, status: i32, body: &[u8]) -> Vec<u8> {
    let mut w = Wr::header(ty::REPLY, tag);
    w.i32(status);
    w.0.extend_from_slice(body);
    w.0
}

/// HELLO reply body.
pub fn hello_body() -> Vec<u8> {
    let mut w = Wr::default();
    w.u16(MAJOR).u16(MINOR);
    w.0
}

/// OPEN_OUTPUT reply body. The reply also carries three fds, in order: the
/// ring memfd, the "data" eventfd (client signals) and the "space" eventfd
/// (bridge signals).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenOutputReply {
    pub stream: u32,
    pub capacity_frames: u32,
    pub data_offset: u32,
    pub map_size: u32,
    pub latency_us: u32,
}

impl OpenOutputReply {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Wr::default();
        w.u32(self.stream)
            .u32(self.capacity_frames)
            .u32(self.data_offset)
            .u32(self.map_size)
            .u32(self.latency_us);
        w.0
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut r = Rd { b };
        let v = OpenOutputReply {
            stream: r.u32().ok()?,
            capacity_frames: r.u32().ok()?,
            data_offset: r.u32().ok()?,
            map_size: r.u32().ok()?,
            latency_us: r.u32().ok()?,
        };
        r.end().ok()?;
        Some(v)
    }
}

/// Splits a reply into `(tag, status, body)`.
pub fn decode_reply(buf: &[u8]) -> Option<(u32, i32, &[u8])> {
    let (t, tag) = decode_header(buf).ok()?;
    if t != ty::REPLY || buf.len() < HDR + 4 {
        return None;
    }
    let status = i32::from_le_bytes(buf[HDR..HDR + 4].try_into().unwrap());
    Some((tag, status, &buf[HDR + 4..]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn out(rate: u32, ch: u8, pf: u32, p: u32) -> Request {
        Request::OpenOutput(OutputConfig {
            rate,
            channels: ch,
            format: SampleFormat::S16Le,
            period_frames: pf,
            periods: p,
            name: "primary".into(),
        })
    }

    #[test]
    fn roundtrip_examples() {
        let reqs = [
            Request::Hello {
                major: 1,
                minor: 0,
                service: service::AUDIO,
                name: "audio.primary.linuxapk".into(),
            },
            out(48000, 2, 480, 2),
            Request::Close { stream: 7 },
            Request::Standby { stream: 7 },
            Request::SetVolume {
                stream: 1,
                left: 0.5,
                right: 1.0,
            },
            Request::SetMaster {
                volume: 0.25,
                muted: true,
            },
            Request::SetMetadata {
                stream: 1,
                usage: 14,
                label: "Game".into(),
            },
        ];
        for (i, r) in reqs.iter().enumerate() {
            let b = encode(i as u32, r);
            assert!(b.len() <= MAX_MSG);
            assert_eq!(decode(&b).unwrap(), (i as u32, r.clone()));
        }
    }

    #[test]
    fn rejects_bad_values() {
        for r in [
            out(7999, 2, 480, 2),
            out(48000, 0, 480, 2),
            out(48000, 3, 480, 2),
            out(48000, 2, 15, 2),
            out(48000, 2, 480, 1),
            out(48000, 2, 480, 17),
            out(192_001, 2, 480, 2),
        ] {
            let e = decode(&encode(5, &r)).unwrap_err();
            assert_eq!(e.0, Some(5));
            assert!(matches!(e.1, DecodeError::Invalid(_)), "{r:?}: {e:?}");
        }
        let nan = encode(
            1,
            &Request::SetVolume {
                stream: 0,
                left: f32::NAN,
                right: 0.0,
            },
        );
        assert!(decode(&nan).is_err());
        let loud = encode(
            1,
            &Request::SetMaster {
                volume: 1.5,
                muted: false,
            },
        );
        assert!(decode(&loud).is_err());
        let mut ctl = encode(
            1,
            &Request::Hello {
                major: 1,
                minor: 0,
                service: 1,
                name: "a\nb".into(),
            },
        );
        assert!(decode(&ctl).is_err());
        // Trailing byte.
        ctl = encode(1, &Request::Close { stream: 1 });
        ctl.push(0);
        assert_eq!(decode(&ctl).unwrap_err().1, DecodeError::Long);
        // Name length pointing past the end.
        let mut h = encode(
            1,
            &Request::Hello {
                major: 1,
                minor: 0,
                service: 1,
                name: "abc".into(),
            },
        );
        let n = h.len();
        h[n - 5] = 0xff;
        assert!(decode(&h).is_err());
        // Flags and unknown types.
        let mut f = encode(1, &Request::Close { stream: 1 });
        f[2] = 1;
        assert_eq!(decode(&f).unwrap_err().1, DecodeError::Flags);
        let mut u = encode(9, &Request::Close { stream: 1 });
        u[0] = 0x77;
        assert_eq!(
            decode(&u).unwrap_err(),
            (Some(9), DecodeError::UnknownType(0x0177))
        );
        assert_eq!(
            decode(&vec![0; MAX_MSG + 1]).unwrap_err().1,
            DecodeError::Long
        );
    }

    #[test]
    fn name_truncates_on_char_boundary() {
        let long = "é".repeat(100);
        let b = encode(
            0,
            &Request::Hello {
                major: 1,
                minor: 0,
                service: 1,
                name: long,
            },
        );
        let Ok((_, Request::Hello { name, .. })) = decode(&b) else {
            panic!()
        };
        assert!(name.len() <= MAX_NAME && name.chars().all(|c| c == 'é'));
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20000))]

        // The decoder never panics and never accepts trailing garbage.
        #[test]
        fn decode_arbitrary_bytes(b in proptest::collection::vec(any::<u8>(), 0..600)) {
            if let Ok((_, req)) = decode(&b) {
                // Anything accepted re-encodes to the same bytes.
                let tag = u32::from_le_bytes(b[4..8].try_into().unwrap());
                prop_assert_eq!(encode(tag, &req), b);
            }
        }

        // Mutating one byte of a valid message never panics.
        #[test]
        fn decode_mutated(rate in 0u32..300_000, ch in 0u8..4, pf in 0u32..20000, p in 0u32..20,
                          pos in 0usize..64, val in any::<u8>()) {
            let mut b = encode(3, &out(rate, ch, pf, p));
            let i = pos % b.len();
            b[i] = val;
            let _ = decode(&b);
        }

        #[test]
        fn decoded_output_configs_are_bounded(rate in any::<u32>(), ch in any::<u8>(), pf in any::<u32>(), p in any::<u32>()) {
            if let Ok((_, Request::OpenOutput(c))) = decode(&encode(0, &out(rate, ch, pf, p))) {
                prop_assert!(c.capacity_frames() as u64 * c.frame_bytes() as u64 <= 16384 * 16 * 8);
            }
        }
    }
}
