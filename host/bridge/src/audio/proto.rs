//! Audio service messages (id 1, types 0x0100..=0x01ff). The client is
//! `android/vendor/audio`'s module (`audio_bridge`); framing and HELLO are
//! in `docs/bridge-protocol.md`.
//!
//! | Type     | Request      | Body                                                                                                                                               | Reply                                                                                                                                          |
//! | -------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------- |
//! | `0x0101` | OPEN_OUTPUT  | `u32 rate` (8000–192000), `u8 channels` (1–2), `u8 format` (1 s16le, 2 f32le), `u16 0`, `u32 period_frames` (16–16384), `u32 periods` (2–16), name | `u32 stream`, `u32 capacity_frames`, `u32 data_offset`, `u32 map_size`, `u32 latency_us`; fds: ring memfd, data eventfd, space pipe (read end) |
//! | `0x0107` | OPEN_INPUT | Same PCM config as OPEN_OUTPUT | Same reply and fds; bridge produces, client consumes |
//! | `0x0102` | CLOSE        | `u32 stream`                                                                                                                                       |                                                                                                                                                |
//! | `0x0103` | STANDBY      | `u32 stream`                                                                                                                                       | Once the ring drains and PipeWire underruns, the stream is corked so the sink can suspend                                                     |
//! | `0x0104` | SET_VOLUME   | `u32 stream`, `f32 left`, `f32 right` (each 0–1)                                                                                                   |                                                                                                                                                |
//! | `0x0106` | SET_METADATA | `u32 stream`, `u32 usage` (Android `audio_usage_t`), label (empty: derived from usage)                                                             | Sets the PipeWire `media.name` and `media.role`                                                                                                |
//!
//! Limits: 8 streams and 8 MiB of rings per connection; 32 streams and 32
//! connections in all. Over a stream limit OPEN_OUTPUT gets `-EBUSY`; over
//! the connection limit the connection is closed. Unknown streams get
//! `-ENOENT`.
//!
//! The PCM goes through a shared ring, not messages: its layout and who
//! writes what are in [`super::ring`].

use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const OPEN_OUTPUT: u16 = 0x0101;
    pub const OPEN_INPUT: u16 = 0x0107;
    pub const CLOSE: u16 = 0x0102;
    pub const STANDBY: u16 = 0x0103;
    pub const SET_VOLUME: u16 = 0x0104;
    pub const SET_METADATA: u16 = 0x0106;
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
pub struct StreamConfig {
    pub rate: u32,
    pub channels: u8,
    pub format: SampleFormat,
    pub period_frames: u32,
    pub periods: u32,
    pub name: String,
}

impl StreamConfig {
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
    OpenOutput(StreamConfig),
    OpenInput(StreamConfig),
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
    SetMetadata {
        stream: u32,
        usage: u32,
        label: String,
    },
}

/// Decodes the body of an audio message of type `t`.
pub fn decode(t: u16, body: &[u8]) -> Result<Request, DecodeError> {
    use limits::*;
    let mut r = Reader::new(body);
    let req = match t {
        ty::OPEN_OUTPUT | ty::OPEN_INPUT => {
            let rate = r.u32_in(MIN_RATE..=MAX_RATE, "rate")?;
            let channels = r.u8()?;
            let format = match r.u8()? {
                1 => SampleFormat::S16Le,
                2 => SampleFormat::F32Le,
                _ => return Err(DecodeError::Invalid("format")),
            };
            if r.u16()? != 0 {
                return Err(DecodeError::Invalid("reserved"));
            }
            let period_frames = r.u32_in(MIN_PERIOD_FRAMES..=MAX_PERIOD_FRAMES, "period_frames")?;
            let periods = r.u32_in(MIN_PERIODS..=MAX_PERIODS, "periods")?;
            let name = r.name()?;
            if !(1..=MAX_CHANNELS).contains(&channels) {
                return Err(DecodeError::Invalid("channels"));
            }
            let cfg = StreamConfig {
                rate,
                channels,
                format,
                period_frames,
                periods,
                name,
            };
            if t == ty::OPEN_INPUT {
                Request::OpenInput(cfg)
            } else {
                Request::OpenOutput(cfg)
            }
        }
        ty::CLOSE => Request::Close { stream: r.u32()? },
        ty::STANDBY => Request::Standby { stream: r.u32()? },
        ty::SET_VOLUME => Request::SetVolume {
            stream: r.u32()?,
            left: r.unit_f32()?,
            right: r.unit_f32()?,
        },
        ty::SET_METADATA => Request::SetMetadata {
            stream: r.u32()?,
            usage: r.u32()?,
            label: r.name()?,
        },
        other => return Err(DecodeError::UnknownType(other)),
    };
    r.end()?;
    Ok(req)
}

