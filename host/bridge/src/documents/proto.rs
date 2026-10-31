//! Documents service messages (id 4, types 0x0400..=0x04ff, events
//! 0x4400..=0x44ff). The client is the Innkeeper System app's document
//! picker (`PickerActivity.kt`) and DocumentsProvider
//! (`HostDocumentsProvider.kt`); only its uid may open the service.
//!
//! | Type     | Request | Body                                                                                                                                                                                                   | Reply                                                                                                                                                         |
//! | -------- | ------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------- |
//! | `0x0401` | PICK    | `u32 request` (client's id), `u8 mode` (1 open, 2 open several, 3 save, 4 folder), `u8 write`, `u32 window` (Android display id, 0 none), name `title`, name `suggested`, `u8 n` (≤ 16) + n MIME names | At once; the result comes as events. `-EBUSY` with 4 picks open in the whole bridge (open until the user answers, even if the client is gone), `-ENOSYS` without a session bus; test sessions complete with `PICK_DONE(-ECANCELED, 0)` without a portal |
//! | `0x0402` | OPEN    | `u64 grant`, `u8 mode` (1 read, 2 write+truncate, 3 read-write, 4 append), path                                                                                                                        | fd: the open file                                                                                                                                             |
//! | `0x0403` | STAT    | `u64 grant`, path                                                                                                                                                                                      | meta                                                                                                                                                          |
//! | `0x0404` | LIST    | `u64 grant`, path (a folder)                                                                                                                                                                           | `u32 len`, `u32 count`; fd: sealed memfd with `count` metas (regular files and folders, ≤ 10000)                                                              |
//! | `0x0405` | CREATE  | `u64 grant`, path (the folder), `u8 kind` (0 file, 1 folder), file name                                                                                                                                | `u16 len` + the name used (`name (1).ext` if taken)                                                                                                           |
//! | `0x0406` | DELETE  | `u64 grant`, path                                                                                                                                                                                      | Only beneath a folder grant, never the picked item itself; folders only when empty                                                                           |
//! | `0x0407` | RENAME  | `u64 grant`, path, new file name (same folder, never replacing)                                                                                                                                        |                                                                                                                                                               |
//! | `0x0408` | FORGET  | `u64 grant`                                                                                                                                                                                            |                                                                                                                                                               |
//!
//! | Event    | Message   | Body                                                                                       |
//! | -------- | --------- | ------------------------------------------------------------------------------------------ |
//! | `0x4401` | PICKED    | `u32 request`, `u64 grant`, `u8 kind`, `u8 writable`, name (display name), one per item    |
//! | `0x4402` | PICK_DONE | `u32 request`, `i32 status` (0, `-ECANCELED` when the user cancelled, `-EIO`), `u32 count` |
//!
//! Paths are relative to a folder grant: `u16 len` + `/`-separated UTF-8
//! names, none empty, `.` or `..`, at most 400 bytes; names are at most 255
//! bytes with no NUL or `/`. A meta is `u8 kind`, `u64 size`,
//! `i64 mtime_ms`, `u8 writable`, `u16 len` + name. Writes, creates, deletes
//! and renames need a grant made for writing.

use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const PICK: u16 = 0x0401;
    pub const OPEN: u16 = 0x0402;
    pub const STAT: u16 = 0x0403;
    pub const LIST: u16 = 0x0404;
    pub const CREATE: u16 = 0x0405;
    pub const DELETE: u16 = 0x0406;
    pub const RENAME: u16 = 0x0407;
    pub const FORGET: u16 = 0x0408;
    /// Import a desktop file (one fd); reply: u64 read-only grant id.
    pub const IMPORT: u16 = 0x0409;

    /// Events.
    pub const PICKED: u16 = 0x4401;
    pub const PICK_DONE: u16 = 0x4402;
}

