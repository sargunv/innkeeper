//! Android notifications as desktop notifications, through
//! `org.freedesktop.Notifications` (KDE, GNOME, mako, dunst ...).
//!
//! The system app's NotificationListenerService posts a notification blob
//! per Android notification (keyed by its own nonzero id); reposting an id
//! replaces the desktop notification. Clicks, actions, inline replies and
//! dismissals come back as `NOTIFY_EVENT`s. When the connection ends, its
//! notifications are closed.
//!
//! The baseline Notification portal cannot update a notification in place
//! or show progress, so the bridge uses the freedesktop notification service.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use zbus::zvariant::{StructureBuilder, Value};

use super::proto::{ev, notify_event};
use crate::ipc::blob::{self, Doc, Image};
use crate::ipc::server::Events;
use crate::ipc::wire::DecodeError;

const DEST: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";

/// Per-connection cap on live notifications.
pub const MAX_PER_CLIENT: usize = 64;
/// Longest inline reply passed back to Android.
const MAX_REPLY: usize = 4096;

pub mod flags {
    /// An ongoing notification (foreground service): resident, no timeout.
    pub const ONGOING: u8 = 1;
    /// Don't play a sound (an update, or a silent channel).
    pub const SILENT: u8 = 2;
    /// Has a content intent: clicking it opens the app.
    pub const CLICKABLE: u8 = 4;
    pub const ALL: u8 = ONGOING | SILENT | CLICKABLE;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub label: String,
    /// Takes free text (an Android RemoteInput).
    pub reply: bool,
    pub placeholder: String,
}

/// A notification blob, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub package: String,
    pub app_name: String,
    pub title: String,
    pub body: String,
    pub flags: u8,
    /// 0 low, 1 normal, 2 critical (the spec's urgency levels).
    pub urgency: u8,
    /// -1: none, -2: indeterminate, else 0 to 100.
    pub progress: i32,
    pub actions: Vec<Action>,
    /// The app's icon, for desktops that don't know the app.
    pub icon: Option<Image>,
    /// The large icon or picture.
    pub picture: Option<Image>,
}

pub const NOTE_VERSION: u32 = 1;
const MAX_ACTIONS: usize = 3;
const MAX_ICON_SIDE: u16 = 256;

