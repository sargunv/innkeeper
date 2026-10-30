//! The shared PCM ring: one sealed memfd per PCM stream, created here and
//! handed to the Android client with an eventfd ("data") and the read end of
//! a pipe ("space"). Layout, all little-endian:
//!
//! ```text
//!   0  u32 magic "LXAR"   4 u32 version   8 u32 frame_bytes  12 u32 capacity_frames
//!  16  u32 rate          20 u32 data_offset (4096)
//!  64  u64 write_pos      client-owned: frames written (monotonic)
//! 128  u64 read_pos       bridge-owned: frames consumed
//! 192  u64 timing_seq     bridge-owned seqlock (odd while the next three change)
//! 200  u64 played_frames  frames presented at ts_ns
//! 208  u64 ts_ns          CLOCK_MONOTONIC of that estimate (shared with the session)
//! 216  u64 latency_ns     downstream latency (PipeWire queue plus device)
//! 224  u32 state          0 idle, 1 playing, 2 dead (client must reopen)
//! 4096 PCM data, capacity_frames * frame_bytes bytes
//! ```
//!
//! ```mermaid
//! sequenceDiagram
//!   participant C as libtinyalsav2 (HAL)
//!   participant R as ring memfd
//!   participant B as bridge stream thread
//!   participant P as PipeWire (pulse)
//!   C->>R: copy frames at write_pos % capacity, publish write_pos
//!   C->>B: write 1 to the data eventfd
//!   P->>B: wants frames
//!   B->>R: read frames, advance read_pos, update timing (seqlock)
//!   B->>C: one byte on the space pipe (unless one is pending)
//!   Note over C: when full, poll the space pipe, then drain it
//! ```
//!
//! - The client never gets more than `capacity` frames ahead of `read_pos`.
//! - Presentation position at time `t` is
//!   `played_frames + (t - ts_ns) * rate` while `state == 1`, capped at
//!   `read_pos`; app-visible latency is written minus presented.
//! - When the bridge drops a stream (PipeWire went away, or CLOSE) it sets
//!   `state = 2` and signals space, so a blocked writer wakes up.
//!
//! Capture reverses ownership: the bridge publishes write_pos, the client
//! publishes read_pos after copying. New frames are dropped on overrun so
//! unread samples are never overwritten. The same pipe signals available
//! capture data (or death); the eventfd requests recording.
//!
//! The client can write anything into the mapping at any time, so the bridge
//! never reads back what it owns: `read_pos` lives in this struct and is only
//! mirrored out. The one client-owned value, `write_pos`, is clamped on every
//! read (behind `read_pos` is ignored, more than `capacity` ahead is
//! clamped). The memfd is sealed against shrinking, so a client cannot
//! truncate it under our mapping.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use rustix::fs::{MemfdFlags, SealFlags};
use rustix::mm::{MapFlags, ProtFlags};

pub const MAGIC: u32 = 0x5241_584c; // "LXAR"
pub const VERSION: u32 = 1;
pub const DATA_OFFSET: usize = 4096;

pub const OFF_WRITE_POS: usize = 64;
pub const OFF_READ_POS: usize = 128;
pub const OFF_TIMING_SEQ: usize = 192;
pub const OFF_PLAYED: usize = 200;
pub const OFF_TS_NS: usize = 208;
pub const OFF_LATENCY_NS: usize = 216;
pub const OFF_STATE: usize = 224;

pub const STATE_IDLE: u32 = 0;
pub const STATE_PLAYING: u32 = 1;
pub const STATE_DEAD: u32 = 2;

pub struct Ring {
    map: NonNull<u8>,
    len: usize,
    memfd: OwnedFd,
    frame_bytes: usize,
    capacity: u64,
    read_pos: u64,
    write_pos: u64,
}

// SAFETY: the mapping is owned by this struct and only touched through it
// (atomics for the header, raw copies for PCM), so moving it between threads
// is fine.
unsafe impl Send for Ring {}

impl Ring {
    /// Size of the mapping for a ring, or None if it would be absurd.
    pub fn map_size(frame_bytes: u32, capacity_frames: u32) -> Option<usize> {
        let data = (frame_bytes as usize).checked_mul(capacity_frames as usize)?;
        let total = DATA_OFFSET.checked_add(data)?;
        // Round up to a page.
        Some(total.checked_add(4095)? & !4095)
    }

