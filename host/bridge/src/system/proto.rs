//! System service messages (id 3, types 0x0300..=0x03ff, events
//! 0x4300..=0x43ff). The client is the Innkeeper System app, through
//! `BridgeConnection` and its integration features. It sends `START` and
//! then receives events. Anything that can exceed one message travels as a
//! sealed memfd blob ([`crate::ipc::blob`]), and the message carries the
//! blob's length.
//!
//! | Type     | Request      | Body (fd)                                                                       | Reply                                                                                          |
//! | -------- | ------------ | ------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------- |
//! | `0x0301` | START        |                                                                                 | `u8 notifications` (1: the desktop shows them); the current settings and host clipboard follow as events |
//! | `0x0302` | NOTIFY       | `u32 id` (≠ 0), `u32 len` ≤ 1 MiB (fd: notification blob)                       | `-EBUSY` over 64 per connection, `-EIO` if the server refused; posting an id again replaces it |
//! | `0x0303` | NOTIFY_CLOSE | `u32 id`                                                                        |                                                                                                |
//! | `0x0304` | CLIP_SET     | `u8 kind` (1 text, 2 image, 3 file), `u8 sensitive`, `u16 0`, `u32 len` ≤ 1/16 MiB (fd) | `-ENOSYS` when clipboard disabled                                                                 |
//! | `0x0306` | MEDIA        | `u32 player` (≠ 0), `u32 len` ≤ 2 MiB (fd: media blob)                          | `-EBUSY` over 8 players per connection                                                         |
//! | `0x0307` | MEDIA_CLOSE  | `u32 player`                                                                    |                                                                                                |
//! | `0x0308` | LIFECYCLE    | `u32 windows`, `u32 services`, `u64 wakeup`, `u32 len` ≤ 16 KiB (fd if len > 0: background apps) | the session's activity, for idle shutdown ([`Lifecycle`])                              |
//! | `0x0309` | HANDOFF | `u32 window`, `u8 action` (1 URI, 2 open, 3 save, 4 email), `u8 files` (0..3), `u8 ask`, `u8 0`, `u32 len` ≤ 64 KiB; metadata fd then content fds | accepted for asynchronous portal operation; `-EBUSY` over 4 pending, `-EAGAIN` over 5 per 10 s |
//! | `0x030a` | PLAYBACK_APP | name package (empty: nothing playing), name label | labels Android's mixed playback stream in the desktop mixer ([`PlaybackApp`]) |
//! | `0x030b` | SHOW_SCREEN  | `u32 id` (0: no notification)                                                   | shows OpenHarmony's screen window with the activation token of notification `id`'s last click; `-ENOENT` if the session has no screen window |
//!
//! HANDOFF carries every portal operation that hands something from Android
//! to the desktop. Its metadata is `u32 version` (1), text (≤32 KiB), a
//! subject (≤4 KiB), and then one display name (≤255 B, no `/`, not `.` or
//! `..`) per content fd. Each string is a `u32` length followed by UTF-8. A
//! URI handoff has no content fds and refuses `file:`, `content:`,
//! `intent:`, `javascript:` and `data:`. The reply means the operation was
//! scheduled, and the bridge logs a portal's cancellation or failure. Open
//! and save without files write the text as a `.txt` file, and email without
//! files uses the text as the body. Each content file is at most 64 MiB.
//! Opened files and attachments stay in the profile cache until session
//! shutdown, capped at 256 MiB in total. Save copies are removed after
//! writing.
//!
//! | Event    | Message      | Body (fd)                                                                                                                                                                                                |
//! | -------- | ------------ | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
//! | `0x4301` | SETTINGS     | `u8 color_scheme` (0 none, 1 dark, 2 light), `u8 has_accent`, `u8 r, g, b`, `u8 contrast`, `u8 reduced_motion`, `u8 clock` (0, 1 12h, 2 24h), `f32 text_scale` (0 unknown), name time zone, name locales, name LC_TIME locale, name LC_MEASUREMENT locale |
//! | `0x4302` | NOTIFY_EVENT | `u32 id`, `u8 what` (1 clicked, 2 action, 3 dismissed, 4 reply), `u8 action`, `u16 0`, `u32 len` (fd: the reply text, for 4)                                                                             |
//! | `0x4303` | CLIP         | `u8 kind`, `u8 sensitive`, `u16 0`, `u32 len` (fd: the content)                                                                                                                                          |
//! | `0x4304` | MEDIA_CMD    | `u32 player`, `u8 cmd` (1 play, 2 pause, 3 play/pause, 4 stop, 5 next, 6 previous, 7 seek by, 8 seek to, 9 raise), `u8 0`, `u16 0`, `i64 arg` (µs)                                                       |
//!
//! Blob contents start with `u32 version` (1). [`super::notify::Note`] and
//! [`super::mpris::Media`] document their layouts, and the system app's
//! encoders (`Notifications.kt`, `MediaSync.kt`) mirror them. Text is a
//! `u32 len` followed by UTF-8. Images are `u16 width, u16 height` followed
//! by raw RGBA8 rows, so nothing Android sends reaches a host image decoder.
//! Clip kind 3 is a `u32 count` (1..16) of files. Each file is a text name
//! (1..255 bytes, no slash), a text MIME type (1..255 bytes), then a
//! `u32 size` and raw bytes (at most 16 MiB minus 1024). The whole blob is at
//! most 16 MiB, with no trailing bytes. Clipboard text is raw UTF-8 without a
//! length prefix.

