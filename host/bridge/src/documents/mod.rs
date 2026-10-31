//! The documents service (id 4): host files and folders the user picks in the
//! FileChooser portal, for the Innkeeper System app's document picker and
//! DocumentsProvider. Messages: [`proto`].
//!
//! The bridge is the only one that knows host paths. Android gets opaque
//! grant ids and file descriptors:
//!
//! - Portal grants expose only what the user picked. CLI imports hold just the
//!   supplied file fd, read-only, until session exit or FORGET. Portal grants persist (the provider's URIs outlive the session)
//!   in the profile's state directory, and FORGET drops one.
//! - A file grant opens only that file. A folder grant opens paths beneath it
//!   through `openat2(RESOLVE_BENEATH | RESOLVE_NO_MAGICLINKS)` from the
//!   folder's fd, so `..`, absolute symlinks and `/proc` links can't leave
//!   it; paths are also checked for `.`/`..` before that.
//! - Only regular files are opened (FIFOs and devices are refused before any
//!   blocking open), and writes need a grant made for writing.

pub mod portal;
pub mod proto;

use std::ffi::OsStr;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use rustix::fs::{Mode, OFlags, ResolveFlags};

use crate::ipc::blob;
use crate::ipc::server::{Events, Peer, Reply, Service};
use crate::ipc::wire::Writer;
use crate::ipc::wire::errno::*;
use proto::{Kind, Meta, OpenMode, Pick, PickMode, Request};

/// Grants kept at most; the oldest go first.
const MAX_GRANTS: usize = 4096;
/// Entries one LIST returns at most.
const MAX_LIST: usize = 10_000;
/// Picks waiting for the user, bridge-wide. A pick outlives its connection
/// (its dialog stays up until the user answers), so a per-connection limit
/// would let a client reconnect for more.
const MAX_PICKS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub path: PathBuf,
    pub dir: bool,
    pub write: bool,
}

/// Grants by id, in creation order, saved to `file` after each change.
pub struct Store {
    file: Option<PathBuf>,
    grants: Mutex<Vec<(u64, Grant)>>,
    /// Desktop Open With files: held by fd, read-only, until forgotten or session exit.
    files: Mutex<Vec<(u64, OwnedFd, String)>>,
    dbus: OnceLock<Option<zbus::blocking::Connection>>,
    picks: AtomicUsize,
}

/// One of the MAX_PICKS outstanding picks, released when dropped.
pub struct PickSlot(Arc<Store>);

