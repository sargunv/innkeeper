//! Bounded single-file clipboard payloads and cache copies. File-transfer portal
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipFile {
    pub name: String,
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl ClipFile {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        w.text(&self.name)
            .text(&self.mime)
            .u32(self.bytes.len() as u32);
        w.0.extend_from_slice(&self.bytes);
        w.into_vec()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(
            bytes.len() <= limits::CLIP_FILE as usize,
            "file clip too large"
        );
        let mut d = Doc::new(bytes);
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
        d.end()?;
        Ok(Self { name, mime, bytes })
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
pub struct CachedFile {
    dir: PathBuf,
    path: PathBuf,
    portal: Option<(Connection, String)>,
}

impl CachedFile {
    pub fn create(cache: &Path, clip: &ClipFile) -> Result<Self> {
        static SERIAL: AtomicU64 = AtomicU64::new(0);
        let dir = cache.join(SERIAL.fetch_add(1, Ordering::Relaxed).to_string());
        std::fs::create_dir(&dir)?;
        let mut file = Self {
            path: dir.join(&clip.name),
            dir,
            portal: None,
        };
        std::fs::write(&file.path, &clip.bytes)?;
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
                let fd = std::fs::File::open(&file.path)?;
                if let Err(e) = proxy.call::<_, _, ()>(
                    "AddFiles",
                    &(&key, vec![zbus::zvariant::Fd::from(&fd)], empty_options()),
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
        format!("{}\r\n", url::Url::from_file_path(&self.path).unwrap()).into_bytes()
    }

    pub fn portal_key(&self) -> Option<Vec<u8>> {
        self.portal.as_ref().map(|(_, key)| key.as_bytes().to_vec())
    }
}

impl Drop for CachedFile {
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

pub fn retrieve(key: &[u8]) -> Result<ClipFile> {
    ensure!(key.len() <= 4096, "file transfer key too long");
    let key = std::str::from_utf8(key)?.trim_end_matches('\0');
    let conn = Connection::session()?;
    let paths: Vec<String> = portal(&conn)?.call("RetrieveFiles", &(key, empty_options()))?;
    ensure!(paths.len() == 1, "copy one file at a time");
    read_file(Path::new(&paths[0]))
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
        ensure!(paths.is_empty(), "copy one file at a time");
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
    ensure!(paths.len() == 1, "empty URI list");
    Ok(paths)
}

pub fn read_uri_list(bytes: &[u8]) -> Result<ClipFile> {
    read_file(&uri_paths(bytes)?[0])
}

fn read_file(path: &Path) -> Result<ClipFile> {
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
    let mut bytes = Vec::new();
    f.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= MAX_BYTES, "file too large");
    let mime = mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string();
    Ok(ClipFile { name, mime, bytes })
}
use std::os::unix::fs::OpenOptionsExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_payload_is_bounded() {
        let f = ClipFile {
            name: "a b.txt".into(),
            mime: "text/plain".into(),
            bytes: b"hello".to_vec(),
        };
        let b = f.encode();
        assert_eq!(ClipFile::decode(&b).unwrap(), f);
        for n in 0..b.len() {
            assert!(ClipFile::decode(&b[..n]).is_err());
        }
        let mut extra = b.clone();
        extra.push(0);
        assert!(ClipFile::decode(&extra).is_err());
        for name in ["../a", "..", "", "a/b"] {
            assert!(
                ClipFile::decode(
                    &ClipFile {
                        name: name.into(),
                        ..f.clone()
                    }
                    .encode()
                )
                .is_err()
            );
        }
        assert!(ClipFile::decode(&vec![0; limits::CLIP_FILE as usize + 1]).is_err());
    }

    #[test]
    fn uri_list_file_bytes_and_cache_cleanup() {
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../out/test/clipboard-files");
        std::fs::create_dir_all(&dir).unwrap();
        let cached_dir = dir.join(format!("{}", std::process::id()));
        std::fs::create_dir_all(&cached_dir).unwrap();
        let path = cached_dir.join("a b.txt");
        std::fs::write(&path, b"host file bytes").unwrap();
        let cached = CachedFile {
            dir: cached_dir,
            path: path.clone(),
            portal: None,
        };
        let list = cached.uri_list();
        assert!(std::str::from_utf8(&list).unwrap().contains("a%20b.txt"));
        let clip = read_uri_list(&list).unwrap();
        assert_eq!(clip.name, "a b.txt");
        assert_eq!(clip.mime, "text/plain");
        assert_eq!(clip.bytes, b"host file bytes");
        let f = std::fs::File::create(&path).unwrap();
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
        for bytes in [
            b"https://example.com/a".as_slice(),
            b"file://remote/a",
            b"file:///a\nfile:///b",
            b"\xff",
            b"file:///a#b",
            b"",
        ] {
            assert!(uri_paths(bytes).is_err(), "{bytes:?}");
        }
        let types = ["text/plain", "text/uri-list", "image/png"].map(String::from);
        assert_eq!(super::super::preferred_type(&types), Some("text/uri-list"));
    }
}