use crate::ipc::wire::{DecodeError, Reader, Writer};

pub mod ty {
    pub const START: u16 = 0x0301;
    pub const NOTIFY: u16 = 0x0302;
    pub const NOTIFY_CLOSE: u16 = 0x0303;
    pub const CLIP_SET: u16 = 0x0304;
    pub const MEDIA: u16 = 0x0306;
    pub const MEDIA_CLOSE: u16 = 0x0307;
    pub const LIFECYCLE: u16 = 0x0308;
    pub const HANDOFF: u16 = 0x0309;
    pub const PLAYBACK_APP: u16 = 0x030a;
    pub const SHOW_SCREEN: u16 = 0x030b;
}

pub mod ev {
    pub const SETTINGS: u16 = 0x4301;
    pub const NOTIFY_EVENT: u16 = 0x4302;
    pub const CLIP: u16 = 0x4303;
    pub const MEDIA_CMD: u16 = 0x4304;
}

/// Caps on blob lengths, per message.
pub mod limits {
    pub const NOTIFY_BLOB: u32 = 1 << 20;
    pub const CLIP_TEXT: u32 = 1 << 20;
    pub const CLIP_IMAGE: u32 = 16 << 20;
    pub const CLIP_FILE: u32 = 16 << 20;
    pub const MEDIA_BLOB: u32 = 2 << 20;
    /// Caps on LIFECYCLE's blob and on the background-allowed apps it lists.
    pub const LIFECYCLE_BLOB: u32 = 16 << 10;
    pub const BACKGROUND_APPS: usize = 32;
}

/// Clipboard content kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipKind {
    /// UTF-8 text.
    Text = 1,
    /// An RGBA image (`blob::Doc::image`).
    Image = 2,
    /// Files: a `u32` count, then per file a blob text name, a blob text MIME
    /// type, a `u32` byte count and the raw bytes.
    File = 3,
}

impl ClipKind {
    fn from_u8(v: u8) -> Result<ClipKind, DecodeError> {
        match v {
            1 => Ok(ClipKind::Text),
            2 => Ok(ClipKind::Image),
            3 => Ok(ClipKind::File),
            _ => Err(DecodeError::Invalid("clip kind")),
        }
    }
    pub fn max_len(self) -> u32 {
        match self {
            ClipKind::Text => limits::CLIP_TEXT,
            ClipKind::Image => limits::CLIP_IMAGE,
            ClipKind::File => limits::CLIP_FILE,
        }
    }
}

/// What keeps the session up, as the system app sees it (docs/session.md,
/// "Lifecycle"). The app sends a new report whenever a part changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Lifecycle {
    /// Windows the window manager has shown or is starting.
    pub windows: u32,
    /// User apps' running foreground services.
    pub services: u32,
    /// The next alarm clock in ms since the epoch, or 0 for none.
    pub wakeup_ms: u64,
    /// Length of the blob that lists the apps the user lets run in the
    /// background, one `package<TAB>label` line each ([`background_apps`]).
    /// 0 means no apps and no fd.
    pub len: u32,
}