impl Drop for PickSlot {
    fn drop(&mut self) {
        self.0.picks.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Store {
    pub fn new(file: Option<PathBuf>) -> Arc<Store> {
        let grants = file.as_deref().map(load).unwrap_or_default();
        Arc::new(Store {
            file,
            grants: Mutex::new(grants),
            files: Mutex::new(Vec::new()),
            dbus: OnceLock::new(),
            picks: AtomicUsize::new(0),
        })
    }

    /// A slot for one more pick, unless MAX_PICKS are outstanding.
    pub fn pick_slot(self: &Arc<Self>) -> Option<PickSlot> {
        let ok = self
            .picks
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < MAX_PICKS).then_some(n + 1)
            })
            .is_ok();
        ok.then(|| PickSlot(self.clone()))
    }

    fn dbus(&self) -> Option<&zbus::blocking::Connection> {
        self.dbus
            .get_or_init(|| {
                zbus::blocking::Connection::session()
                    .map_err(|e| crate::warn!("documents: no session bus: {e}"))
                    .ok()
            })
            .as_ref()
    }

    pub fn get(&self, id: u64) -> Option<Grant> {
        let g = self.grants.lock().unwrap();
        g.iter().find(|(i, _)| *i == id).map(|(_, g)| g.clone())
    }

    pub fn add(&self, grant: Grant) -> u64 {
        let id = random_id();
        let mut g = self.grants.lock().unwrap();
        // Picking the same thing again reuses its grant (widened to write).
        if let Some((i, old)) = g
            .iter_mut()
            .find(|(_, o)| o.path == grant.path && o.dir == grant.dir)
        {
            old.write |= grant.write;
            let i = *i;
            self.save(&g);
            return i;
        }
        if g.len() >= MAX_GRANTS {
            g.remove(0);
        }
        g.push((id, grant));
        self.save(&g);
        id
    }

    pub fn forget(&self, id: u64) {
        self.files.lock().unwrap().retain(|(i, _, _)| *i != id);
        let mut g = self.grants.lock().unwrap();
        g.retain(|(i, _)| *i != id);
        self.save(&g);
    }

    /// Import only the supplied regular file; no host path reaches Android.
    pub fn import_file(&self, fd: OwnedFd, name: String) -> Result<u64, i32> {
        let st = rustix::fs::fstat(&fd).map_err(errno)?;
        if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::RegularFile {
            return Err(-EINVAL);
        }
        let id = random_id();
        let mut files = self.files.lock().unwrap();
        if files.len() >= MAX_GRANTS {
            files.remove(0);
        }
        files.push((id, fd, name));
        Ok(id)
    }

    fn file_request(&self, req: &Request) -> Option<Reply> {
        let (id, path, mode) = match req {
            Request::Open { grant, mode, path } => (*grant, path, Some(*mode)),
            Request::Stat { grant, path } => (*grant, path, None),
            _ => return None,
        };
        let files = self.files.lock().unwrap();
        let (_, fd, name) = files.iter().find(|(i, _, _)| *i == id)?;
        if !path.is_empty() {
            return Some(Reply::err(-ENOENT));
        }
        if mode.is_some_and(OpenMode::writes) {
            return Some(Reply::err(-EACCES));
        }
        let grant = Grant {
            path: format!("/proc/self/fd/{}", fd.as_raw_fd()).into(),
            dir: false,
            write: false,
        };
        Some(match mode {
            Some(mode) => match Fs(&grant).open("", mode) {
                Ok(fd) => {
                    let mut r = Reply::ok();
                    r.fds.push(fd);
                    r
                }
                Err(e) => Reply::err(e),
            },
            None => match Fs(&grant).stat("") {
                Ok(mut meta) => {
                    meta.name = name.clone();
                    meta_reply(&meta)
                }
                Err(e) => Reply::err(e),
            },
        })
    }

    fn save(&self, g: &[(u64, Grant)]) {
        let Some(f) = &self.file else { return };
        let mut s = String::new();
        for (id, gr) in g {
            s.push_str(&format!(
                "{id:016x} {} {} {}\n",
                if gr.dir { 'd' } else { 'f' },
                if gr.write { 'w' } else { 'r' },
                hex(gr.path.as_os_str().as_bytes())
            ));
        }
        let tmp = f.with_extension("tmp");
        if let Err(e) = std::fs::write(&tmp, s).and_then(|_| std::fs::rename(&tmp, f)) {
            crate::warn!("documents: saving grants: {e}");
        }
    }
}

fn load(f: &Path) -> Vec<(u64, Grant)> {
    let Ok(text) = std::fs::read_to_string(f) else {
        return vec![];
    };
    text.lines()
        .filter_map(|l| {
            let mut it = l.split(' ');
            let id = u64::from_str_radix(it.next()?, 16).ok()?;
            let dir = it.next()? == "d";
            let write = it.next()? == "w";
            let path = PathBuf::from(OsStr::from_bytes(&unhex(it.next()?)?));
            path.is_absolute()
                .then_some((id, Grant { path, dir, write }))
        })
        .take(MAX_GRANTS)
        .collect()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn random_id() -> u64 {
    let mut b = [0u8; 8];
    // getrandom(2) never fails for 8 bytes once the pool is ready.
    let _ = rustix::rand::getrandom(&mut b, rustix::rand::GetRandomFlags::empty());
    u64::from_le_bytes(b) | 1
}

fn errno(e: rustix::io::Errno) -> i32 {
    -e.raw_os_error()
}

/// File operations on one grant. Paths are already validated
/// (`proto::valid_path`).
pub struct Fs<'a>(pub &'a Grant);

