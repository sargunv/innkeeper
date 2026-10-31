//! Android → desktop handoffs. Metadata is a sealed blob; content fds are
//! copied to real, size-capped files in the profile cache (portals cannot
//! export memfds). Opened files and email attachments stay until session
//! shutdown so a launched app can read them after the portal responds.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsFd, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use zbus::zvariant::{Fd, OwnedValue, Value};

use crate::documents::{
    portal,
    proto::{Pick, PickMode},
};
use crate::ipc::blob::Doc;
use crate::ipc::wire::{DecodeError, errno::*};

pub const MAX_FILE: u64 = 64 << 20;
const MAX_CACHE: u64 = 256 << 20;
const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
static NEXT: AtomicUsize = AtomicUsize::new(0);

pub struct Payload {
    pub text: String,
    pub subject: String,
    pub names: Vec<String>,
}

impl Payload {
    /// version 1; text (≤32 KiB), subject (≤4 KiB), one name (≤255 B) per fd.
    pub fn decode(bytes: &[u8], files: u8) -> Result<Self, DecodeError> {
        let mut d = Doc::new(bytes);
        if d.0.u32()? != 1 {
            return Err(DecodeError::Invalid("handoff version"));
        }
        let text = d.text(32 << 10)?;
        let subject = d.text(4096)?;
        let mut names = Vec::new();
        for _ in 0..files {
            let name = d.text(255)?;
            if !crate::documents::proto::valid_name(&name) {
                return Err(DecodeError::Invalid("file name"));
            }
            names.push(name);
        }
        d.end()?;
        Ok(Self {
            text,
            subject,
            names,
        })
    }
}

struct Stage {
    dir: PathBuf,
    files: Vec<File>,
    bytes: u64,
}
impl Drop for Stage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub struct Handoffs {
    root: PathBuf,
    retained: Mutex<Vec<Arc<Stage>>>,
    initialized: OnceLock<()>,
    active: AtomicUsize,
    input: Arc<crate::input::Hub>,
}

impl Handoffs {
    pub fn new(input: Arc<crate::input::Hub>) -> Arc<Self> {
        let profile = innkeeper_core::paths::profile().unwrap_or_default();
        let name = if profile.is_empty() {
            "innkeeper".into()
        } else {
            format!("innkeeper-{profile}")
        };
        let root = innkeeper_core::paths::xdg_home("XDG_CACHE_HOME", ".cache")
            .join(name)
            .join("handoff");
        Arc::new(Self {
            root,
            initialized: OnceLock::new(),
            retained: Mutex::new(vec![]),
            active: AtomicUsize::new(0),
            input,
        })
    }

    pub fn activation_token(&self, window: u32) -> String {
        self.input.activation_token(window).unwrap_or_default()
    }