pub mod limits {
    /// Relative paths inside a granted folder, in bytes.
    pub const MAX_PATH: usize = 400;
    /// One file name, in bytes.
    pub const MAX_NAME: usize = 255;
    pub const MAX_MIMES: usize = 16;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickMode {
    Open = 1,
    OpenMultiple = 2,
    Save = 3,
    Folder = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenMode {
    Read = 1,
    /// Write, truncating.
    Write = 2,
    ReadWrite = 3,
    /// Write, appending.
    Append = 4,
}

impl OpenMode {
    pub fn writes(self) -> bool {
        self != OpenMode::Read
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File = 0,
    Dir = 1,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// The client's id for this pick, echoed in the events.
    pub request: u32,
    pub mode: PickMode,
    /// The app asked for write access.
    pub write: bool,
    /// Android display id of the app's window (0: none), for the dialog's
    /// parent.
    pub window: u32,
    pub title: String,
    /// SaveFile's suggested name.
    pub name: String,
    pub mimes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Import {
        name: String,
    },
    Pick(Pick),
    Open {
        grant: u64,
        mode: OpenMode,
        path: String,
    },
    Stat {
        grant: u64,
        path: String,
    },
    List {
        grant: u64,
        path: String,
    },
    Create {
        grant: u64,
        path: String,
        kind: Kind,
        name: String,
    },
    Delete {
        grant: u64,
        path: String,
    },
    Rename {
        grant: u64,
        path: String,
        name: String,
    },
    Forget {
        grant: u64,
    },
}

/// A relative path inside a granted folder: `/`-separated names, none empty,
/// `.` or `..`, no NUL, at most MAX_PATH bytes. Empty is the folder itself.
pub fn valid_path(p: &str) -> bool {
    p.len() <= limits::MAX_PATH && (p.is_empty() || p.split('/').all(valid_name))
}

/// One file name: 1 to 255 bytes, no `/` or NUL, not `.` or `..`.
pub fn valid_name(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= limits::MAX_NAME
        && n != "."
        && n != ".."
        && !n.contains(['/', '\0'])
}

fn text(r: &mut Reader<'_>, max: usize) -> Result<String, DecodeError> {
    let n = r.u16()? as usize;
    if n > max {
        return Err(DecodeError::Invalid("text too long"));
    }
    let s = std::str::from_utf8(r.take(n)?).map_err(|_| DecodeError::Invalid("not UTF-8"))?;
    Ok(s.to_owned())
}

fn path(r: &mut Reader<'_>) -> Result<String, DecodeError> {
    let p = text(r, limits::MAX_PATH)?;
    if !valid_path(&p) {
        return Err(DecodeError::Invalid("path"));
    }
    Ok(p)
}

fn name(r: &mut Reader<'_>) -> Result<String, DecodeError> {
    let n = text(r, limits::MAX_NAME)?;
    if !valid_name(&n) {
        return Err(DecodeError::Invalid("name"));
    }
    Ok(n)
}

fn mime(m: &str) -> bool {
    let ok = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
    };
    match m.split_once('/') {
        Some((t, s)) => ok(t) && (s == "*" || ok(s)) || m == "*/*",
        None => false,
    }
}

pub fn decode(t: u16, body: &[u8]) -> Result<Request, DecodeError> {
    let mut r = Reader::new(body);
    let req = match t {
        ty::PICK => {
            let request = r.u32()?;
            let mode = match r.u8()? {
                1 => PickMode::Open,
                2 => PickMode::OpenMultiple,
                3 => PickMode::Save,
                4 => PickMode::Folder,
                _ => return Err(DecodeError::Invalid("pick mode")),
            };
            let write = r.bool()?;
            let window = r.u32()?;
            let title = r.name()?;
            let name = r.name()?;
            let n = r.u8()? as usize;
            if n > limits::MAX_MIMES {
                return Err(DecodeError::Invalid("too many types"));
            }
            let mut mimes = Vec::with_capacity(n);
            for _ in 0..n {
                let m = r.name()?;
                if !mime(&m) {
                    return Err(DecodeError::Invalid("MIME type"));
                }
                mimes.push(m);
            }
            Request::Pick(Pick {
                request,
                mode,
                write,
                window,
                title,
                name,
                mimes,
            })
        }
        ty::OPEN => Request::Open {
            grant: r.u64()?,
            mode: match r.u8()? {
                1 => OpenMode::Read,
                2 => OpenMode::Write,
                3 => OpenMode::ReadWrite,
                4 => OpenMode::Append,
                _ => return Err(DecodeError::Invalid("open mode")),
            },
            path: path(&mut r)?,
        },
        ty::STAT => Request::Stat {
            grant: r.u64()?,
            path: path(&mut r)?,
        },
        ty::LIST => Request::List {
            grant: r.u64()?,
            path: path(&mut r)?,
        },
        ty::CREATE => Request::Create {
            grant: r.u64()?,
            path: path(&mut r)?,
            kind: match r.u8()? {
                0 => Kind::File,
                1 => Kind::Dir,
                _ => return Err(DecodeError::Invalid("kind")),
            },
            name: name(&mut r)?,
        },
        ty::DELETE => Request::Delete {
            grant: r.u64()?,
            path: path(&mut r)?,
        },
        ty::RENAME => Request::Rename {
            grant: r.u64()?,
            path: path(&mut r)?,
            name: name(&mut r)?,
        },
        ty::FORGET => Request::Forget { grant: r.u64()? },
        ty::IMPORT => Request::Import {
            name: name(&mut r)?,
        },
        t => return Err(DecodeError::UnknownType(t)),
    };
    r.end()?;
    Ok(req)
}

fn put_text(w: &mut Writer, s: &str) {
    w.u16(s.len() as u16);
    w.0.extend_from_slice(s.as_bytes());
}

pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let mut w;
    match req {
        Request::Import { name } => {
            w = Writer::message(ty::IMPORT, tag);
            put_text(&mut w, name);
        }
        Request::Pick(p) => {
            w = Writer::message(ty::PICK, tag);
            w.u32(p.request)
                .u8(p.mode as u8)
                .u8(p.write as u8)
                .u32(p.window)
                .name(&p.title)
                .name(&p.name)
                .u8(p.mimes.len() as u8);
            for m in &p.mimes {
                w.name(m);
            }
        }
        Request::Open { grant, mode, path } => {
            w = Writer::message(ty::OPEN, tag);
            w.u64(*grant).u8(*mode as u8);
            put_text(&mut w, path);
        }
        Request::Stat { grant, path }
        | Request::List { grant, path }
        | Request::Delete { grant, path } => {
            let t = match req {
                Request::Stat { .. } => ty::STAT,
                Request::List { .. } => ty::LIST,
                _ => ty::DELETE,
            };
            w = Writer::message(t, tag);
            w.u64(*grant);
            put_text(&mut w, path);
        }
        Request::Create {
            grant,
            path,
            kind,
            name,
        } => {
            w = Writer::message(ty::CREATE, tag);
            w.u64(*grant);
            put_text(&mut w, path);
            w.u8(*kind as u8);
            put_text(&mut w, name);
        }
        Request::Rename { grant, path, name } => {
            w = Writer::message(ty::RENAME, tag);
            w.u64(*grant);
            put_text(&mut w, path);
            put_text(&mut w, name);
        }
        Request::Forget { grant } => {
            w = Writer::message(ty::FORGET, tag);
            w.u64(*grant);
        }
    }
    w.into_vec()
}

/// File metadata, as STAT's reply and each LIST entry: `u8 kind`, `u64
/// size`, `i64 mtime_ms`, `u8 writable`, `u16 len` + name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Meta {
    pub kind: Kind,
    pub size: u64,
    pub mtime_ms: i64,
    pub writable: bool,
    pub name: String,
}