impl Note {
    /// Layout: `u32 version (1)`, text package, text app name, text title,
    /// text body, `u8 flags`, `u8 urgency`, `u16 0`, `i32 progress`,
    /// `u8 actions` (at most 3) then per action: text label, `u8 reply`, text
    /// placeholder; then the icon and the picture (`blob::Doc::image`, at
    /// most 256x256).
    pub fn decode(b: &[u8]) -> Result<Note, DecodeError> {
        let mut d = Doc::new(b);
        if d.0.u32()? != NOTE_VERSION {
            return Err(DecodeError::Invalid("version"));
        }
        let package = d.text(255)?;
        if !super::valid_package(&package) {
            return Err(DecodeError::Invalid("package"));
        }
        let app_name = d.text(256)?;
        let title = d.text(1024)?;
        let body = d.text(8192)?;
        let flags = d.0.u8()?;
        if flags & !flags::ALL != 0 {
            return Err(DecodeError::Invalid("flags"));
        }
        let urgency = d.0.u8()?;
        if urgency > 2 {
            return Err(DecodeError::Invalid("urgency"));
        }
        if d.0.u16()? != 0 {
            return Err(DecodeError::Invalid("reserved"));
        }
        let progress = d.0.i32()?;
        if !(progress == -1 || progress == -2 || (0..=100).contains(&progress)) {
            return Err(DecodeError::Invalid("progress"));
        }
        let n = d.0.u8()? as usize;
        if n > MAX_ACTIONS {
            return Err(DecodeError::Invalid("too many actions"));
        }
        let mut actions = Vec::with_capacity(n);
        for _ in 0..n {
            actions.push(Action {
                label: d.text(128)?,
                reply: d.0.bool()?,
                placeholder: d.text(128)?,
            });
        }
        let icon = d.image(MAX_ICON_SIDE)?;
        let picture = d.image(MAX_ICON_SIDE)?;
        d.end()?;
        Ok(Note {
            package,
            app_name,
            title,
            body,
            flags,
            urgency,
            progress,
            actions,
            icon,
            picture,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        use crate::ipc::blob::DocWriter;
        let mut w = crate::ipc::wire::Writer::default();
        w.u32(NOTE_VERSION)
            .text(&self.package)
            .text(&self.app_name)
            .text(&self.title)
            .text(&self.body)
            .u8(self.flags)
            .u8(self.urgency)
            .u16(0)
            .i32(self.progress)
            .u8(self.actions.len() as u8);
        for a in &self.actions {
            w.text(&a.label).u8(a.reply as u8).text(&a.placeholder);
        }
        w.image(self.icon.as_ref()).image(self.picture.as_ref());
        w.into_vec()
    }
}

/// Where a desktop notification's events go.
struct Route {
    client: u64,
    id: u32,
    events: Events,
    /// Action keys we sent, by index (`a0`, `inline-reply` ...).
    keys: Vec<String>,
}

#[derive(Default)]
struct Maps {
    /// Desktop id -> route.
    routes: HashMap<u32, Route>,
    /// (client, Android-side id) -> desktop id.
    ids: HashMap<(u64, u32), u32>,
}

pub struct NotifyHub {
    conn: zbus::blocking::Connection,
    markup: bool,
    inline_reply: bool,
    maps: Mutex<Maps>,
}

impl NotifyHub {
    /// Connects to the notification server (activating it if need be) and
    /// starts the signal thread.
    pub fn start(conn: zbus::blocking::Connection) -> anyhow::Result<Arc<NotifyHub>> {
        let caps: Vec<String> = conn
            .call_method(Some(DEST), PATH, Some(DEST), "GetCapabilities", &())?
            .body()
            .deserialize()?;
        crate::info!("notify: server capabilities {caps:?}");
        let hub = Arc::new(NotifyHub {
            conn: conn.clone(),
            markup: caps.iter().any(|c| c == "body-markup"),
            inline_reply: caps.iter().any(|c| c == "inline-reply"),
            maps: Mutex::default(),
        });
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface(DEST)?
            .path(PATH)?
            .build();
        let iter = zbus::blocking::MessageIterator::for_match_rule(rule, &conn, Some(64))?;
        let h = hub.clone();
        std::thread::Builder::new()
            .name("notify-signals".into())
            .spawn(move || {
                for msg in iter.flatten() {
                    h.on_signal(&msg);
                }
                crate::warn!("notify: signal stream ended");
            })?;
        Ok(hub)
    }

    /// Shows or updates `note` for (client, id). Returns the desktop id.
    pub fn post(&self, client: u64, events: &Events, id: u32, note: &Note) -> anyhow::Result<u32> {
        let replaces = self
            .maps
            .lock()
            .unwrap()
            .ids
            .get(&(client, id))
            .copied()
            .unwrap_or(0);
        let mut actions: Vec<String> = Vec::new();
        let mut keys = Vec::new();
        if note.flags & flags::CLICKABLE != 0 {
            actions.extend(["default".into(), "Open".into()]);
        }
        let mut placeholder = None;
        for (i, a) in note.actions.iter().enumerate() {
            let key = if a.reply && self.inline_reply && placeholder.is_none() {
                placeholder = Some(a.placeholder.as_str());
                "inline-reply".to_owned()
            } else {
                format!("a{i}")
            };
            actions.extend([key.clone(), a.label.clone()]);
            keys.push(key);
        }
        let entry = linuxapk_core::desktop::app_id(&note.package);
        let mut hints: HashMap<&str, Value<'_>> = HashMap::new();
        hints.insert("desktop-entry", Value::from(entry.as_str()));
        hints.insert("x-kde-display-appname", Value::from(note.app_name.as_str()));
        hints.insert("urgency", Value::from(note.urgency));
        if note.flags & flags::SILENT != 0 {
            hints.insert("suppress-sound", Value::from(true));
        }
        if note.flags & flags::ONGOING != 0 {
            hints.insert("resident", Value::from(true));
        }
        if (0..=100).contains(&note.progress) {
            hints.insert("value", Value::from(note.progress));
        }
        if let Some(p) = placeholder.filter(|p| !p.is_empty()) {
            hints.insert("x-kde-reply-placeholder-text", Value::from(p));
        }
        let image = note
            .picture
            .as_ref()
            .or(note.icon.as_ref().filter(|_| !desktop_file_exists(&entry)));
        if let Some(img) = image {
            hints.insert("image-data", image_data(img));
        }
        let body = if self.markup {
            escape_markup(&note.body)
        } else {
            note.body.clone()
        };
        let timeout: i32 = if note.flags & flags::ONGOING != 0 {
            0
        } else {
            -1
        };
        let host: u32 = self
            .conn
            .call_method(
                Some(DEST),
                PATH,
                Some(DEST),
                "Notify",
                &(
                    note.app_name.as_str(),
                    replaces,
                    "",
                    note.title.as_str(),
                    body.as_str(),
                    actions,
                    hints,
                    timeout,
                ),
            )?
            .body()
            .deserialize()?;
        let mut m = self.maps.lock().unwrap();
        if replaces != 0 && replaces != host {
            m.routes.remove(&replaces);
        }
        m.ids.insert((client, id), host);
        m.routes.insert(
            host,
            Route {
                client,
                id,
                events: events.clone(),
                keys,
            },
        );
        Ok(host)
    }

    /// Closes (client, id) on the desktop, if it is showing.
    pub fn close(&self, client: u64, id: u32) {
        let host = {
            let mut m = self.maps.lock().unwrap();
            let Some(host) = m.ids.remove(&(client, id)) else {
                return;
            };
            m.routes.remove(&host);
            host
        };
        let r = self
            .conn
            .call_method(Some(DEST), PATH, Some(DEST), "CloseNotification", &(host,));
        if let Err(e) = r {
            crate::warn!("notify: close {host}: {e}");
        }
    }

    pub fn count(&self, client: u64) -> usize {
        let m = self.maps.lock().unwrap();
        m.ids.keys().filter(|(c, _)| *c == client).count()
    }

    pub fn contains(&self, client: u64, id: u32) -> bool {
        self.maps.lock().unwrap().ids.contains_key(&(client, id))
    }

    /// Closes everything `client` posted (its connection ended).
    pub fn close_all(&self, client: u64) {
        let ids: Vec<u32> = {
            let m = self.maps.lock().unwrap();
            m.ids
                .keys()
                .filter(|(c, _)| *c == client)
                .map(|k| k.1)
                .collect()
        };
        for id in ids {
            self.close(client, id);
        }
    }

    /// Closes every notification we show (the bridge is exiting).
    pub fn close_everything(&self) {
        let hosts: Vec<u32> = {
            let mut m = self.maps.lock().unwrap();
            m.ids.clear();
            m.routes.drain().map(|(h, _)| h).collect()
        };
        for host in hosts {
            let _ =
                self.conn
                    .call_method(Some(DEST), PATH, Some(DEST), "CloseNotification", &(host,));
        }
    }

    fn on_signal(&self, msg: &zbus::Message) {
        let h = msg.header();
        let Some(member) = h.member() else { return };
        let body = msg.body();
        match member.as_str() {
            "ActionInvoked" => {
                let Ok((host, key)) = body.deserialize::<(u32, String)>() else {
                    return;
                };
                let m = self.maps.lock().unwrap();
                let Some(r) = m.routes.get(&host) else { return };
                let (what, idx) = if key == "default" {
                    (notify_event::CLICKED, 0)
                } else if let Some(i) = r.keys.iter().position(|k| *k == key) {
                    (notify_event::ACTION, i as u8)
                } else {
                    return;
                };
                crate::info!("notify: {host} ({}): {key}", r.id);
                let _ = r
                    .events
                    .try_send(ev::NOTIFY_EVENT, &notify_event(r.id, what, idx, 0), &[]);
            }
            "NotificationReplied" => {
                let Ok((host, text)) = body.deserialize::<(u32, String)>() else {
                    return;
                };
                let m = self.maps.lock().unwrap();
                let Some(r) = m.routes.get(&host) else { return };
                let Some(idx) = r.keys.iter().position(|k| k == "inline-reply") else {
                    return;
                };
                let text = crate::ipc::wire::truncate(&text, MAX_REPLY);
                crate::info!("notify: {host} ({}): reply, {} bytes", r.id, text.len());
                match blob::create("linuxapk-reply", text.as_bytes()) {
                    Ok(fd) => {
                        let body =
                            notify_event(r.id, notify_event::REPLY, idx as u8, text.len() as u32);
                        let _ = r.events.try_send(
                            ev::NOTIFY_EVENT,
                            &body,
                            &[std::os::fd::AsFd::as_fd(&fd)],
                        );
                    }
                    Err(e) => crate::warn!("notify: reply memfd: {e}"),
                }
            }
            "NotificationClosed" => {
                let Ok((host, reason)) = body.deserialize::<(u32, u32)>() else {
                    return;
                };
                // 1: expired. The server keeps it in its history, where updates
                // (replaces_id) and actions still reach it, and Android keeps
                // it too: leave the mapping alone.
                if reason == 1 {
                    return;
                }
                let mut m = self.maps.lock().unwrap();
                let Some(r) = m.routes.remove(&host) else {
                    return;
                };
                m.ids.remove(&(r.client, r.id));
                // 2: dismissed by the user; 3: closed by us.
                if reason == 2 {
                    crate::info!("notify: {host} ({}): dismissed", r.id);
                    let _ = r.events.try_send(
                        ev::NOTIFY_EVENT,
                        &notify_event(r.id, notify_event::DISMISSED, 0, 0),
                        &[],
                    );
                }
            }
            _ => {}
        }
    }
}

/// The spec's `image-data` hint: `(iiibiiay)` width, height, rowstride,
/// has alpha, bits per sample, channels, data.
fn image_data(img: &Image) -> Value<'static> {
    StructureBuilder::new()
        .add_field(img.width as i32)
        .add_field(img.height as i32)
        .add_field(img.width as i32 * 4)
        .add_field(true)
        .add_field(8i32)
        .add_field(4i32)
        .add_field(img.rgba.clone())
        .build()
        .expect("non-empty structure")
        .into()
}

/// Servers with `body-markup` parse a small HTML subset; Android text is
/// plain, so escape it.
pub fn escape_markup(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c => o.push(c),
        }
    }
    o
}