impl Fs<'_> {
    /// The grant's folder, as an `O_PATH` fd.
    fn root(&self) -> Result<OwnedFd, i32> {
        rustix::fs::open(
            &self.0.path,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(errno)
    }

    /// Opens `rel` with `flags`: the grant itself (empty path), or a path
    /// beneath a folder grant.
    fn open_at(&self, rel: &str, flags: OFlags, mode: Mode) -> Result<OwnedFd, i32> {
        // openat2 refuses flags that don't apply to O_PATH.
        let flags = if flags.contains(OFlags::PATH) {
            flags | OFlags::CLOEXEC
        } else {
            flags | OFlags::CLOEXEC | OFlags::NOCTTY
        };
        if rel.is_empty() {
            return rustix::fs::open(&self.0.path, flags, mode).map_err(errno);
        }
        if !self.0.dir {
            return Err(-ENOENT);
        }
        let root = self.root()?;
        rustix::fs::openat2(
            &root,
            rel,
            flags,
            mode,
            ResolveFlags::BENEATH | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(errno)
    }

    /// The folder holding `rel`, and `rel`'s last name.
    fn parent<'p>(&self, rel: &'p str) -> Result<(OwnedFd, &'p str), i32> {
        if rel.is_empty() || !self.0.dir {
            return Err(-EPERM);
        }
        let (dir, name) = rel.rsplit_once('/').unwrap_or(("", rel));
        let fd = self.open_at(dir, OFlags::PATH | OFlags::DIRECTORY, Mode::empty())?;
        Ok((fd, name))
    }

    fn need_write(&self) -> Result<(), i32> {
        if self.0.write { Ok(()) } else { Err(-EACCES) }
    }

    /// A regular file, opened for `mode`.
    pub fn open(&self, rel: &str, mode: OpenMode) -> Result<OwnedFd, i32> {
        if mode.writes() {
            self.need_write()?;
        }
        let flags = match mode {
            OpenMode::Read => OFlags::RDONLY,
            OpenMode::Write => OFlags::WRONLY | OFlags::TRUNC,
            OpenMode::ReadWrite => OFlags::RDWR,
            OpenMode::Append => OFlags::WRONLY | OFlags::APPEND,
        };
        // Check the type through O_PATH first: opening a FIFO for reading
        // would block, and truncating must only happen to regular files.
        let probe = self.open_at(rel, OFlags::PATH, Mode::empty())?;
        let st = rustix::fs::fstat(&probe).map_err(errno)?;
        if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::RegularFile {
            return Err(-EINVAL);
        }
        let fd = self.open_at(rel, flags | OFlags::NONBLOCK, Mode::empty())?;
        let st2 = rustix::fs::fstat(&fd).map_err(errno)?;
        if (st2.st_dev, st2.st_ino) != (st.st_dev, st.st_ino) {
            return Err(-EAGAIN); // replaced between the two opens
        }
        let fl = rustix::fs::fcntl_getfl(&fd).map_err(errno)?;
        rustix::fs::fcntl_setfl(&fd, fl - OFlags::NONBLOCK).map_err(errno)?;
        Ok(fd)
    }

    fn meta_of(&self, st: &rustix::fs::Stat, name: &str) -> Option<Meta> {
        let kind = match rustix::fs::FileType::from_raw_mode(st.st_mode) {
            rustix::fs::FileType::RegularFile => Kind::File,
            rustix::fs::FileType::Directory => Kind::Dir,
            _ => return None,
        };
        Some(Meta {
            kind,
            size: if kind == Kind::File {
                st.st_size as u64
            } else {
                0
            },
            mtime_ms: st.st_mtime * 1000 + st.st_mtime_nsec as i64 / 1_000_000,
            writable: self.0.write,
            name: name.to_string(),
        })
    }

    pub fn stat(&self, rel: &str) -> Result<Meta, i32> {
        let fd = self.open_at(rel, OFlags::PATH, Mode::empty())?;
        let st = rustix::fs::fstat(&fd).map_err(errno)?;
        let name = match rel.rsplit_once('/') {
            Some((_, n)) => n.to_string(),
            None if !rel.is_empty() => rel.to_string(),
            None => self
                .0
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "/".into()),
        };
        self.meta_of(&st, &name).ok_or(-EINVAL)
    }

    /// The folder's regular files and folders (symlinks and special files
    /// are left out), at most MAX_LIST.
    pub fn list(&self, rel: &str) -> Result<Vec<Meta>, i32> {
        if !self.0.dir {
            return Err(-ENOTDIR);
        }
        let fd = self.open_at(rel, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())?;
        let dir = rustix::fs::Dir::read_from(&fd).map_err(errno)?;
        let mut out = Vec::new();
        for e in dir {
            let Ok(e) = e else { break };
            let name = e.file_name().to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            let Ok(name) = std::str::from_utf8(name) else {
                continue; // not representable in a document id
            };
            let Ok(st) = rustix::fs::statat(&fd, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW)
            else {
                continue;
            };
            if let Some(m) = self.meta_of(&st, name) {
                out.push(m);
                if out.len() >= MAX_LIST {
                    break;
                }
            }
        }
        Ok(out)
    }

    /// Creates a file or folder `name` in `rel`; if the name is taken, tries
    /// "name (1).ext" and so on. Returns the name used.
    pub fn create(&self, rel: &str, kind: Kind, name: &str) -> Result<String, i32> {
        self.need_write()?;
        if !self.0.dir {
            return Err(-ENOTDIR);
        }
        let dir = self.open_at(rel, OFlags::PATH | OFlags::DIRECTORY, Mode::empty())?;
        let (stem, ext) = match name.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() && kind == Kind::File => (s, Some(e)),
            _ => (name, None),
        };
        for n in 0..100 {
            let candidate = match (n, ext) {
                (0, _) => name.to_string(),
                (n, Some(e)) => format!("{stem} ({n}).{e}"),
                (n, None) => format!("{stem} ({n})"),
            };
            if !proto::valid_name(&candidate) {
                return Err(-ENAMETOOLONG);
            }
            let r = match kind {
                Kind::File => rustix::fs::openat(
                    &dir,
                    candidate.as_str(),
                    OFlags::CREATE | OFlags::EXCL | OFlags::WRONLY | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o644),
                )
                .map(drop),
                Kind::Dir => {
                    rustix::fs::mkdirat(&dir, candidate.as_str(), Mode::from_raw_mode(0o755))
                }
            };
            match r {
                Ok(()) => return Ok(candidate),
                Err(rustix::io::Errno::EXIST) => continue,
                Err(e) => return Err(errno(e)),
            }
        }
        Err(-EEXIST)
    }

    /// Deletes a file or an empty folder beneath a folder grant.
    pub fn delete(&self, rel: &str) -> Result<(), i32> {
        self.need_write()?;
        let (dir, name) = self.parent(rel)?;
        let st =
            rustix::fs::statat(&dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW).map_err(errno)?;
        let flags =
            if rustix::fs::FileType::from_raw_mode(st.st_mode) == rustix::fs::FileType::Directory {
                rustix::fs::AtFlags::REMOVEDIR
            } else {
                rustix::fs::AtFlags::empty()
            };
        rustix::fs::unlinkat(&dir, name, flags).map_err(errno)
    }

    /// Renames within the same folder, never replacing.
    pub fn rename(&self, rel: &str, to: &str) -> Result<(), i32> {
        self.need_write()?;
        let (dir, name) = self.parent(rel)?;
        rustix::fs::renameat_with(&dir, name, &dir, to, rustix::fs::RenameFlags::NOREPLACE)
            .map_err(errno)
    }
}