/// Parses a LIFECYCLE blob into (package, label) pairs, at most
/// [`limits::BACKGROUND_APPS`].
pub fn background_apps(blob: &[u8]) -> Result<Vec<(String, String)>, DecodeError> {
    let text = std::str::from_utf8(blob).map_err(|_| DecodeError::Invalid("utf-8"))?;
    let mut v = vec![];
    for l in text.lines().filter(|l| !l.is_empty()) {
        let (pkg, label) = l.split_once('\t').unwrap_or((l, l));
        if !super::valid_package(pkg) {
            return Err(DecodeError::Invalid("package"));
        }
        if v.len() == limits::BACKGROUND_APPS {
            return Err(DecodeError::Invalid("too many background apps"));
        }
        v.push((
            pkg.to_string(),
            label.chars().filter(|c| !c.is_control()).collect(),
        ));
    }
    Ok(v)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The reply is `u8 notifications`, which says whether NOTIFY reaches the
    /// desktop.
    Start,
    /// `u32 id` (nonzero) and `u32 len`, with the notification blob as the
    /// fd. Posting an id again updates that notification.
    Notify { id: u32, len: u32 },
    /// `u32 id`.
    NotifyClose { id: u32 },
    /// `u8 kind`, `u8 sensitive`, `u16 0` and `u32 len`, with the content as
    /// the fd.
    ClipSet {
        kind: ClipKind,
        sensitive: bool,
        len: u32,
    },
    /// `u32 player` (nonzero) and `u32 len`, with the media blob as the fd.
    Media { player: u32, len: u32 },
    /// `u32 player`.
    MediaClose { player: u32 },
    /// `u32 windows`, `u32 services`, `u64 wakeup` and `u32 len`. If len > 0,
    /// the fd holds the background apps.
    Lifecycle(Lifecycle),
    /// The fds are a metadata memfd followed by `files` content fds. See
    /// `handoff`.
    Handoff {
        window: u32,
        action: u8,
        files: u8,
        ask: bool,
        len: u32,
    },
    /// The app on Android's mixed playback stream. An empty package means
    /// nothing is playing, and the mixer goes back to the default name.
    PlaybackApp(PlaybackApp),
    /// OpenHarmony's system app is about to start an ability for notification
    /// `id`, so the screen window comes up with the click's activation token.
    /// Android's windows open by themselves, so its app doesn't send this.
    ShowScreen { id: u32 },
}

/// What the desktop mixer shows for Android's single playback stream.
/// Android mixes every app into that stream, so this names the most recently
/// started app that is playing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlaybackApp {
    /// Android package, or empty when nothing is playing.
    pub package: String,
    /// The app's label. If it is empty, the bridge uses the package.
    pub label: String,
}

impl Request {
    /// The number of fds this request carries.
    pub fn fd_count(&self) -> usize {
        if let Request::Handoff { files, .. } = self {
            return 1 + *files as usize;
        }
        self.takes_fd() as usize
    }

    pub fn takes_fd(&self) -> bool {
        matches!(
            self,
            Request::Notify { .. }
                | Request::ClipSet { .. }
                | Request::Media { .. }
                | Request::Handoff { .. }
        ) || matches!(self, Request::Lifecycle(l) if l.len > 0)
    }
}

fn nonzero(v: u32, what: &'static str) -> Result<u32, DecodeError> {
    if v == 0 {
        Err(DecodeError::Invalid(what))
    } else {
        Ok(v)
    }
}

fn limited(v: u32, max: u32, what: &'static str) -> Result<u32, DecodeError> {
    if v > max {
        Err(DecodeError::Invalid(what))
    } else {
        Ok(v)
    }
}

