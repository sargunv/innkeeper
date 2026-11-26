//! The frame ring: a sealed memfd that holds the camera's latest frames as
//! NV12, which the camera HAL maps read-only. The bridge converts each frame
//! PipeWire delivers into the next slot and sends `FRAME` with its number.
//! The layout is little-endian:
//!
//! ```text
//!    0  u32 magic "LXCF"   4 u32 version (1)   8 u32 slots   12 u32 slot_bytes
//!   16  u32 data_offset (4096)
//!   64  slot headers, 64 bytes each:
//!         +0 u64 seq     odd while the bridge writes the slot
//!         +8 u32 width  +12 u32 height
//!        +16 u64 frame   the frame's number, from 1
//!        +24 u64 ts_ns   CLOCK_MONOTONIC when the frame arrived
//! 4096  slot i at data_offset + i * slot_bytes: Y (width x height), then
//!       interleaved CbCr (ceil(width / 2) pairs x ceil(height / 2) rows)
//! ```
//!
//! The memfd is sealed against resizing and against new writable mappings,
//! so the HAL can only read it. A reader copies a slot between two loads of
//! its `seq`, and drops the copy if `seq` was odd or changed.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering, fence};

use rustix::fs::{MemfdFlags, SealFlags};
use rustix::mm::{MapFlags, ProtFlags};

use super::pod::VideoFormat;

pub const MAGIC: u32 = 0x4643_584c; // "LXCF"
pub const VERSION: u32 = 1;
pub const DATA_OFFSET: usize = 4096;
pub const SLOT_HEADERS: usize = 64;
pub const SLOTS: u32 = 3;
/// The largest frame a slot holds: 1920x1080 NV12.
pub const MAX_PIXELS: usize = 1920 * 1080;

/// Bytes of an NV12 frame.
pub fn nv12_size(width: u32, height: u32) -> usize {
    let (w, h) = (width as usize, height as usize);
    w * h + 2 * w.div_ceil(2) * h.div_ceil(2)
}

pub struct Ring {
    map: NonNull<u8>,
    len: usize,
    memfd: OwnedFd,
    slot_bytes: usize,
    next: u32,
    frame: u64,
}

// SAFETY: The mapping is owned by this struct and written only through it.
unsafe impl Send for Ring {}

impl Ring {
    pub fn new() -> std::io::Result<Ring> {
        let slot_bytes = nv12_size(1920, 1080).next_multiple_of(4096);
        let len = DATA_OFFSET + slot_bytes * SLOTS as usize;
        let memfd = rustix::fs::memfd_create(
            "innkeeper-camera",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )?;
        rustix::fs::ftruncate(&memfd, len as u64)?;
        // SAFETY: A new shared mapping of a memfd we own, of its full size.
        let p = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &memfd,
                0,
            )?
        };
        // FUTURE_WRITE keeps this mapping writable and refuses any new one.
        rustix::fs::fcntl_add_seals(
            &memfd,
            SealFlags::SHRINK | SealFlags::GROW | SealFlags::FUTURE_WRITE | SealFlags::SEAL,
        )?;
        let r = Ring {
            map: NonNull::new(p.cast()).expect("mmap returned null"),
            len,
            memfd,
            slot_bytes,
            next: 0,
            frame: 0,
        };
        r.put_u32(0, MAGIC);
        r.put_u32(4, VERSION);
        r.put_u32(8, SLOTS);
        r.put_u32(12, slot_bytes as u32);
        r.put_u32(16, DATA_OFFSET as u32);
        Ok(r)
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.memfd.as_fd()
    }

    pub fn slot_bytes(&self) -> usize {
        self.slot_bytes
    }

    fn put_u32(&self, off: usize, v: u32) {
        // SAFETY: Every offset used is inside the header page.
        unsafe { self.map.as_ptr().add(off).cast::<u32>().write(v.to_le()) }
    }

    fn put_u64(&self, off: usize, v: u64) {
        // SAFETY: As in put_u32. Slot header fields are 8-byte aligned.
        unsafe { self.map.as_ptr().add(off).cast::<u64>().write(v.to_le()) }
    }

    /// The slot's `seq`. The reference outlives the borrow of self, but
    /// only write() uses it, while self is alive.
    fn seq<'a>(&self, slot: u32) -> &'a AtomicU64 {
        // SAFETY: The slot header is inside the mapping, which lives as long
        // as self, and is 8-byte aligned.
        unsafe {
            &*self
                .map
                .as_ptr()
                .add(SLOT_HEADERS + slot as usize * 64)
                .cast()
        }
    }

    /// Converts `src` into the next slot and returns the slot and the
    /// frame's number. A frame that doesn't fit a slot is dropped.
    pub fn write(&mut self, src: &Source, ts_ns: u64) -> Option<(u32, u64)> {
        if nv12_size(src.width, src.height) > self.slot_bytes {
            return None;
        }
        let slot = self.next;
        let off = DATA_OFFSET + slot as usize * self.slot_bytes;
        // SAFETY: The slot is inside the mapping and only this struct writes
        // it. The HAL may read it concurrently, which the seqlock covers.
        let dst =
            unsafe { std::slice::from_raw_parts_mut(self.map.as_ptr().add(off), self.slot_bytes) };
        let seq = self.seq(slot);
        let s = seq.load(Ordering::Relaxed) | 1;
        seq.store(s, Ordering::Relaxed);
        fence(Ordering::Release);
        let ok = to_nv12(src, dst);
        let h = SLOT_HEADERS + slot as usize * 64;
        // A failed conversion may have overwritten part of the slot's old
        // frame, so the slot then holds no frame.
        let (w, hh, n) = if ok {
            self.frame += 1;
            (src.width, src.height, self.frame)
        } else {
            (0, 0, 0)
        };
        self.put_u32(h + 8, w);
        self.put_u32(h + 12, hh);
        self.put_u64(h + 16, n);
        self.put_u64(h + 24, ts_ns);
        seq.store(s + 1, Ordering::Release);
        if !ok {
            return None;
        }
        self.next = (slot + 1) % SLOTS;
        Some((slot, self.frame))
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        // SAFETY: The mapping was created in new with this length.
        let _ = unsafe { rustix::mm::munmap(self.map.as_ptr().cast(), self.len) };
    }
}

