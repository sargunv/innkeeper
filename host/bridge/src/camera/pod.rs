//! The few SPA POD values the camera stream needs: the `EnumFormat` it
//! offers PipeWire and the `Format` PipeWire settles on. The encoding is
//! SPA's stable wire format (`spa/pod/pod.h`). Each value is a `u32 size`, a
//! `u32 type` and a body padded to 8 bytes, in native byte order.

/// SPA type ids (`spa/utils/type.h`).
mod ty {
    pub const ID: u32 = 3;
    pub const INT: u32 = 4;
    pub const RECTANGLE: u32 = 10;
    pub const FRACTION: u32 = 11;
    pub const OBJECT: u32 = 15;
    pub const CHOICE: u32 = 19;
    pub const OBJECT_FORMAT: u32 = 0x40003;
}

/// `SPA_PARAM_EnumFormat` and `SPA_PARAM_Format`.
pub const PARAM_ENUM_FORMAT: u32 = 3;
pub const PARAM_FORMAT: u32 = 4;

/// `enum spa_format` keys.
mod key {
    pub const MEDIA_TYPE: u32 = 1;
    pub const MEDIA_SUBTYPE: u32 = 2;
    pub const VIDEO_FORMAT: u32 = 0x20001;
    pub const VIDEO_SIZE: u32 = 0x20003;
    pub const VIDEO_FRAMERATE: u32 = 0x20004;
}

const MEDIA_TYPE_VIDEO: u32 = 2;
const MEDIA_SUBTYPE_RAW: u32 = 1;

const CHOICE_NONE: u32 = 0;
const CHOICE_RANGE: u32 = 1;
const CHOICE_ENUM: u32 = 3;

/// The raw video formats the bridge converts (`enum spa_video_format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum VideoFormat {
    I420 = 2,
    Yv12 = 3,
    Yuy2 = 4,
    Uyvy = 5,
    Nv12 = 23,
    Nv21 = 24,
}

impl VideoFormat {
    /// Packed 4:2:2 first: it is what UVC webcams send uncompressed.
    pub const OFFERED: [VideoFormat; 6] = [
        VideoFormat::Yuy2,
        VideoFormat::Nv12,
        VideoFormat::I420,
        VideoFormat::Yv12,
        VideoFormat::Uyvy,
        VideoFormat::Nv21,
    ];

    pub fn from_id(id: u32) -> Option<VideoFormat> {
        Self::OFFERED.into_iter().find(|f| *f as u32 == id)
    }
}

/// The format PipeWire fixated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    pub format: VideoFormat,
    pub width: u32,
    pub height: u32,
}

/// The largest frame the bridge accepts, which a frame ring slot holds.
pub const MAX_WIDTH: u32 = 1920;
pub const MAX_HEIGHT: u32 = 1080;

struct Builder(Vec<u8>);

impl Builder {
    fn u32(&mut self, v: u32) -> &mut Self {
        self.0.extend_from_slice(&v.to_ne_bytes());
        self
    }

    fn pad(&mut self) {
        while !self.0.len().is_multiple_of(8) {
            self.0.push(0);
        }
    }

    /// Writes a POD header with a placeholder size and returns where the
    /// size goes. [`Builder::close`] fills it in.
    fn open(&mut self, ty: u32) -> usize {
        let at = self.0.len();
        self.u32(0).u32(ty);
        at
    }

    fn close(&mut self, at: usize) {
        let size = (self.0.len() - at - 8) as u32;
        self.0[at..at + 4].copy_from_slice(&size.to_ne_bytes());
        self.pad();
    }

    fn prop(&mut self, key: u32) -> &mut Self {
        self.u32(key).u32(0)
    }

    fn id(&mut self, v: u32) {
        self.u32(4).u32(ty::ID).u32(v);
        self.pad();
    }