pub struct DocumentsService {
    store: Arc<Store>,
    events: Events,
    label: String,
    /// False in a test session: picks cancel without asking the portal.
    desktop: bool,
}

impl DocumentsService {
    pub fn new(store: Arc<Store>, peer: &Peer, events: Events, desktop: bool) -> DocumentsService {
        DocumentsService {
            store,
            events,
            label: peer.label.clone(),
            desktop,
        }
    }

    fn pick(&mut self, p: Pick) -> Reply {
        if !self.desktop {
            crate::info!("documents: {}: pick cancelled (test session)", self.label);
            let _ = self.events.try_send(
                proto::ty::PICK_DONE,
                &proto::event::done(p.request, -ECANCELED, 0),
                &[],
            );
            return Reply::ok();
        }
        if self.store.dbus().is_none() {
            return Reply::err(-ENOSYS);
        }
        let Some(slot) = self.store.pick_slot() else {
            return Reply::err(-EBUSY);
        };
        let (store, events, label) = (self.store.clone(), self.events.clone(), self.label.clone());
        let spawned = std::thread::Builder::new()
            .name("documents-pick".into())
            .spawn(move || {
                run_pick(&store, &events, &p, &label);
                drop(slot);
            });
        match spawned {
            Ok(_) => Reply::ok(),
            Err(_) => Reply::err(-EAGAIN),
        }
    }
}