/// One plane of a source frame: its bytes and its row stride.
#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub data: &'a [u8],
    pub stride: usize,
}

/// A frame as PipeWire delivered it. Planar formats come as one plane per
/// buffer data, or as a single data that holds every plane, as V4L2 does.
pub struct Source<'a> {
    pub format: VideoFormat,
    pub width: u32,
    pub height: u32,
    pub planes: Vec<Plane<'a>>,
}

impl Plane<'_> {
    /// The plane's row `y`, `n` bytes long, if it is inside the data.
    fn row(&self, y: usize, n: usize) -> Option<&[u8]> {
        let start = y.checked_mul(self.stride)?;
        self.data.get(start..start.checked_add(n)?)
    }

    /// The rest of this plane from `rows` rows down, with `stride`.
    fn below(&self, rows: usize, stride: usize) -> Option<Plane<'_>> {
        Some(Plane {
            data: self.data.get(rows.checked_mul(self.stride)?..)?,
            stride,
        })
    }
}

/// Converts `src` into tightly packed NV12 at the start of `dst`. Returns
/// false if a plane is missing or too short.
pub fn to_nv12(src: &Source, dst: &mut [u8]) -> bool {
    let (w, h) = (src.width as usize, src.height as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    if dst.len() < nv12_size(src.width, src.height) || src.planes.is_empty() {
        return false;
    }
    let (y_out, uv_out) = dst.split_at_mut(w * h);
    let p0 = src.planes[0];
    // The chroma planes: their own datas, or after the luma in the first.
    let split = |n: usize| -> Option<Vec<Plane>> {
        if src.planes.len() >= n {
            return Some(src.planes[1..n].to_vec());
        }
        let half = p0.stride.div_ceil(2);
        match n {
            2 => Some(vec![p0.below(h, p0.stride)?]),
            _ => {
                let u = p0.below(h, half)?;
                let v = Plane {
                    data: u.data.get(half.checked_mul(ch)?..)?,
                    stride: half,
                };
                Some(vec![u, v])
            }
        }
    };
    let copy_y = |y_out: &mut [u8]| -> bool {
        for y in 0..h {
            let Some(r) = p0.row(y, w) else { return false };
            y_out[y * w..(y + 1) * w].copy_from_slice(r);
        }
        true
    };
    match src.format {
        VideoFormat::Nv12 | VideoFormat::Nv21 => {
            let Some(c) = split(2) else { return false };
            if !copy_y(y_out) {
                return false;
            }
            let swap = src.format == VideoFormat::Nv21;
            for y in 0..ch {
                let Some(r) = c[0].row(y, cw * 2) else {
                    return false;
                };
                let out = &mut uv_out[y * cw * 2..(y + 1) * cw * 2];
                if swap {
                    for (o, i) in out
                        .as_chunks_mut::<2>()
                        .0
                        .iter_mut()
                        .zip(r.as_chunks::<2>().0)
                    {
                        o[0] = i[1];
                        o[1] = i[0];
                    }
                } else {
                    out.copy_from_slice(r);
                }
            }
        }
        VideoFormat::I420 | VideoFormat::Yv12 => {
            let Some(c) = split(3) else { return false };
            if !copy_y(y_out) {
                return false;
            }
            let (u, v) = match src.format {
                VideoFormat::I420 => (c[0], c[1]),
                _ => (c[1], c[0]),
            };
            for y in 0..ch {
                let (Some(ur), Some(vr)) = (u.row(y, cw), v.row(y, cw)) else {
                    return false;
                };
                let out = &mut uv_out[y * cw * 2..(y + 1) * cw * 2];
                for (x, o) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    o[0] = ur[x];
                    o[1] = vr[x];
                }
            }
        }
        VideoFormat::Yuy2 | VideoFormat::Uyvy => {
            // Y0 U Y1 V, or U Y0 V Y1. Chroma is averaged over row pairs.
            let (yo, uo, vo) = match src.format {
                VideoFormat::Yuy2 => (0, 1, 3),
                _ => (1, 0, 2),
            };
            // An odd width still carries its last pixel pair whole.
            let row_bytes = cw * 4;
            for y in 0..h {
                let Some(r) = p0.row(y, row_bytes) else {
                    return false;
                };
                let out = &mut y_out[y * w..(y + 1) * w];
                for (x, o) in out.iter_mut().enumerate() {
                    *o = r[(x / 2) * 4 + yo + (x % 2) * 2];
                }
            }
            for y in 0..ch {
                let a = p0.row(2 * y, row_bytes);
                let b = p0.row((2 * y + 1).min(h - 1), row_bytes);
                let (Some(a), Some(b)) = (a, b) else {
                    return false;
                };
                let out = &mut uv_out[y * cw * 2..(y + 1) * cw * 2];
                for (x, o) in out.as_chunks_mut::<2>().0.iter_mut().enumerate() {
                    let avg =
                        |k: usize| (a[x * 4 + k] as u16 + b[x * 4 + k] as u16).div_ceil(2) as u8;
                    o[0] = avg(uo);
                    o[1] = avg(vo);
                }
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(format: VideoFormat, w: u32, h: u32, planes: Vec<Plane>) -> Source {
        Source {
            format,
            width: w,
            height: h,
            planes,
        }
    }

    /// The expected NV12 of a 4x2 test frame: luma 10..17, Cb 100, 101 and
    /// Cr 200, 201.
    const NV12: [u8; 12] = [10, 11, 12, 13, 14, 15, 16, 17, 100, 200, 101, 201];

    fn convert(s: &Source) -> Option<Vec<u8>> {
        let mut out = vec![0; nv12_size(s.width, s.height)];
        to_nv12(s, &mut out).then_some(out)
    }

    #[test]
    fn every_format_converts_to_nv12() {
        let y = [10u8, 11, 12, 13, 14, 15, 16, 17];
        let p = |d: &'static [u8], stride| Plane { data: d, stride };
        // NV12 in one data with a padded stride, and in two datas.
        let one: &[u8] = &[
            10, 11, 12, 13, 0, 0, 14, 15, 16, 17, 0, 0, 100, 200, 101, 201, 0, 0,
        ];
        assert_eq!(
            convert(&src(VideoFormat::Nv12, 4, 2, vec![p(one, 6)])).unwrap(),
            NV12
        );
        let y_only: &'static [u8] = Box::leak(Box::new(y));
        assert_eq!(
            convert(&src(
                VideoFormat::Nv12,
                4,
                2,
                vec![p(y_only, 4), p(&[100, 200, 101, 201], 4)]
            ))
            .unwrap(),
            NV12
        );
        assert_eq!(
            convert(&src(
                VideoFormat::Nv21,
                4,
                2,
                vec![p(y_only, 4), p(&[200, 100, 201, 101], 4)]
            ))
            .unwrap(),
            NV12
        );
        // I420 and YV12, packed into one data.
        let i420: &[u8] = &[10, 11, 12, 13, 14, 15, 16, 17, 100, 101, 200, 201];
        assert_eq!(
            convert(&src(VideoFormat::I420, 4, 2, vec![p(i420, 4)])).unwrap(),
            NV12
        );
        let yv12: &[u8] = &[10, 11, 12, 13, 14, 15, 16, 17, 200, 201, 100, 101];
        assert_eq!(
            convert(&src(VideoFormat::Yv12, 4, 2, vec![p(yv12, 4)])).unwrap(),
            NV12
        );
        // Packed 4:2:2 averages chroma over the two rows.
        let yuy2: &[u8] = &[
            10, 99, 11, 199, 12, 100, 13, 200, //
            14, 101, 15, 201, 16, 102, 17, 202,
        ];
        assert_eq!(
            convert(&src(VideoFormat::Yuy2, 4, 2, vec![p(yuy2, 8)])).unwrap(),
            NV12
        );
        let uyvy: &[u8] = &[
            99, 10, 199, 11, 100, 12, 200, 13, //
            101, 14, 201, 15, 102, 16, 202, 17,
        ];
        assert_eq!(
            convert(&src(VideoFormat::Uyvy, 4, 2, vec![p(uyvy, 8)])).unwrap(),
            NV12
        );
    }

    #[test]
    fn odd_sizes_and_short_planes() {
        // 3x3 YUY2: the last pair is cut at the width and the last chroma
        // row repeats the last row.
        let yuy2: &'static [u8] = Box::leak(vec![50u8; 3 * 8].into_boxed_slice());
        let s = src(
            VideoFormat::Yuy2,
            3,
            3,
            vec![Plane {
                data: yuy2,
                stride: 8,
            }],
        );
        let out = convert(&s).unwrap();
        assert_eq!(out.len(), 9 + 2 * 2 * 2);
        assert!(out.iter().all(|b| *b == 50));
        // A plane one byte short is refused.
        let short = src(
            VideoFormat::Nv12,
            4,
            2,
            vec![Plane {
                data: &NV12[..11],
                stride: 4,
            }],
        );
        assert!(convert(&short).is_none());
        let none = src(VideoFormat::I420, 4, 2, vec![]);
        assert!(convert(&none).is_none());
    }

    #[test]
    fn ring_writes_slots_in_turn_under_a_seqlock() {
        let mut r = Ring::new().unwrap();
        let s = src(
            VideoFormat::Nv12,
            4,
            2,
            vec![Plane {
                data: &NV12,
                stride: 4,
            }],
        );
        assert_eq!(r.write(&s, 7), Some((0, 1)));
        assert_eq!(r.write(&s, 8), Some((1, 2)));
        assert_eq!(r.write(&s, 9), Some((2, 3)));
        assert_eq!(r.write(&s, 10), Some((0, 4)));
        // Read it back as the HAL does, through a new read-only mapping.
        let len = DATA_OFFSET + r.slot_bytes() * SLOTS as usize;
        let ro = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ,
                MapFlags::SHARED,
                r.fd(),
                0,
            )
            .unwrap()
        };
        let m = unsafe { std::slice::from_raw_parts(ro.cast::<u8>(), len) };
        let u32_at = |o: usize| u32::from_le_bytes(m[o..o + 4].try_into().unwrap());
        let u64_at = |o: usize| u64::from_le_bytes(m[o..o + 8].try_into().unwrap());
        assert_eq!(u32_at(0), MAGIC);
        assert_eq!(u32_at(8), SLOTS);
        assert_eq!(u64_at(SLOT_HEADERS), 4, "slot 0 written twice");
        assert_eq!(u64_at(SLOT_HEADERS + 16), 4);
        assert_eq!(u64_at(SLOT_HEADERS + 24), 10);
        assert_eq!(
            (u32_at(SLOT_HEADERS + 8), u32_at(SLOT_HEADERS + 12)),
            (4, 2)
        );
        assert_eq!(&m[DATA_OFFSET..DATA_OFFSET + 12], &NV12);
        // No new writable mapping, and no resizing.
        let rw = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                len,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                r.fd(),
                0,
            )
        };
        assert!(rw.is_err());
        assert!(rustix::fs::ftruncate(r.fd(), 0).is_err());
        unsafe { rustix::mm::munmap(ro, len).unwrap() };
        // Too large for a slot.
        let big = src(VideoFormat::Nv12, 4096, 4096, vec![]);
        assert_eq!(r.write(&big, 0), None);
        // A short frame fails in slot 1, which then holds no frame.
        let short = src(
            VideoFormat::Nv12,
            4,
            2,
            vec![Plane {
                data: &NV12[..10],
                stride: 4,
            }],
        );
        assert_eq!(r.write(&short, 11), None);
        let m = unsafe { std::slice::from_raw_parts(r.map.as_ptr(), DATA_OFFSET) };
        let h1 = SLOT_HEADERS + 64;
        assert_eq!(u64::from_le_bytes(m[h1..h1 + 8].try_into().unwrap()) % 2, 0);
        assert_eq!(&m[h1 + 8..h1 + 24], &[0; 16]);
        // The next good frame takes that slot.
        assert_eq!(r.write(&s, 12), Some((1, 5)));
    }
}