pub fn decode(ty: u16, body: &[u8]) -> Result<Request, DecodeError> {
    let mut r = Reader::new(body);
    let req = match ty {
        ty::HANDOFF => {
            let window = r.u32()?;
            let action = r.u8()?;
            let files = r.u8()?;
            let ask = r.bool()?;
            if r.u8()? != 0
                || !(1..=4).contains(&action)
                || files > 3
                || (action == 1 && files != 0)
            {
                return Err(DecodeError::Invalid("handoff"));
            }
            Request::Handoff {
                window,
                action,
                files,
                ask,
                len: limited(nonzero(r.u32()?, "len")?, 64 << 10, "len")?,
            }
        }
        ty::START => Request::Start,
        ty::NOTIFY => Request::Notify {
            id: nonzero(r.u32()?, "id")?,
            len: limited(r.u32()?, limits::NOTIFY_BLOB, "len")?,
        },
        ty::NOTIFY_CLOSE => Request::NotifyClose {
            id: nonzero(r.u32()?, "id")?,
        },
        ty::CLIP_SET => {
            let kind = ClipKind::from_u8(r.u8()?)?;
            let sensitive = r.bool()?;
            if r.u16()? != 0 {
                return Err(DecodeError::Invalid("reserved"));
            }
            Request::ClipSet {
                kind,
                sensitive,
                len: limited(r.u32()?, kind.max_len(), "len")?,
            }
        }
        ty::MEDIA => Request::Media {
            player: nonzero(r.u32()?, "player")?,
            len: limited(r.u32()?, limits::MEDIA_BLOB, "len")?,
        },
        ty::MEDIA_CLOSE => Request::MediaClose {
            player: nonzero(r.u32()?, "player")?,
        },
        ty::LIFECYCLE => Request::Lifecycle(Lifecycle {
            windows: r.u32()?,
            services: r.u32()?,
            wakeup_ms: r.u64()?,
            len: limited(r.u32()?, limits::LIFECYCLE_BLOB, "len")?,
        }),
        ty::SHOW_SCREEN => Request::ShowScreen { id: r.u32()? },
        ty::PLAYBACK_APP => {
            let package = r.name()?;
            if !package.is_empty() && !super::valid_package(&package) {
                return Err(DecodeError::Invalid("package"));
            }
            Request::PlaybackApp(PlaybackApp {
                package,
                label: r.name()?,
            })
        }
        t => return Err(DecodeError::UnknownType(t)),
    };
    r.end()?;
    Ok(req)
}

pub fn encode(tag: u32, req: &Request) -> Vec<u8> {
    let w = match *req {
        Request::Handoff {
            window,
            action,
            files,
            ask,
            len,
        } => {
            let mut w = Writer::message(ty::HANDOFF, tag);
            w.u32(window)
                .u8(action)
                .u8(files)
                .u8(ask as u8)
                .u8(0)
                .u32(len);
            w
        }
        Request::Lifecycle(l) => {
            let mut w = Writer::message(ty::LIFECYCLE, tag);
            w.u32(l.windows).u32(l.services).u64(l.wakeup_ms).u32(l.len);
            w
        }
        Request::Start => Writer::message(ty::START, tag),
        Request::Notify { id, len } => {
            let mut w = Writer::message(ty::NOTIFY, tag);
            w.u32(id).u32(len);
            w
        }
        Request::NotifyClose { id } => {
            let mut w = Writer::message(ty::NOTIFY_CLOSE, tag);
            w.u32(id);
            w
        }
        Request::ClipSet {
            kind,
            sensitive,
            len,
        } => {
            let mut w = Writer::message(ty::CLIP_SET, tag);
            w.u8(kind as u8).u8(sensitive as u8).u16(0).u32(len);
            w
        }
        Request::Media { player, len } => {
            let mut w = Writer::message(ty::MEDIA, tag);
            w.u32(player).u32(len);
            w
        }
        Request::MediaClose { player } => {
            let mut w = Writer::message(ty::MEDIA_CLOSE, tag);
            w.u32(player);
            w
        }
        Request::PlaybackApp(ref app) => {
            let mut w = Writer::message(ty::PLAYBACK_APP, tag);
            w.name(&app.package).name(&app.label);
            w
        }
        Request::ShowScreen { id } => {
            let mut w = Writer::message(ty::SHOW_SCREEN, tag);
            w.u32(id);
            w
        }
    };
    w.into_vec()
}