    pub fn clear(&self) {
        self.retained.lock().unwrap().clear();
        if self.initialized.get().is_some() {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    /// An outstanding portal operation, released by the worker on every exit.
    pub fn slot(self: &Arc<Self>) -> Option<Slot> {
        self.active
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < 4).then_some(n + 1)
            })
            .ok()
            .map(|_| Slot(self.clone()))
    }

    fn stage(&self, p: &Payload, fds: Vec<OwnedFd>) -> Result<Stage, i32> {
        self.initialized.get_or_init(|| {
            // One bridge owns a profile; reap copies after a previous crash.
            let _ = std::fs::remove_dir_all(&self.root);
        });
        for fd in &fds {
            let stat = rustix::fs::fstat(fd).map_err(|_| -EINVAL)?;
            if rustix::fs::FileType::from_raw_mode(stat.st_mode)
                != rustix::fs::FileType::RegularFile
            {
                return Err(-EINVAL);
            }
            rustix::fs::seek(fd, rustix::fs::SeekFrom::Start(0)).map_err(|_| -EINVAL)?;
            if stat.st_size < 0 || stat.st_size as u64 > MAX_FILE {
                return Err(-rustix::io::Errno::FBIG.raw_os_error());
            }
        }
        let dir = self.root.join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).map_err(|_| -EIO)?;
        let mut stage = Stage {
            dir,
            files: vec![],
            bytes: 0,
        };
        let sources: Vec<(String, Box<dyn Read>)> = if fds.is_empty() {
            vec![(
                "Shared text.txt".into(),
                Box::new(std::io::Cursor::new(p.text.clone().into_bytes())),
            )]
        } else {
            p.names
                .iter()
                .cloned()
                .zip(
                    fds.into_iter()
                        .map(|fd| Box::new(File::from(fd)) as Box<dyn Read>),
                )
                .collect()
        };
        for (i, (name, mut source)) in sources.into_iter().enumerate() {
            // Separate directories preserve duplicate display names.
            let dir = stage.dir.join(i.to_string());
            std::fs::create_dir(&dir).map_err(|_| -EIO)?;
            let path = dir.join(name);
            let mut target = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
                .map_err(|_| -EIO)?;
            let bytes = std::io::copy(&mut source.by_ref().take(MAX_FILE + 1), &mut target)
                .map_err(|_| -EIO)?;
            if bytes > MAX_FILE {
                return Err(-rustix::io::Errno::FBIG.raw_os_error());
            }
            stage.bytes += bytes;
            target.flush().map_err(|_| -EIO)?;
            stage.files.push(File::open(path).map_err(|_| -EIO)?);
        }
        Ok(stage)
    }

    pub fn run(
        &self,
        conn: &zbus::blocking::Connection,
        window: u32,
        action: u8,
        ask: bool,
        p: Payload,
        fds: Vec<OwnedFd>,
    ) -> Result<(), i32> {
        let parent = crate::display::foreign_handle(window)
            .map(|h| format!("wayland:{h}"))
            .unwrap_or_default();
        let token = self.activation_token(window);
        if action == 1 {
            super::openuri::validate(&p.text).map_err(|_| -EINVAL)?;
            request(conn, &parent, &token, Operation::Uri(&p.text))?;
            return Ok(());
        }
        // Email text needs no synthetic attachment.
        let stage = if action == 4 && fds.is_empty() {
            None
        } else {
            Some(self.stage(&p, fds)?)
        };
        if action == 3 {
            let stage = stage.as_ref().ok_or(-EINVAL)?;
            for (i, file) in stage.files.iter().enumerate() {
                let pick = Pick {
                    request: 0,
                    mode: PickMode::Save,
                    write: true,
                    window,
                    title: "Save shared file".into(),
                    name: p
                        .names
                        .get(i)
                        .cloned()
                        .unwrap_or_else(|| "Shared text.txt".into()),
                    mimes: vec![],
                };
                let paths = portal::pick_with_activation(conn, &parent, &pick, &token)?;
                // Only the portal's explicitly chosen destination is written.
                let mut target = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(&paths[0])
                    .map_err(|_| -EIO)?;
                let mut source = file.try_clone().map_err(|_| -EIO)?;
                std::io::copy(&mut source, &mut target).map_err(|_| -EIO)?;
            }
            return Ok(()); // Stage's Drop removes the now-saved copies.
        }
        let stage = stage.map(Arc::new);
        // Reserve before launching, without locking across a portal dialog.
        // An earlier item may still need its copy if a later item fails.
        if let Some(stage) = &stage {
            let mut retained = self.retained.lock().unwrap();
            if retained.iter().map(|s| s.bytes).sum::<u64>() + stage.bytes > MAX_CACHE {
                return Err(-rustix::io::Errno::NOSPC.raw_os_error());
            }
            retained.push(stage.clone());
        }
        let mut opened = false;
        let result = (|| {
            if action == 4 {
                let files: Vec<_> = stage
                    .as_ref()
                    .map(|s| s.files.iter().map(|f| Fd::from(f.as_fd())).collect())
                    .unwrap_or_default();
                request(
                    conn,
                    &parent,
                    &token,
                    Operation::Email(&p.text, &p.subject, files),
                )?;
                opened = true;
            } else {
                for file in &stage.as_ref().ok_or(-EINVAL)?.files {
                    request(
                        conn,
                        &parent,
                        &token,
                        Operation::File(Fd::from(file.as_fd()), ask),
                    )?;
                    opened = true;
                }
            }
            Ok(())
        })();
        if !opened && let Some(stage) = &stage {
            self.retained
                .lock()
                .unwrap()
                .retain(|s| !Arc::ptr_eq(s, stage));
        }
        result
    }
}

pub struct Slot(Arc<Handoffs>);
impl Drop for Slot {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}