fn desktop_file_exists(entry: &str) -> bool {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share"))
        });
    home.is_some_and(|h| {
        h.join("applications")
            .join(format!("{entry}.desktop"))
            .exists()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Note {
        Note {
            package: "org.example.app".into(),
            app_name: "Example".into(),
            title: "Hello".into(),
            body: "<b>x</b> & y".into(),
            flags: flags::CLICKABLE,
            urgency: 1,
            progress: 40,
            actions: vec![
                Action {
                    label: "Mark read".into(),
                    reply: false,
                    placeholder: String::new(),
                },
                Action {
                    label: "Reply".into(),
                    reply: true,
                    placeholder: "Message".into(),
                },
            ],
            icon: Some(Image {
                width: 2,
                height: 2,
                rgba: vec![255; 16],
            }),
            picture: None,
        }
    }

    #[test]
    fn roundtrip_and_checks() {
        let n = sample();
        assert_eq!(Note::decode(&n.encode()), Ok(n.clone()));
        for bad in [
            Note {
                package: "org/../x".into(),
                ..n.clone()
            },
            Note {
                progress: 101,
                ..n.clone()
            },
            Note {
                urgency: 3,
                ..n.clone()
            },
            Note {
                flags: 0x80,
                ..n.clone()
            },
            Note {
                actions: vec![n.actions[0].clone(); 4],
                ..n.clone()
            },
        ] {
            assert!(Note::decode(&bad.encode()).is_err(), "{bad:?}");
        }
        let mut b = n.encode();
        b.push(0);
        assert!(Note::decode(&b).is_err());
        assert_eq!(escape_markup("<b>&"), "&lt;b&gt;&amp;");
    }
}