fn run_pick(store: &Store, events: &Events, p: &Pick, label: &str) {
    let parent = crate::display::foreign_handle(p.window)
        .map(|h| format!("wayland:{h}"))
        .unwrap_or_default();
    crate::info!(
        "documents: {label}: pick {:?} (window {}, parent {:?})",
        p.mode,
        p.window,
        parent
    );
    let Some(conn) = store.dbus() else { return };
    let result = portal::pick(conn, &parent, p);
    let mut count = 0;
    let status = match result {
        Ok(paths) => {
            for path in paths {
                match grant_for(&path, p) {
                    Ok(g) => {
                        let (kind, write) = (if g.dir { Kind::Dir } else { Kind::File }, g.write);
                        let name = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let id = store.add(g);
                        crate::info!("documents: {label}: granted {kind:?} {id:016x}");
                        let body = proto::event::picked(p.request, id, kind, write, &name);
                        let _ = events.try_send(proto::ty::PICKED, &body, &[]);
                        count += 1;
                    }
                    Err(e) => crate::warn!("documents: {}: {e}", path.display()),
                }
            }
            if count == 0 { -EIO } else { 0 }
        }
        Err(e) => e,
    };
    let _ = events.try_send(
        proto::ty::PICK_DONE,
        &proto::event::done(p.request, status, count),
        &[],
    );
}

/// The grant for what the user picked, after checking it is what was asked
/// for. A saved file is created (empty) right away, like Android's
/// CREATE_DOCUMENT does.
fn grant_for(path: &Path, p: &Pick) -> Result<Grant, String> {
    if !path.is_absolute() {
        return Err("not absolute".into());
    }
    let dir = p.mode == PickMode::Folder;
    if p.mode == PickMode::Save {
        let fd = rustix::fs::open(
            path,
            OFlags::CREATE | OFlags::WRONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY,
            Mode::from_raw_mode(0o644),
        )
        .map_err(|e| e.to_string())?;
        let st = rustix::fs::fstat(&fd).map_err(|e| e.to_string())?;
        if rustix::fs::FileType::from_raw_mode(st.st_mode) != rustix::fs::FileType::RegularFile {
            return Err("not a regular file".into());
        }
        return Ok(Grant {
            path: path.to_path_buf(),
            dir: false,
            write: true,
        });
    }
    let st = rustix::fs::stat(path).map_err(|e| e.to_string())?;
    let ft = rustix::fs::FileType::from_raw_mode(st.st_mode);
    let ok = if dir {
        ft == rustix::fs::FileType::Directory
    } else {
        ft == rustix::fs::FileType::RegularFile
    };
    if !ok {
        return Err(format!(
            "not a {}",
            if dir { "folder" } else { "regular file" }
        ));
    }
    Ok(Grant {
        path: path.to_path_buf(),
        dir,
        write: p.write,
    })
}

fn meta_reply(m: &Meta) -> Reply {
    let mut w = Writer::default();
    m.put(&mut w);
    Reply::with_body(w.into_vec())
}