/// Desktop settings, sent after START and whenever one changes. Unknown
/// values are 0 or empty, and the client leaves those settings alone.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Settings {
    /// `org.freedesktop.appearance` `color-scheme`: 0 no preference, 1 dark,
    /// 2 light.
    pub color_scheme: u8,
    /// `accent-color` as sRGB bytes.
    pub accent: Option<[u8; 3]>,
    /// Set when the user opted out, sent as has_accent=2. Android's accent is
    /// then left untouched.
    pub accent_disabled: bool,
    /// `contrast`: 0 normal, 1 high.
    pub contrast: u8,
    /// `reduced-motion`: 0 no preference, 1 reduced.
    pub reduced_motion: u8,
    /// Clock format: 0 unknown, 1 12-hour, 2 24-hour.
    pub clock: u8,
    /// Text scaling factor from 0.5 to 3, or 0 if unknown.
    pub text_scale: f32,
    /// IANA time zone id, e.g. `Europe/Berlin`.
    pub timezone: String,
    /// BCP 47 language tags in preference order, comma-separated.
    pub locales: String,
    /// The `LC_TIME` locale as a BCP 47 tag, used for the week start and
    /// regional preferences. It is empty if unset.
    pub time_locale: String,
    /// The `LC_MEASUREMENT` locale as a BCP 47 tag, used for the temperature
    /// unit.
    pub measure_locale: String,
}

impl Settings {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = Writer::default();
        let [r, g, b] = self.accent.unwrap_or_default();
        w.u8(self.color_scheme)
            .u8(if self.accent_disabled {
                2
            } else {
                self.accent.is_some() as u8
            })
            .u8(r)
            .u8(g)
            .u8(b)
            .u8(self.contrast)
            .u8(self.reduced_motion)
            .u8(self.clock)
            .f32(self.text_scale)
            .name(&self.timezone)
            .name(&self.locales)
            .name(&self.time_locale)
            .name(&self.measure_locale);
        w.into_vec()
    }

    pub fn decode(body: &[u8]) -> Result<Settings, DecodeError> {
        let mut r = Reader::new(body);
        let color_scheme = r.u8()?;
        let has_accent = r.u8()?;
        if has_accent > 2 {
            return Err(DecodeError::Invalid("accent policy"));
        }
        let rgb = [r.u8()?, r.u8()?, r.u8()?];
        let s = Settings {
            color_scheme,
            accent: (has_accent == 1).then_some(rgb),
            accent_disabled: has_accent == 2,
            contrast: r.u8()?,
            reduced_motion: r.u8()?,
            clock: r.u8()?,
            text_scale: r.f32()?,
            timezone: r.name()?,
            locales: r.name()?,
            time_locale: r.name()?,
            measure_locale: r.name()?,
        };
        r.end()?;
        Ok(s)
    }
}

/// What happened to a notification on the desktop.
pub mod notify_event {
    /// The notification was clicked.
    pub const CLICKED: u8 = 1;
    /// Action `action` was invoked.
    pub const ACTION: u8 = 2;
    /// The user dismissed it on the desktop.
    pub const DISMISSED: u8 = 3;
    /// An inline reply to action `action`. The fd holds the text (`len`
    /// bytes).
    pub const REPLY: u8 = 4;
}

/// `NOTIFY_EVENT` body: `u32 id`, `u8 what`, `u8 action`, `u16 0`, `u32 len`.
pub fn notify_event(id: u32, what: u8, action: u8, len: u32) -> Vec<u8> {
    let mut w = Writer::default();
    w.u32(id).u8(what).u8(action).u16(0).u32(len);
    w.into_vec()
}

/// `CLIP` body: `u8 kind`, `u8 sensitive`, `u16 0`, `u32 len`. The fd holds
/// the content.
pub fn clip_event(kind: ClipKind, sensitive: bool, len: u32) -> Vec<u8> {
    let mut w = Writer::default();
    w.u8(kind as u8).u8(sensitive as u8).u16(0).u32(len);
    w.into_vec()
}

/// Media commands (`MEDIA_CMD`).
pub mod media_cmd {
    pub const PLAY: u8 = 1;
    pub const PAUSE: u8 = 2;
    pub const PLAY_PAUSE: u8 = 3;
    pub const STOP: u8 = 4;
    pub const NEXT: u8 = 5;
    pub const PREVIOUS: u8 = 6;
    /// Seek by `arg` microseconds (relative).
    pub const SEEK: u8 = 7;
    /// Seek to `arg` microseconds (absolute).
    pub const SET_POSITION: u8 = 8;
    /// Bring the app's window up (MPRIS `Raise`).
    pub const RAISE: u8 = 9;
}