enum Operation<'a> {
    Uri(&'a str),
    File(Fd<'a>, bool),
    Email(&'a str, &'a str, Vec<Fd<'a>>),
}

/// Subscribe before the call so an immediate Response cannot be lost.
fn request(
    conn: &zbus::blocking::Connection,
    parent: &str,
    activation: &str,
    op: Operation<'_>,
) -> Result<HashMap<String, OwnedValue>, i32> {
    let token = format!("handoff{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let sender = conn
        .unique_name()
        .ok_or(-EIO)?
        .trim_start_matches(':')
        .replace('.', "_");
    let path = format!("{PATH}/request/{sender}/{token}");
    let proxy = zbus::blocking::Proxy::new(
        conn,
        PORTAL,
        path.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(|_| -EIO)?;
    let mut responses = proxy.receive_signal("Response").map_err(|_| -EIO)?;
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("handle_token", Value::from(token.as_str()));
    if !activation.is_empty() {
        opts.insert("activation_token", Value::from(activation));
    }
    let result = match op {
        Operation::Uri(uri) => conn.call_method(
            Some(PORTAL),
            PATH,
            Some("org.freedesktop.portal.OpenURI"),
            "OpenURI",
            &(parent, uri, opts),
        ),
        Operation::File(fd, ask) => {
            opts.insert("ask", Value::from(ask));
            conn.call_method(
                Some(PORTAL),
                PATH,
                Some("org.freedesktop.portal.OpenURI"),
                "OpenFile",
                &(parent, fd, opts),
            )
        }
        Operation::Email(text, subject, files) => {
            opts.insert("body", Value::from(text));
            opts.insert("subject", Value::from(subject));
            if !files.is_empty() {
                opts.insert("attachment_fds", Value::from(files));
            }
            conn.call_method(
                Some(PORTAL),
                PATH,
                Some("org.freedesktop.portal.Email"),
                "ComposeEmail",
                &(parent, opts),
            )
        }
    };
    result.map_err(|e| {
        crate::warn!("handoff: portal: {e}");
        -EIO
    })?;
    let message = responses.next().ok_or(-EIO)?;
    let (code, results): (u32, HashMap<String, OwnedValue>) =
        message.body().deserialize().map_err(|_| -EIO)?;
    match code {
        0 => Ok(results),
        1 => Err(-ECANCELED),
        _ => Err(-EIO),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::blob::DocWriter;
    use crate::ipc::wire::Writer;

    #[test]
    fn metadata_bounds_and_names() {
        let mut w = Writer::default();
        w.u32(1).text("hello").text("subject").text("a.odt");
        let p = Payload::decode(&w.0, 1).unwrap();
        assert_eq!(p.text, "hello");
        assert_eq!(p.names, ["a.odt"]);
        assert!(Payload::decode(&w.0, 0).is_err());
        for name in ["../a", "", ".", "..", "a/b"] {
            let mut w = Writer::default();
            w.u32(1).text("").text("").text(name);
            assert!(Payload::decode(&w.0, 1).is_err(), "{name}");
        }
        let mut w = Writer::default();
        w.u32(1).text(&"x".repeat((32 << 10) + 1)).text("");
        assert!(Payload::decode(&w.0, 0).is_err());
    }

    #[test]
    fn real_copy_preserves_name_and_is_cleaned() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../out/test/handoff")
            .join(format!(
                "{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let h = Handoffs {
            root: root.clone(),
            retained: Mutex::new(vec![]),
            initialized: OnceLock::new(),
            active: AtomicUsize::new(0),
            input: crate::input::Hub::new(),
        };
        let source = crate::ipc::blob::create("handoff-test", b"document contents").unwrap();
        let p = Payload {
            text: String::new(),
            subject: String::new(),
            names: vec!["a.odt".into()],
        };
        let stage = h.stage(&p, vec![source]).unwrap();
        assert_eq!(
            std::fs::read(stage.dir.join("0/a.odt")).unwrap(),
            b"document contents"
        );
        assert!(rustix::fs::fcntl_get_seals(&stage.files[0]).is_err()); // a real file
        let path = stage.dir.clone();
        drop(stage);
        assert!(!path.exists());
        // Reject an oversized sparse file before reading/copying its contents.
        let path = root.join("large");
        let file = File::create(&path).unwrap();
        file.set_len(MAX_FILE + 1).unwrap();
        assert!(
            matches!(h.stage(&p, vec![file.into()]), Err(e) if e == -rustix::io::Errno::FBIG.raw_os_error())
        );
        h.clear();
        assert!(!root.exists());
    }
}