impl Meta {
    pub fn put(&self, w: &mut Writer) {
        w.u8(self.kind as u8)
            .u64(self.size)
            .u64(self.mtime_ms as u64)
            .u8(self.writable as u8);
        put_text(w, &self.name);
    }
}

pub mod event {
    use super::*;

    /// One picked item: `u32 request`, `u64 grant`, `u8 kind`, `u8
    /// writable`, name (display name, truncated).
    pub fn picked(request: u32, grant: u64, kind: Kind, writable: bool, name: &str) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(request)
            .u64(grant)
            .u8(kind as u8)
            .u8(writable as u8)
            .name(crate::ipc::wire::truncate(name, crate::ipc::wire::MAX_NAME));
        w.into_vec()
    }

    /// The pick ended: `u32 request`, `i32 status` (0, -ECANCELED, -EIO),
    /// `u32 count`.
    pub fn done(request: u32, status: i32, count: u32) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(request).i32(status).u32(count);
        w.into_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::wire::{HDR, decode_header};

    fn roundtrip(b: &[u8]) -> Result<Request, DecodeError> {
        let h = decode_header(b)?;
        decode(h.ty, &b[HDR..])
    }

    #[test]
    fn imports_roundtrip_and_reject_bad_names() {
        let req = Request::Import {
            name: "note.md".into(),
        };
        let b = encode(1, &req);
        assert_eq!(decode(ty::IMPORT, &b[crate::ipc::wire::HDR..]), Ok(req));
        assert!(decode(ty::IMPORT, &[0, 0]).is_err());
        let b = encode(
            1,
            &Request::Import {
                name: "../note.md".into(),
            },
        );
        assert!(decode(ty::IMPORT, &b[crate::ipc::wire::HDR..]).is_err());
    }

    #[test]
    fn roundtrips() {
        for r in [
            Request::Pick(Pick {
                request: 7,
                mode: PickMode::Save,
                write: true,
                window: 3,
                title: "Export".into(),
                name: "notes.md".into(),
                mimes: vec!["text/markdown".into(), "text/*".into(), "*/*".into()],
            }),
            Request::Open {
                grant: 0x1234_5678_9abc_def0,
                mode: OpenMode::ReadWrite,
                path: "sub/dir/ünï.txt".into(),
            },
            Request::Stat {
                grant: 1,
                path: String::new(),
            },
            Request::List {
                grant: 1,
                path: "a".into(),
            },
            Request::Create {
                grant: 1,
                path: "a".into(),
                kind: Kind::Dir,
                name: "new".into(),
            },
            Request::Delete {
                grant: 1,
                path: "a/b".into(),
            },
            Request::Rename {
                grant: 1,
                path: "a/b".into(),
                name: "c".into(),
            },
            Request::Forget { grant: 9 },
        ] {
            assert_eq!(roundtrip(&encode(1, &r)), Ok(r));
        }
    }

    #[test]
    fn paths_and_names() {
        for ok in ["", "a", "a/b", "a b/c.d", "...", ".x"] {
            assert!(valid_path(ok), "{ok:?}");
        }
        for bad in [
            "/",
            "/a",
            "a/",
            "a//b",
            ".",
            "..",
            "a/../b",
            "a/./b",
            "a\0b",
            &"x".repeat(401),
        ] {
            assert!(!valid_path(bad), "{bad:?}");
        }
        for bad in ["", ".", "..", "a/b", "a\0", &"x".repeat(256)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        for bad in ["text", "/x", "text/", "a b/c", "text/plain;x=1"] {
            assert!(!mime(bad), "{bad:?}");
        }
        let traversal = Request::Open {
            grant: 1,
            mode: OpenMode::Read,
            path: "../etc/passwd".into(),
        };
        assert!(roundtrip(&encode(1, &traversal)).is_err());
    }
}