    pub fn new(frame_bytes: u32, capacity_frames: u32, rate: u32) -> std::io::Result<Ring> {
        let len = Self::map_size(frame_bytes, capacity_frames)
            .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let memfd = rustix::fs::memfd_create(
            "linuxapk-ring",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )?;
        rustix::fs::ftruncate(&memfd, len as u64)?;
        rustix::fs::fcntl_add_seals(
            &memfd,
            SealFlags::SHRINK | SealFlags::GROW | SealFlags::SEAL,
        )?;
        // SAFETY: a fresh shared mapping of a memfd we own, of exactly its
        // (sealed) size.
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
        let ring = Ring {
            map: NonNull::new(p.cast()).expect("mmap returned null"),
            len,
            memfd,
            frame_bytes: frame_bytes as usize,
            capacity: capacity_frames as u64,
            read_pos: 0,
            write_pos: 0,
        };
        ring.u32_at(0).store(MAGIC, Ordering::Relaxed);
        ring.u32_at(4).store(VERSION, Ordering::Relaxed);
        ring.u32_at(8).store(frame_bytes, Ordering::Relaxed);
        ring.u32_at(12).store(capacity_frames, Ordering::Relaxed);
        ring.u32_at(16).store(rate, Ordering::Relaxed);
        ring.u32_at(20).store(DATA_OFFSET as u32, Ordering::Release);
        Ok(ring)
    }

    fn u64_at(&self, off: usize) -> &AtomicU64 {
        debug_assert!(off.is_multiple_of(8) && off + 8 <= DATA_OFFSET);
        // SAFETY: in bounds of the header page and 8-aligned (the mapping is
        // page-aligned); atomics are the right way to share it.
        unsafe { &*self.map.as_ptr().add(off).cast::<AtomicU64>() }
    }

    fn u32_at(&self, off: usize) -> &AtomicU32 {
        debug_assert!(off.is_multiple_of(4) && off + 4 <= DATA_OFFSET);
        // SAFETY: as above.
        unsafe { &*self.map.as_ptr().add(off).cast::<AtomicU32>() }
    }

