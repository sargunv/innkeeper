//! Bounded multi-file clipboard payloads and cache copies. File-transfer portal
//! keys let file managers cross the Flatpak boundary without broad filesystem access.
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, ensure};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

use crate::ipc::blob::{Doc, DocWriter};
use crate::ipc::wire::Writer;
use crate::system::proto::limits;

pub const PORTAL_TYPE: &str = "application/vnd.portal.filetransfer";
const MAX_NAME: usize = 255;
const MAX_MIME: usize = 255;
pub const MAX_BYTES: usize = (16 << 20) - 1024;
pub const MAX_FILES: usize = 16;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipFile {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// One selection, bounded including every file's metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipFiles(pub Vec<ClipFile>);

impl ClipFiles {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.u32(self.0.len() as u32);
        for file in &self.0 {
            w.text(&file.name)
                .text(&file.mime)
                .u32(file.bytes.len() as u32);
            w.0.extend_from_slice(&file.bytes);
        }
        w.into_vec()
    }

    pub fn encoded_len(&self) -> usize {
        4 + self
            .0
            .iter()
            .map(|f| 12 + f.name.len() + f.mime.len() + f.bytes.len())
            .sum::<usize>()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= limits::CLIP_FILE as usize,
            "file clip too large"
        );
        let mut d = Doc::new(bytes);
        let count = d.0.u32()? as usize;
        ensure!((1..=MAX_FILES).contains(&count), "file count");
        let mut files = Vec::new();
        for _ in 0..count {
            let name = d.text(MAX_NAME)?;
            let mime = d.text(MAX_MIME)?;
            ensure!(
                !name.is_empty() && name != "." && name != ".." && !name.contains(['/', '\\']),
                "file name"
            );
            ensure!(
                mime.contains('/') && !mime.chars().any(char::is_whitespace),
                "file MIME type"
            );
            let n = d.0.u32()? as usize;
            ensure!(n <= MAX_BYTES, "file too large");
            let bytes = d.0.take(n)?.to_vec();
            files.push(ClipFile { name, mime, bytes });
        }
        d.end()?;
        Ok(Self(files))
    }
}

pub fn cache_dir() -> Result<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .context("no cache directory")?;
    let instance = std::env::var("LINUXAPK_INSTANCE").unwrap_or_else(|_| "main".into());
    let dir = base.join(format!("linuxapk-{instance}/clipboard"));
    // Previous process's selections are no longer being served.
    if dir.exists() {
        std::fs::remove_dir_all(&dir)?;
    }
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Deleted when the source is replaced/cancelled. Never reuse a pathname: a
/// recipient may still have a URI from an earlier selection.
pub struct CachedFiles {
    dir: PathBuf,
    paths: Vec<PathBuf>,
    portal: Option<(Connection, String)>,
}

impl CachedFiles {
    pub fn create(cache: &Path, clip: &ClipFiles) -> Result<Self> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let dir = cache.join(SERIAL.fetch_add(1, Ordering::Relaxed).to_string());
        std::fs::create_dir(&dir)?;
        let mut file = Self {
            paths: Vec::new(),
            dir,
            portal: None,
        };
        for (index, item) in clip.0.iter().enumerate() {
            // Separate directories preserve duplicate basenames in a selection.
            let parent = file.dir.join(index.to_string());
            std::fs::create_dir(&parent)?;
            let path = parent.join(&item.name);
            std::fs::write(&path, &item.bytes)?;
            file.paths.push(path);
        }
        // A portal may be absent in a minimal nested desktop. The URI remains
        // useful to unsandboxed receivers on the same desktop.
        if let Ok(conn) = Connection::session() {
            let result = (|| -> Result<String> {
                let proxy = portal(&conn)?;
                let key: String = proxy.call(
                    "StartTransfer",
                    &(std::collections::HashMap::from([
                        ("writable", Value::from(false)),
                        ("autostop", Value::from(false)),
                    ]),),
                )?;
                let fds = file
                    .paths
                    .iter()
                    .map(std::fs::File::open)
                    .collect::<std::io::Result<Vec<_>>>()?;
                if let Err(e) = proxy.call::<_, _, ()>(
                    "AddFiles",
                    &(
                        &key,
                        fds.iter().map(zbus::zvariant::Fd::from).collect::<Vec<_>>(),
                        empty_options(),
                    ),
                ) {
                    let _ = proxy.call::<_, _, ()>("StopTransfer", &(&key,));
                    return Err(e.into());
                }
                Ok(key)
            })();
            if let Ok(key) = result {
                file.portal = Some((conn, key));
            }
        }
        Ok(file)
    }

    pub fn uri_list(&self) -> Vec<u8> {
        self.paths
            .iter()
            .map(|p| format!("{}\r\n", url::Url::from_file_path(p).unwrap()))
            .collect::<String>()
            .into_bytes()
    }

    pub fn gnome_copied_files(&self) -> Vec<u8> {
        // Nautilus' format is LF-separated with no empty final item. It is
        // distinct from text/uri-list, whose lines end in CRLF.
        format!(
            "copy\n{}",
            self.paths
                .iter()
                .map(|p| url::Url::from_file_path(p).unwrap().to_string())
                .collect::<Vec<_>>()
                .join("\n")
        )
        .into_bytes()
    }

    pub fn portal_key(&self) -> Option<Vec<u8>> {
        self.portal.as_ref().map(|(_, key)| key.as_bytes().to_vec())
    }
}