    /// A choice of `values`, each `child_size` bytes of type `child`.
    fn choice(&mut self, kind: u32, child: u32, child_size: u32, values: &[&[u32]]) {
        let at = self.open(ty::CHOICE);
        self.u32(kind).u32(0).u32(child_size).u32(child);
        for v in values {
            for w in *v {
                self.u32(*w);
            }
        }
        self.close(at);
    }
}

/// The `EnumFormat` the stream offers: raw video in the formats the bridge
/// converts, preferring `width`x`height` at 30 fps.
pub fn enum_format(width: u32, height: u32) -> Vec<u8> {
    let mut b = Builder(Vec::with_capacity(256));
    let at = b.open(ty::OBJECT);
    b.u32(ty::OBJECT_FORMAT).u32(PARAM_ENUM_FORMAT);
    b.prop(key::MEDIA_TYPE).id(MEDIA_TYPE_VIDEO);
    b.prop(key::MEDIA_SUBTYPE).id(MEDIA_SUBTYPE_RAW);
    b.prop(key::VIDEO_FORMAT);
    let first = VideoFormat::OFFERED[0] as u32;
    let mut ids: Vec<[u32; 1]> = vec![[first]];
    ids.extend(VideoFormat::OFFERED.iter().map(|f| [*f as u32]));
    let ids: Vec<&[u32]> = ids.iter().map(|v| &v[..]).collect();
    b.choice(CHOICE_ENUM, ty::ID, 4, &ids);
    b.prop(key::VIDEO_SIZE);
    let (w, h) = (width.clamp(1, MAX_WIDTH), height.clamp(1, MAX_HEIGHT));
    b.choice(
        CHOICE_RANGE,
        ty::RECTANGLE,
        8,
        &[&[w, h], &[1, 1], &[MAX_WIDTH, MAX_HEIGHT]],
    );
    b.prop(key::VIDEO_FRAMERATE);
    b.choice(
        CHOICE_RANGE,
        ty::FRACTION,
        8,
        &[&[30, 1], &[0, 1], &[120, 1]],
    );
    b.close(at);
    b.0
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn u32(&mut self) -> Option<u32> {
        let (h, t) = self.0.split_first_chunk::<4>()?;
        self.0 = t;
        Some(u32::from_ne_bytes(*h))
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if n > self.0.len() {
            return None;
        }
        let (h, t) = self.0.split_at(n);
        self.0 = t;
        Some(h)
    }

    /// Reads one POD and returns its type and body, skipping its padding.
    fn pod(&mut self) -> Option<(u32, &'a [u8])> {
        let size = self.u32()? as usize;
        let ty = self.u32()?;
        let body = self.take(size)?;
        let pad = (8 - size % 8) % 8;
        self.take(pad.min(self.0.len()));
        Some((ty, body))
    }
}

/// The first value of a POD, looking through a choice as SPA does for a
/// fixated format.
fn value(ty: u32, body: &[u8]) -> Option<(u32, &[u8])> {
    if ty != ty::CHOICE {
        return Some((ty, body));
    }
    let mut r = Reader(body);
    let kind = r.u32()?;
    let _flags = r.u32()?;
    let size = r.u32()? as usize;
    let child = r.u32()?;
    if kind != CHOICE_NONE && kind != CHOICE_RANGE && kind != CHOICE_ENUM {
        return None;
    }
    Some((child, r.take(size)?))
}