/// `MEDIA_CMD` body: `u32 player`, `u8 cmd`, `u8 0`, `u16 0`, `i64 arg`.
pub fn media_cmd(player: u32, cmd: u8, arg: i64) -> Vec<u8> {
    let mut w = Writer::default();
    w.u32(player).u8(cmd).u8(0).u16(0).u64(arg as u64);
    w.into_vec()
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
    fn examples() {
        let reqs = [
            Request::Start,
            Request::Notify { id: 3, len: 900 },
            Request::NotifyClose { id: 3 },
            Request::ClipSet {
                kind: ClipKind::Text,
                sensitive: true,
                len: 5,
            },
            Request::ClipSet {
                kind: ClipKind::File,
                sensitive: false,
                len: limits::CLIP_FILE,
            },
            Request::Handoff {
                window: 12,
                action: 4,
                files: 3,
                ask: false,
                len: 100,
            },
            Request::Media {
                player: 1,
                len: 100,
            },
            Request::MediaClose { player: 1 },
            Request::Lifecycle(Lifecycle::default()),
            Request::Lifecycle(Lifecycle {
                windows: 1,
                services: 2,
                wakeup_ms: 1_793_222_130_000,
                len: 20,
            }),
            Request::PlaybackApp(PlaybackApp {
                package: "org.example.player".into(),
                label: "Example Player".into(),
            }),
            Request::PlaybackApp(PlaybackApp::default()),
            Request::ShowScreen { id: 0 },
            Request::ShowScreen { id: 7 },
        ];
        for r in reqs {
            assert_eq!(roundtrip(&encode(1, &r)), Ok(r));
        }
        for (t, body) in [
            (ty::NOTIFY, &[0u8, 0, 0, 0, 1, 0, 0, 0][..]), // id 0
            (ty::NOTIFY, &[1, 0, 0, 0, 0, 0, 0x20, 0]),    // over the cap
            (ty::CLIP_SET, &[4, 0, 0, 0, 1, 0, 0, 0]),
            (ty::CLIP_SET, &[1, 2, 0, 0, 1, 0, 0, 0]),
            (ty::CLIP_SET, &[1, 0, 1, 0, 1, 0, 0, 0]),
            (ty::START, &[0]),
            (ty::SHOW_SCREEN, &[1, 0, 0]),
            // A package name with a slash.
            (ty::PLAYBACK_APP, &[3, 0, b'a', b'/', b'b', 0, 0]),
            // A label with a control character.
            (ty::PLAYBACK_APP, &[3, 0, b'a', b'.', b'b', 1, 0, 0x01]),
            // A blob over 16 KiB.
            (
                ty::LIFECYCLE,
                &[
                    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0x40, 0, 0,
                ][..],
            ),
        ] {
            assert!(decode(t, body).is_err(), "{t:#x} {body:?}");
        }
        assert_eq!(decode(0x0399, &[]), Err(DecodeError::UnknownType(0x0399)));
    }

    #[test]
    fn settings_roundtrip() {
        let s = Settings {
            color_scheme: 1,
            accent: Some([0x3d, 0xae, 0xe9]),
            accent_disabled: false,
            contrast: 0,
            reduced_motion: 1,
            clock: 2,
            text_scale: 1.25,
            timezone: "Europe/Berlin".into(),
            locales: "de-DE,en-US".into(),
            time_locale: "en-GB".into(),
            measure_locale: "en-US".into(),
        };
        let b = s.encode();
        assert!(b.len() <= crate::ipc::wire::MAX_MSG - HDR);
        assert_eq!(Settings::decode(&b), Ok(s));
        assert_eq!(
            Settings::decode(&Settings::default().encode()),
            Ok(Settings::default())
        );
    }

    #[test]
    fn background_blob() {
        assert_eq!(
            background_apps(b"me.ash.reader\tRead You\norg.x\n"),
            Ok(vec![
                ("me.ash.reader".into(), "Read You".into()),
                ("org.x".into(), "org.x".into())
            ])
        );
        assert_eq!(background_apps(b""), Ok(vec![]));
        assert!(background_apps(b"../x\tX\n").is_err());
        assert!(background_apps("a.b\tx\n".repeat(33).as_bytes()).is_err());
        assert!(
            Request::Lifecycle(Lifecycle {
                len: 1,
                ..Default::default()
            })
            .takes_fd()
        );
        assert!(!Request::Lifecycle(Lifecycle::default()).takes_fd());
    }
}