/// Encodes a request (the client side: tests, the example client, and the
/// reference for `guest/bridge-client/audio.c`).
pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let w = match req {
        Request::OpenOutput(c) | Request::OpenInput(c) => {
            let mut w = Writer::message(
                if matches!(req, Request::OpenInput(_)) {
                    ty::OPEN_INPUT
                } else {
                    ty::OPEN_OUTPUT
                },
                tag,
            );
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
            let mut w = Writer::message(ty::CLOSE, tag);
            w.u32(*stream);
            w
        }
        Request::Standby { stream } => {
            let mut w = Writer::message(ty::STANDBY, tag);
            w.u32(*stream);
            w
        }
        Request::SetVolume {
            stream,
            left,
            right,
        } => {
            let mut w = Writer::message(ty::SET_VOLUME, tag);
            w.u32(*stream).f32(*left).f32(*right);
            w
        }
        Request::SetMetadata {
            stream,
            usage,
            label,
        } => {
            let mut w = Writer::message(ty::SET_METADATA, tag);
            w.u32(*stream).u32(*usage).name(label);
            w
        }
    };
    w.into_vec()
}

/// OPEN_OUTPUT / OPEN_INPUT reply body. The reply also carries three fds, in order: the
/// ring memfd, the "data" eventfd (client signals) and the "space" eventfd
/// (bridge signals). Input uses the same notification direction: the client
/// signals a read request, the bridge signals data availability or death.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenReply {
    pub stream: u32,
    pub capacity_frames: u32,
    pub data_offset: u32,
    pub map_size: u32,
    pub latency_us: u32,
}

impl OpenReply {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(self.stream)
            .u32(self.capacity_frames)
            .u32(self.data_offset)
            .u32(self.map_size)
            .u32(self.latency_us);
        w.into_vec()
    }
    pub fn decode(b: &[u8]) -> Option<Self> {
        let mut r = Reader::new(b);
        let v = OpenReply {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::wire::{HDR, decode_header};

    fn out(rate: u32, ch: u8, pf: u32, p: u32) -> Request {
        Request::OpenOutput(StreamConfig {
            rate,
            channels: ch,
            format: SampleFormat::S16Le,
            period_frames: pf,
            periods: p,
            name: "primary".into(),
        })
    }

    fn roundtrip(b: &[u8]) -> Result<Request, DecodeError> {
        let h = decode_header(b)?;
        decode(h.ty, &b[HDR..])
    }

    #[test]
    fn roundtrip_examples() {
        let Request::OpenOutput(cfg) = out(44100, 1, 441, 4) else {
            unreachable!()
        };
        let reqs = [
            Request::OpenInput(cfg),
            out(48000, 2, 480, 2),
            Request::Close { stream: 7 },
            Request::Standby { stream: 7 },
            Request::SetVolume {
                stream: 1,
                left: 0.5,
                right: 1.0,
            },
            Request::SetMetadata {
                stream: 1,
                usage: 14,
                label: "Game".into(),
            },
        ];
        for (i, r) in reqs.iter().enumerate() {
            let b = encode(i as u32, r);
            assert!(b.len() <= crate::ipc::wire::MAX_MSG);
            assert_eq!(decode_header(&b).unwrap().tag, i as u32);
            assert_eq!(roundtrip(&b).unwrap(), r.clone());
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
            assert!(
                matches!(roundtrip(&encode(5, &r)), Err(DecodeError::Invalid(_))),
                "{r:?}"
            );
            let Request::OpenOutput(cfg) = r else {
                unreachable!()
            };
            assert!(matches!(
                roundtrip(&encode(5, &Request::OpenInput(cfg))),
                Err(DecodeError::Invalid(_))
            ));
        }
        assert!(
            roundtrip(&encode(
                1,
                &Request::SetVolume {
                    stream: 0,
                    left: f32::NAN,
                    right: 0.0
                }
            ))
            .is_err()
        );
        let mut b = encode(1, &Request::Close { stream: 1 });
        b.push(0);
        assert_eq!(roundtrip(&b), Err(DecodeError::Long));
        assert_eq!(decode(0x0177, &[]), Err(DecodeError::UnknownType(0x0177)));
    }
}