    pub fn memfd(&self) -> BorrowedFd<'_> {
        self.memfd.as_fd()
    }
    pub fn map_len(&self) -> usize {
        self.len
    }
    pub fn capacity(&self) -> u64 {
        self.capacity
    }
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }
    /// Frames consumed so far.
    pub fn read_pos(&self) -> u64 {
        self.read_pos
    }

    /// Frames the client has made available, clamped to what's plausible.
    pub fn available(&self) -> u64 {
        let w = self.u64_at(OFF_WRITE_POS).load(Ordering::Acquire);
        // A write_pos behind ours means a confused client: nothing to read.
        // One more than a ring ahead means it overran us: the ring is full
        // (of whatever it last wrote).
        w.checked_sub(self.read_pos)
            .map_or(0, |n| n.min(self.capacity))
    }

    /// Copies up to `out.len() / frame_bytes` available frames into `out`
    /// and consumes them. Returns the number of frames.
    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let want = (out.len() / self.frame_bytes) as u64;
        let n = want.min(self.available());
        let start = (self.read_pos % self.capacity) as usize;
        let first = (n as usize).min(self.capacity as usize - start);
        let fb = self.frame_bytes;
        // SAFETY: both ranges are inside the data area (start + first <=
        // capacity, and the wrapped part starts at 0), and `out` holds n
        // frames. The client may be writing concurrently; we copy raw bytes
        // and treat them as opaque PCM, so a torn read is just noise.
        unsafe {
            let data = self.map.as_ptr().add(DATA_OFFSET);
            std::ptr::copy_nonoverlapping(data.add(start * fb), out.as_mut_ptr(), first * fb);
            std::ptr::copy_nonoverlapping(
                data,
                out.as_mut_ptr().add(first * fb),
                (n as usize - first) * fb,
            );
        }
        self.read_pos += n;
        self.u64_at(OFF_READ_POS)
            .store(self.read_pos, Ordering::Release);
        n as usize
    }

    /// Capture producer: never overwrite frames the client is still reading.
    /// On overrun drop the new frames; only the consumer owns read_pos.
    pub fn write(&mut self, input: &[u8]) -> usize {
        let read = self
            .u64_at(OFF_READ_POS)
            .load(Ordering::Acquire)
            .min(self.write_pos);
        let queued = self.write_pos.saturating_sub(read).min(self.capacity);
        let n = (input.len() / self.frame_bytes).min((self.capacity - queued) as usize);
        let start = (self.write_pos % self.capacity) as usize;
        let first = n.min(self.capacity as usize - start);
        let fb = self.frame_bytes;
        // SAFETY: both wrapped ranges fit the mapping and input; the client
        // publishes read_pos only after copying, so unread samples stay intact.
        unsafe {
            let data = self.map.as_ptr().add(DATA_OFFSET);
            std::ptr::copy_nonoverlapping(input.as_ptr(), data.add(start * fb), first * fb);
            std::ptr::copy_nonoverlapping(input.as_ptr().add(first * fb), data, (n - first) * fb);
        }
        self.write_pos += n as u64;
        self.u64_at(OFF_WRITE_POS)
            .store(self.write_pos, Ordering::Release);
        n
    }

    /// Publishes a presentation estimate (seqlock-protected for the client).
    pub fn publish_timing(&self, played_frames: u64, ts_ns: u64, latency_ns: u64) {
        let seq = self.u64_at(OFF_TIMING_SEQ);
        let s = seq.load(Ordering::Relaxed);
        seq.store(s.wrapping_add(1) | 1, Ordering::Relaxed);
        std::sync::atomic::fence(Ordering::Release);
        self.u64_at(OFF_PLAYED)
            .store(played_frames, Ordering::Relaxed);
        self.u64_at(OFF_TS_NS).store(ts_ns, Ordering::Relaxed);
        self.u64_at(OFF_LATENCY_NS)
            .store(latency_ns, Ordering::Relaxed);
        seq.store((s | 1).wrapping_add(1), Ordering::Release);
    }

    pub fn set_state(&self, state: u32) {
        self.u32_at(OFF_STATE).store(state, Ordering::Release);
    }

    /// Test helper: what a client does to produce frames.
    #[cfg(test)]
    pub fn client_write(&self, frames: &[u8]) {
        let fb = self.frame_bytes;
        let w = self.u64_at(OFF_WRITE_POS).load(Ordering::Relaxed);
        let n = frames.len() / fb;
        for i in 0..n {
            let idx = ((w + i as u64) % self.capacity) as usize;
            // SAFETY: idx < capacity; test-only.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    frames.as_ptr().add(i * fb),
                    self.map.as_ptr().add(DATA_OFFSET + idx * fb),
                    fb,
                );
            }
        }
        self.u64_at(OFF_WRITE_POS)
            .store(w + n as u64, Ordering::Release);
    }

    #[cfg(test)]
    pub fn client_set_write_pos(&self, w: u64) {
        self.u64_at(OFF_WRITE_POS).store(w, Ordering::Release);
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        self.set_state(STATE_DEAD);
        // SAFETY: unmapping exactly the mapping we created.
        unsafe {
            let _ = rustix::mm::munmap(self.map.as_ptr().cast(), self.len);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_wraps_without_overwriting_unread_frames() {
        let mut r = Ring::new(2, 4, 48000).unwrap();
        assert_eq!(r.write(&[1, 2, 3, 4, 5, 6]), 3);
        assert_eq!(r.write(&[7, 8, 9, 10]), 1); // drop newest on overrun
        assert_eq!(r.write(&[99, 99]), 0);
        let mut out = [0; 6];
        assert_eq!(r.read(&mut out), 3); // capture consumer publishes read_pos
        assert_eq!(out, [1, 2, 3, 4, 5, 6]);
        assert_eq!(r.write(&[9, 10, 11, 12, 13, 14]), 3);
        let mut out = [0; 8];
        assert_eq!(r.read(&mut out), 4);
        assert_eq!(out, [7, 8, 9, 10, 11, 12, 13, 14]);
        assert_eq!(r.read(&mut out), 0); // underrun
    }

    #[test]
    fn wraps_and_clamps() {
        let mut r = Ring::new(4, 8, 48000).unwrap();
        assert_eq!(r.map_len(), 8192);
        let frames: Vec<u8> = (0..24u8).collect(); // 6 frames
        r.client_write(&frames);
        assert_eq!(r.available(), 6);
        let mut out = [0u8; 16];
        assert_eq!(r.read(&mut out), 4);
        assert_eq!(&out, &frames[..16]);
        // Wrap: 2 left, write 6 more (positions 6..12 -> indexes 6,7,0..3).
        let more: Vec<u8> = (100..124u8).collect();
        r.client_write(&more);
        assert_eq!(r.available(), 8);
        let mut out = [0u8; 40];
        assert_eq!(r.read(&mut out), 8);
        assert_eq!(&out[..8], &frames[16..24]);
        assert_eq!(&out[8..32], &more[..]);
        assert_eq!(r.read_pos(), 12);
    }

    #[test]
    fn hostile_write_pos() {
        let mut r = Ring::new(4, 8, 48000).unwrap();
        r.client_set_write_pos(u64::MAX);
        assert_eq!(r.available(), 8);
        let mut out = [0u8; 64];
        assert_eq!(r.read(&mut out), 8);
        r.client_set_write_pos(3); // behind read_pos
        assert_eq!(r.available(), 0);
        assert_eq!(r.read(&mut out), 0);
    }
}
