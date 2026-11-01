//! Payloads too big for one 512-byte message, such as notifications,
//! clipboard contents, URLs and artwork, travel as a sealed memfd next to a
//! small request or event. The system and apps services use them.
//!
//! - **From a client** ([`read`]): the fd must be a memfd sealed against
//!   writing, shrinking and growing, so its bytes can't change while we read
//!   them. It is read once with `pread` into our own buffer. It is not mapped,
//!   so a client can't make the bridge take SIGBUS. Only the declared length is
//!   read, and that length is capped per message type.
//! - **To a client** ([`create`]): the bridge writes a memfd and seals it the
//!   same way, adding `F_SEAL_SEAL`.
//!
//! [`Doc`] decodes a blob's contents. They use the same little-endian layout
//! as messages, with `u32`-length text fields and RGBA images.

use std::os::fd::{AsFd, OwnedFd};

use rustix::fs::{MemfdFlags, SealFlags};

use crate::ipc::wire::{DecodeError, Reader, errno::*};

/// The seals a client's blob must carry.
pub const REQUIRED_SEALS: SealFlags = SealFlags::WRITE
    .union(SealFlags::SHRINK)
    .union(SealFlags::GROW);

/// Reads `len` bytes, at most `max`, from a client's sealed memfd. Errors are
/// reply statuses.
pub fn read(fd: &OwnedFd, len: u32, max: usize) -> Result<Vec<u8>, i32> {
    let len = len as usize;
    if len > max {
        return Err(-EINVAL);
    }
    // F_GET_SEALS fails on anything that isn't a memfd or shmem. That also
    // rules out pipes, sockets and FUSE files that could block a read.
    let seals = rustix::fs::fcntl_get_seals(fd).map_err(|_| -EINVAL)?;
    if !seals.contains(REQUIRED_SEALS) {
        return Err(-EINVAL);
    }
    let st = rustix::fs::fstat(fd).map_err(|_| -EINVAL)?;
    if (st.st_size as u64) < len as u64 {
        return Err(-EINVAL);
    }
    let mut buf = vec![0u8; len];
    let mut off = 0;
    while off < len {
        match rustix::io::pread(fd, &mut buf[off..], off as u64) {
            Ok(0) => return Err(-EINVAL),
            Ok(n) => off += n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(_) => return Err(-EIO),
        }
    }
    Ok(buf)
}

/// Creates a sealed memfd holding `bytes` to pass to a client.
pub fn create(name: &str, bytes: &[u8]) -> anyhow::Result<OwnedFd> {
    let fd = rustix::fs::memfd_create(name, MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)?;
    let mut off = 0;
    while off < bytes.len() {
        match rustix::io::write(&fd, &bytes[off..]) {
            Ok(n) => off += n,
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        }
    }
    rustix::fs::fcntl_add_seals(fd.as_fd(), REQUIRED_SEALS | SealFlags::SEAL)?;
    Ok(fd)
}

/// An RGBA image with 8 bits per channel, not premultiplied, and packed rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    pub width: u16,
    pub height: u16,
    pub rgba: Vec<u8>,
}

/// Decoder for a blob's contents, built on [`Reader`].
pub struct Doc<'a>(pub Reader<'a>);

impl<'a> Doc<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Doc(Reader::new(b))
    }

    /// `u32 len` then UTF-8 of at most `max` bytes. NUL is rejected. Other
    /// control characters except newline and tab are dropped because Android
    /// text ends up in desktop UI.
    pub fn text(&mut self, max: usize) -> Result<String, DecodeError> {
        let n = self.0.u32()? as usize;
        if n > max {
            return Err(DecodeError::Invalid("text too long"));
        }
        let s = std::str::from_utf8(self.0.take(n)?)
            .map_err(|_| DecodeError::Invalid("text not UTF-8"))?;
        if s.contains('\0') {
            return Err(DecodeError::Invalid("NUL in text"));
        }
        Ok(s.chars()
            .filter(|&c| c == '\n' || c == '\t' || !c.is_control())
            .collect())
    }

    /// `u16 width`, `u16 height`, then `width * height * 4` bytes of RGBA.
    /// 0x0 means no image. Each side is at most `max_side`.
    pub fn image(&mut self, max_side: u16) -> Result<Option<Image>, DecodeError> {
        let (w, h) = (self.0.u16()?, self.0.u16()?);
        if w == 0 && h == 0 {
            return Ok(None);
        }
        if w == 0 || h == 0 || w > max_side || h > max_side {
            return Err(DecodeError::Invalid("image size"));
        }
        let rgba = self.0.take(w as usize * h as usize * 4)?.to_vec();
        Ok(Some(Image {
            width: w,
            height: h,
            rgba,
        }))
    }

    pub fn end(&self) -> Result<(), DecodeError> {
        self.0.end()
    }
}

/// Encoders matching [`Doc`], for tests and bridge-to-client blobs.
pub trait DocWriter {
    fn text(&mut self, s: &str) -> &mut Self;
    fn image(&mut self, img: Option<&Image>) -> &mut Self;
}

impl DocWriter for crate::ipc::wire::Writer {
    fn text(&mut self, s: &str) -> &mut Self {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
        self
    }
    fn image(&mut self, img: Option<&Image>) -> &mut Self {
        match img {
            None => {
                self.u16(0).u16(0);
            }
            Some(i) => {
                self.u16(i.width).u16(i.height);
                self.0.extend_from_slice(&i.rgba);
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::wire::Writer;

    #[test]
    fn memfd_roundtrip_and_seal_checks() {
        let fd = create("t", b"hello world").unwrap();
        assert_eq!(read(&fd, 5, 64).unwrap(), b"hello");
        assert_eq!(read(&fd, 11, 64).unwrap(), b"hello world");
        assert_eq!(read(&fd, 12, 64), Err(-EINVAL)); // longer than the file
        assert_eq!(read(&fd, 11, 4), Err(-EINVAL)); // over the cap

        // An unsealed memfd and a pipe are refused.
        let raw = rustix::fs::memfd_create("u", MemfdFlags::ALLOW_SEALING).unwrap();
        rustix::io::write(&raw, b"x").unwrap();
        assert_eq!(read(&raw, 1, 64), Err(-EINVAL));
        let (r, _w) = rustix::pipe::pipe().unwrap();
        assert_eq!(read(&r, 1, 64), Err(-EINVAL));
    }

    #[test]
    fn text_and_images() {
        let mut w = Writer::default();
        w.text("a\nb\u{7}c\td");
        w.image(Some(&Image {
            width: 1,
            height: 2,
            rgba: vec![1; 8],
        }));
        w.image(None);
        let mut d = Doc::new(&w.0);
        assert_eq!(d.text(64).unwrap(), "a\nbc\td");
        assert_eq!(d.image(4).unwrap().unwrap().rgba.len(), 8);
        assert_eq!(d.image(4).unwrap(), None);
        d.end().unwrap();

        let mut w = Writer::default();
        w.text("x\0y");
        assert!(Doc::new(&w.0).text(64).is_err());
        let mut w = Writer::default();
        w.text("toolong");
        assert!(Doc::new(&w.0).text(3).is_err());
        let mut w = Writer::default();
        w.u16(0).u16(5);
        assert!(Doc::new(&w.0).image(8).is_err());
        let mut w = Writer::default();
        w.u16(9).u16(1);
        assert!(Doc::new(&w.0).image(8).is_err());
    }
}