impl Drop for CachedFiles {
    fn drop(&mut self) {
        if let Some((conn, key)) = &self.portal
            && let Ok(proxy) = portal(conn)
        {
            let _ = proxy.call::<_, _, ()>("StopTransfer", &(key,));
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn empty_options() -> std::collections::HashMap<&'static str, Value<'static>> {
    Default::default()
}
fn portal(conn: &Connection) -> Result<Proxy<'_>> {
    Ok(Proxy::new(
        conn,
        "org.freedesktop.portal.Documents",
        "/org/freedesktop/portal/documents",
        "org.freedesktop.portal.FileTransfer",
    )?)
}

pub fn retrieve(key: &[u8]) -> Result<ClipFiles> {
    ensure!(key.len() <= 4096, "file transfer key too long");
    let key = std::str::from_utf8(key)?.trim_end_matches('\0');
    let conn = Connection::session()?;
    let paths: Vec<String> = portal(&conn)?.call("RetrieveFiles", &(key, empty_options()))?;
    read_paths(&paths.iter().map(PathBuf::from).collect::<Vec<_>>())
}

pub fn uri_paths(bytes: &[u8]) -> Result<Vec<PathBuf>> {
    ensure!(
        bytes.len() <= limits::CLIP_TEXT as usize,
        "URI list too large"
    );
    let mut paths = Vec::new();
    for line in std::str::from_utf8(bytes)?.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        ensure!(paths.len() < MAX_FILES, "too many files");
        let url = url::Url::parse(line)?;
        ensure!(
            url.query().is_none() && url.fragment().is_none(),
            "file URI query/fragment"
        );
        paths.push(
            url.to_file_path()
                .map_err(|_| anyhow::anyhow!("not a local file URI"))?,
        );
    }
    ensure!(!paths.is_empty(), "empty URI list");
    Ok(paths)
}

pub fn read_uri_list(bytes: &[u8]) -> Result<ClipFiles> {
    read_paths(&uri_paths(bytes)?)
}

fn read_paths(paths: &[PathBuf]) -> Result<ClipFiles> {
    ensure!((1..=MAX_FILES).contains(&paths.len()), "file count");
    let mut clip = ClipFiles(Vec::new());
    let mut remaining = limits::CLIP_FILE as usize - 4;
    for path in paths {
        let file = read_file(path, remaining)?;
        remaining -= 12 + file.name.len() + file.mime.len() + file.bytes.len();
        clip.0.push(file);
    }
    debug_assert_eq!(clip.encoded_len(), limits::CLIP_FILE as usize - remaining);
    Ok(clip)
}

fn read_file(path: &Path, remaining: usize) -> Result<ClipFile> {
    let f = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path)?;
    let meta = f.metadata()?;
    ensure!(
        meta.is_file() && meta.len() <= MAX_BYTES as u64,
        "not a bounded regular file"
    );
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("file name")?
        .to_string();
    ensure!(name.len() <= MAX_NAME, "file name too long");
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    let max = remaining
        .checked_sub(12 + name.len() + mime.len())
        .context("file clip too large")?
        .min(MAX_BYTES);
    ensure!(meta.len() <= max as u64, "file clip too large");
    let mut bytes = Vec::new();
    f.take(max as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= max, "file clip too large");
    Ok(ClipFile { name, mime, bytes })
}
use std::os::unix::fs::OpenOptionsExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_payload_is_bounded() {
        let f = ClipFiles(vec![
            ClipFile {
                name: "a b.txt".into(),
                mime: "text/plain".into(),
                bytes: b"hello".to_vec(),
            },
            ClipFile {
                name: "a b.txt".into(),
                mime: "application/octet-stream".into(),
                bytes: vec![0, 255, 1],
            },
        ]);
        let b = f.encode();
        assert_eq!(ClipFiles::decode(&b).unwrap(), f);
        for n in 0..b.len() {
            assert!(ClipFiles::decode(&b[..n]).is_err());
        }
        let mut extra = b.clone();
        extra.push(0);
        assert!(ClipFiles::decode(&extra).is_err());
        for name in ["../a", "..", "", "a/b"] {
            assert!(
                ClipFiles::decode(
                    &ClipFiles(vec![ClipFile {
                        name: name.into(),
                        ..f.0[0].clone()
                    }])
                    .encode()
                )
                .is_err()
            );
        }
        assert!(ClipFiles::decode(&vec![0; limits::CLIP_FILE as usize + 1]).is_err());
        assert!(ClipFiles::decode(&ClipFiles(vec![]).encode()).is_err());
        assert!(
            ClipFiles::decode(&ClipFiles(vec![f.0[0].clone(); MAX_FILES + 1]).encode()).is_err()
        );
        let large = ClipFile {
            bytes: vec![0; 9 << 20],
            ..f.0[0].clone()
        };
        assert!(ClipFiles::decode(&ClipFiles(vec![large; 2]).encode()).is_err());
        let large = ClipFile {
            bytes: vec![0; MAX_BYTES + 1],
            ..f.0[0].clone()
        };
        assert!(ClipFiles::decode(&ClipFiles(vec![large]).encode()).is_err());
    }

    #[test]
    fn uri_list_file_bytes_and_cache_cleanup() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out/test/clipboard-files");
        std::fs::create_dir_all(&dir).unwrap();
        let cached_dir = dir.join(format!("{}", std::process::id()));
        std::fs::create_dir_all(&cached_dir).unwrap();
        let path = cached_dir.join("a b.txt");
        std::fs::write(&path, b"host file bytes").unwrap();
        let second = cached_dir.join("second.bin");
        std::fs::write(&second, [0, 255, 42]).unwrap();
        let cached = CachedFiles {
            dir: cached_dir,
            paths: vec![path.clone(), second.clone()],
            portal: None,
        };
        let list = cached.uri_list();
        let uri = url::Url::from_file_path(&path).unwrap().to_string();
        let second_uri = url::Url::from_file_path(&second).unwrap().to_string();
        assert_eq!(list, format!("{uri}\r\n{second_uri}\r\n").as_bytes());
        assert_eq!(
            cached.gnome_copied_files(),
            format!("copy\n{uri}\n{second_uri}").as_bytes()
        );
        assert!(std::str::from_utf8(&list).unwrap().contains("a%20b.txt"));
        let clip = read_uri_list(&list).unwrap();
        assert_eq!(clip.0[0].name, "a b.txt");
        assert_eq!(clip.0[0].mime, "text/plain");
        assert_eq!(clip.0[0].bytes, b"host file bytes");
        assert_eq!(clip.0.len(), 2);
        assert_eq!(clip.0[1].name, "second.bin");
        assert_eq!(clip.0[1].bytes, [0, 255, 42]);
        assert!(read_paths(std::slice::from_ref(&cached.dir)).is_err()); // Directories reject the selection.
        let f = std::fs::File::create(&path).unwrap();
        f.set_len(9 << 20).unwrap();
        std::fs::File::create(&second)
            .unwrap()
            .set_len(9 << 20)
            .unwrap();
        assert!(read_uri_list(&list).is_err()); // Aggregate cap, even though each file fits.
        f.set_len(MAX_BYTES as u64 + 1).unwrap();
        assert!(read_uri_list(&list).is_err());
        drop(cached);
        assert!(!path.exists());
    }

    #[test]
    fn uri_list_negotiation() {
        assert_eq!(
            uri_paths(b"# comment\r\nfile:///a/b%20c.txt\r\n").unwrap(),
            vec![PathBuf::from("/a/b c.txt")]
        );
        assert_eq!(
            uri_paths(b"file:///a\r\nfile:///b\r\n").unwrap(),
            vec![PathBuf::from("/a"), PathBuf::from("/b")]
        );
        for bytes in [
            b"https://example.com/a".as_slice(),
            b"file://remote/a",
            b"\xff",
            b"file:///a#b",
            b"",
        ] {
            assert!(uri_paths(bytes).is_err(), "{bytes:?}");
        }
        assert!(uri_paths("file:///a\n".repeat(MAX_FILES + 1).as_bytes()).is_err());
        let types = ["text/plain", "text/uri-list", "image/png"].map(String::from);
        assert_eq!(super::super::preferred_type(&types), Some("text/uri-list"));
        let types = ["text/uri-list", PORTAL_TYPE].map(String::from);
        assert_eq!(super::super::preferred_type(&types), Some(PORTAL_TYPE));
    }
}