impl Service for DocumentsService {
    fn handle(&mut self, t: u16, body: &[u8], mut fds: Vec<OwnedFd>) -> Reply {
        let req = match proto::decode(t, body) {
            Ok(r) => r,
            Err(e) => return Reply::err(e.status()),
        };
        if fds.len() != usize::from(matches!(req, Request::Import { .. })) {
            return Reply::err(-EPROTO);
        }
        if let Request::Import { name } = req {
            return match self
                .store
                .import_file(fds.pop().expect("checked above"), name)
            {
                Ok(id) => {
                    let mut w = Writer::default();
                    w.u64(id);
                    Reply::with_body(w.into_vec())
                }
                Err(e) => Reply::err(e),
            };
        }
        if let Some(r) = self.store.file_request(&req) {
            return r;
        }
        let grant_of = |id: u64| self.store.get(id).ok_or(-ENOENT);
        let r = match req {
            Request::Import { .. } => unreachable!("handled above"),
            Request::Pick(p) => return self.pick(p),
            Request::Forget { grant } => {
                self.store.forget(grant);
                return Reply::ok();
            }
            Request::Open { grant, mode, path } => grant_of(grant).and_then(|g| {
                let fd = Fs(&g).open(&path, mode)?;
                let mut r = Reply::ok();
                r.fds.push(fd);
                Ok(r)
            }),
            Request::Stat { grant, path } => {
                grant_of(grant).and_then(|g| Ok(meta_reply(&Fs(&g).stat(&path)?)))
            }
            Request::List { grant, path } => grant_of(grant).and_then(|g| {
                let list = Fs(&g).list(&path)?;
                let mut w = Writer::default();
                for m in &list {
                    m.put(&mut w);
                }
                let b = w.into_vec();
                let fd = blob::create("innkeeper-list", &b).map_err(|_| -ENOMEM)?;
                let mut out = Writer::default();
                out.u32(b.len() as u32).u32(list.len() as u32);
                let mut r = Reply::with_body(out.into_vec());
                r.fds.push(fd);
                Ok(r)
            }),
            Request::Create {
                grant,
                path,
                kind,
                name,
            } => grant_of(grant).and_then(|g| {
                let n = Fs(&g).create(&path, kind, &name)?;
                let mut w = Writer::default();
                w.u16(n.len() as u16);
                w.0.extend_from_slice(n.as_bytes());
                Ok(Reply::with_body(w.into_vec()))
            }),
            Request::Delete { grant, path } => {
                grant_of(grant).and_then(|g| Fs(&g).delete(&path).map(|_| Reply::ok()))
            }
            Request::Rename { grant, path, name } => {
                grant_of(grant).and_then(|g| Fs(&g).rename(&path, &name).map(|_| Reply::ok()))
            }
        };
        r.unwrap_or_else(Reply::err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn test_pick_cancels_without_a_portal() {
        use crate::ipc::{transport, wire};
        use rustix::net::{AddressFamily, SocketFlags, SocketType};
        let (server, client) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC | SocketFlags::NONBLOCK,
            None,
        )
        .unwrap();
        let store = Store::new(None);
        let mut service = DocumentsService::new(
            store.clone(),
            &Peer::test(),
            Events::new(Arc::new(server)),
            false,
        );
        for mode in [
            PickMode::Open,
            PickMode::OpenMultiple,
            PickMode::Save,
            PickMode::Folder,
        ] {
            let reply = service.pick(Pick {
                request: 42,
                mode,
                write: true,
                window: 0,
                title: String::new(),
                name: String::new(),
                mimes: vec![],
            });
            assert_eq!(reply.status, 0);
            let mut buf = [0; wire::MAX_MSG + 1];
            let packet = transport::recv(&client, &mut buf).unwrap().unwrap();
            let expected = proto::event::done(42, -ECANCELED, 0);
            let mut message = Writer::message(proto::ty::PICK_DONE, 0);
            message.0.extend_from_slice(&expected);
            assert_eq!(&buf[..packet.len], message.0);
            assert!(packet.fds.is_empty());
        }
        assert!(store.dbus.get().is_none());
        assert_eq!(store.picks.load(Ordering::SeqCst), 0);
        assert!(store.grants.lock().unwrap().is_empty());
    }

    #[test]
    fn imported_files_keep_contents_and_name_without_path_or_write_access() {
        let dir = crate::test_dir("import-file");
        let path = dir.join("note.md");
        std::fs::write(&path, "# imported file").unwrap();
        let store = Store::new(None);
        let fd = std::fs::File::open(&path).unwrap().into();
        let id = store.import_file(fd, "note.md".into()).unwrap();
        std::fs::remove_file(&path).unwrap();
        for _ in 0..2 {
            let mut reply = store
                .file_request(&Request::Open {
                    grant: id,
                    mode: OpenMode::Read,
                    path: String::new(),
                })
                .unwrap();
            assert_eq!(reply.status, 0);
            let mut text = String::new();
            std::fs::File::from(reply.fds.pop().unwrap())
                .read_to_string(&mut text)
                .unwrap();
            assert_eq!(text, "# imported file");
        }
        let reply = store
            .file_request(&Request::Stat {
                grant: id,
                path: String::new(),
            })
            .unwrap();
        assert!(reply.body.ends_with(b"note.md"));
        assert_eq!(
            store
                .file_request(&Request::Open {
                    grant: id,
                    mode: OpenMode::ReadWrite,
                    path: String::new()
                })
                .unwrap()
                .status,
            -EACCES
        );
        assert_eq!(
            store
                .file_request(&Request::Open {
                    grant: id,
                    mode: OpenMode::Read,
                    path: "other.md".into()
                })
                .unwrap()
                .status,
            -ENOENT
        );
        assert!(
            store
                .import_file(
                    std::fs::File::open(&dir).unwrap().into(),
                    "directory".into()
                )
                .is_err()
        );
        store.forget(id);
        assert!(
            store
                .file_request(&Request::Stat {
                    grant: id,
                    path: String::new()
                })
                .is_none()
        );
    }

    /// Outstanding picks are counted for the whole bridge (every connection
    /// shares the Store), and stay charged until the pick finishes, not
    /// until its connection closes.
    #[test]
    fn picks_are_capped_bridge_wide() {
        let s = Store::new(None);
        let held: Vec<PickSlot> = (0..MAX_PICKS).map(|_| s.pick_slot().unwrap()).collect();
        assert!(s.pick_slot().is_none());
        drop(held);
        assert!(s.pick_slot().is_some());
    }

    fn tmp(name: &str) -> PathBuf {
        crate::test_dir(&format!("documents-{name}"))
    }

    #[test]
    fn folder_grant_stays_beneath() {
        let d = tmp("tree");
        let root = d.join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/a.txt"), "hello").unwrap();
        std::fs::write(d.join("secret"), "no").unwrap();
        std::os::unix::fs::symlink("/etc/passwd", root.join("abs")).unwrap();
        std::os::unix::fs::symlink("../secret", root.join("rel")).unwrap();
        rustix::fs::mknodat(
            rustix::fs::CWD,
            root.join("fifo").as_path(),
            rustix::fs::FileType::Fifo,
            Mode::from_raw_mode(0o644),
            0,
        )
        .unwrap();
        let g = Grant {
            path: root.clone(),
            dir: true,
            write: false,
        };
        let fs = Fs(&g);
        let mut s = String::new();
        std::fs::File::from(fs.open("sub/a.txt", OpenMode::Read).unwrap())
            .read_to_string(&mut s)
            .unwrap();
        assert_eq!(s, "hello");
        // Symlinks out of the folder, FIFOs and writes without a write grant
        // are refused.
        assert_eq!(fs.open("abs", OpenMode::Read).err(), Some(-EXDEV));
        assert_eq!(fs.open("rel", OpenMode::Read).err(), Some(-EXDEV));
        assert_eq!(fs.open("fifo", OpenMode::Read).err(), Some(-EINVAL));
        assert_eq!(fs.open("sub/a.txt", OpenMode::Write).err(), Some(-EACCES));
        assert_eq!(fs.create("", Kind::File, "x").err(), Some(-EACCES));
        let names: Vec<_> = fs.list("").unwrap().into_iter().map(|m| m.name).collect();
        assert_eq!(names, vec!["sub".to_string()]);
        assert_eq!(fs.stat("sub/a.txt").unwrap().size, 5);

        let w = Grant {
            write: true,
            ..g.clone()
        };
        let fs = Fs(&w);
        assert_eq!(fs.create("sub", Kind::File, "a.txt").unwrap(), "a (1).txt");
        assert_eq!(fs.create("", Kind::Dir, "new").unwrap(), "new");
        fs.rename("sub/a (1).txt", "b.txt").unwrap();
        assert_eq!(fs.rename("sub/b.txt", "a.txt").err(), Some(-EEXIST));
        fs.delete("sub/b.txt").unwrap();
        fs.delete("new").unwrap();
        assert_eq!(fs.delete("").err(), Some(-EPERM));
        assert!(d.join("secret").exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn file_grant_is_only_that_file() {
        let d = tmp("file");
        std::fs::write(d.join("f.txt"), "x").unwrap();
        let g = Grant {
            path: d.join("f.txt"),
            dir: false,
            write: true,
        };
        assert!(Fs(&g).open("", OpenMode::ReadWrite).is_ok());
        assert_eq!(Fs(&g).open("other", OpenMode::Read).err(), Some(-ENOENT));
        assert_eq!(Fs(&g).list("").err(), Some(-ENOTDIR));
        assert_eq!(Fs(&g).delete("").err(), Some(-EPERM));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn grants_persist() {
        let d = tmp("store");
        let f = d.join("grants");
        let s = Store::new(Some(f.clone()));
        let g = Grant {
            path: "/home/u/a b\nc".into(),
            dir: true,
            write: false,
        };
        let id = s.add(g.clone());
        assert_eq!(
            s.add(Grant {
                write: true,
                ..g.clone()
            }),
            id
        );
        let s2 = Store::new(Some(f));
        assert_eq!(s2.get(id), Some(Grant { write: true, ..g }));
        s2.forget(id);
        assert_eq!(Store::new(s2.file.clone()).get(id), None);
        let _ = std::fs::remove_dir_all(&d);
    }
}