/// Parses a `Format` object. Anything but raw video in a format and size
/// the bridge handles is `None`.
pub fn parse_format(pod: &[u8]) -> Option<Format> {
    let (t, body) = Reader(pod).pod()?;
    if t != ty::OBJECT {
        return None;
    }
    let mut r = Reader(body);
    if r.u32()? != ty::OBJECT_FORMAT {
        return None;
    }
    let _id = r.u32()?;
    let (mut media, mut sub, mut format, mut size) = (None, None, None, None);
    while !r.0.is_empty() {
        let k = r.u32()?;
        let _flags = r.u32()?;
        let (t, body) = r.pod()?;
        let (t, v) = value(t, body)?;
        let mut v = Reader(v);
        match (k, t) {
            (key::MEDIA_TYPE, ty::ID) => media = v.u32(),
            (key::MEDIA_SUBTYPE, ty::ID) => sub = v.u32(),
            (key::VIDEO_FORMAT, ty::ID) => format = v.u32(),
            (key::VIDEO_SIZE, ty::RECTANGLE) => size = Some((v.u32()?, v.u32()?)),
            (key::VIDEO_FORMAT | key::VIDEO_SIZE, ty::INT) => return None,
            _ => {}
        }
    }
    if media? != MEDIA_TYPE_VIDEO || sub? != MEDIA_SUBTYPE_RAW {
        return None;
    }
    let (width, height) = size?;
    if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
        return None;
    }
    Some(Format {
        format: VideoFormat::from_id(format?)?,
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixated format as PipeWire sends it, with plain values.
    fn fixed(format: u32, w: u32, h: u32, subtype: u32) -> Vec<u8> {
        let mut b = Builder(Vec::new());
        let at = b.open(ty::OBJECT);
        b.u32(ty::OBJECT_FORMAT).u32(PARAM_FORMAT);
        b.prop(key::MEDIA_TYPE).id(MEDIA_TYPE_VIDEO);
        b.prop(key::MEDIA_SUBTYPE).id(subtype);
        b.prop(key::VIDEO_FORMAT).id(format);
        b.prop(key::VIDEO_SIZE)
            .u32(8)
            .u32(ty::RECTANGLE)
            .u32(w)
            .u32(h);
        b.prop(key::VIDEO_FRAMERATE)
            .u32(8)
            .u32(ty::FRACTION)
            .u32(30)
            .u32(1);
        b.close(at);
        b.0
    }

    #[test]
    fn offered_format_parses_as_its_defaults() {
        let pod = enum_format(640, 480);
        assert_eq!(pod.len() % 8, 0);
        assert_eq!(
            u32::from_ne_bytes(pod[0..4].try_into().unwrap()) as usize,
            pod.len() - 8
        );
        assert_eq!(
            parse_format(&pod),
            Some(Format {
                format: VideoFormat::Yuy2,
                width: 640,
                height: 480
            })
        );
        // Out-of-range preferences are clamped.
        let f = parse_format(&enum_format(0, 99999)).unwrap();
        assert_eq!((f.width, f.height), (1, MAX_HEIGHT));
    }

    #[test]
    fn fixated_formats() {
        let f = parse_format(&fixed(23, 1280, 720, MEDIA_SUBTYPE_RAW)).unwrap();
        assert_eq!(f.format, VideoFormat::Nv12);
        assert_eq!((f.width, f.height), (1280, 720));
        // MJPEG and unknown raw formats are refused.
        assert_eq!(parse_format(&fixed(4, 640, 480, 0x20002)), None);
        assert_eq!(parse_format(&fixed(7, 640, 480, MEDIA_SUBTYPE_RAW)), None);
        assert_eq!(parse_format(&fixed(4, 0, 480, MEDIA_SUBTYPE_RAW)), None);
        assert_eq!(parse_format(&fixed(4, 640, 8192, MEDIA_SUBTYPE_RAW)), None);
        // Larger than a frame ring slot.
        assert_eq!(parse_format(&fixed(4, 3840, 2160, MEDIA_SUBTYPE_RAW)), None);
        assert!(parse_format(&fixed(4, 1920, 1080, MEDIA_SUBTYPE_RAW)).is_some());
    }

    #[test]
    fn truncated_pods_are_refused() {
        let pod = fixed(4, 640, 480, MEDIA_SUBTYPE_RAW);
        for n in 0..pod.len() - 8 {
            assert_eq!(parse_format(&pod[..n]), None, "{n} bytes");
        }
        assert_eq!(parse_format(&[]), None);
    }
}
