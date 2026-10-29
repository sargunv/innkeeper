//! Android media sessions as MPRIS players, so desktop media controls and
//! media keys drive them.
//!
//! The system app sends a media blob per active `MediaSession` (keyed by its
//! own nonzero player id) whenever its metadata or state changes. Each player
//! is its own session-bus connection owning
//! `org.mpris.MediaPlayer2.dev.linuxapk.Manager.<package, _ for .>` (plus
//! `.instance<N>` if the package has two), serving `/org/mpris/MediaPlayer2`. MPRIS calls come
//! back as `MEDIA_CMD` events. Artwork arrives as RGBA and is written as our
//! own PNG encoding to a private directory (`mpris:artUrl` must be a URL).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use linuxapk_core::desktop::MANAGER_ID;
use zbus::object_server::SignalEmitter;
use zbus::zvariant::{ObjectPath, OwnedValue, Value};

use super::proto::{ev, media_cmd};
use crate::ipc::blob::{Doc, Image};
use crate::ipc::server::Events;
use crate::ipc::wire::DecodeError;

pub const MAX_PER_CLIENT: usize = 8;
const PATH: &str = "/org/mpris/MediaPlayer2";
const MAX_ART_SIDE: u16 = 512;
/// Longest duration or position we accept: about four months.
const MAX_US: i64 = 10_000_000_000_000;

pub mod actions {
    pub const PLAY: u32 = 1;
    pub const PAUSE: u32 = 2;
    pub const NEXT: u32 = 4;
    pub const PREVIOUS: u32 = 8;
    pub const SEEK: u32 = 16;
    pub const STOP: u32 = 32;
    pub const ALL: u32 = 63;
}

/// Playback states.
pub mod state {
    pub const STOPPED: u8 = 0;
    pub const PAUSED: u8 = 1;
    pub const PLAYING: u8 = 2;
}

/// A media blob, decoded.
#[derive(Debug, Clone, PartialEq)]
pub struct Media {
    pub package: String,
    pub app_name: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub state: u8,
    pub actions: u32,
    /// Microseconds; -1 unknown.
    pub duration_us: i64,
    pub position_us: i64,
    pub rate: f32,
    pub art: Option<Image>,
}

pub const MEDIA_VERSION: u32 = 1;

impl Media {
    /// Layout: `u32 version (1)`, text package, app name, title, artist,
    /// album; `u8 state`, `u8 0`, `u16 0`, `u32 actions`, `i64 duration_us`,
    /// `i64 position_us`, `f32 rate`, then the artwork (at most 512x512).
    pub fn decode(b: &[u8]) -> Result<Media, DecodeError> {
        let mut d = Doc::new(b);
        if d.0.u32()? != MEDIA_VERSION {
            return Err(DecodeError::Invalid("version"));
        }
        let package = d.text(255)?;
        if !super::valid_package(&package) {
            return Err(DecodeError::Invalid("package"));
        }
        let app_name = d.text(256)?;
        let title = d.text(1024)?;
        let artist = d.text(1024)?;
        let album = d.text(1024)?;
        let st = d.0.u8()?;
        if st > state::PLAYING {
            return Err(DecodeError::Invalid("state"));
        }
        if d.0.u8()? != 0 || d.0.u16()? != 0 {
            return Err(DecodeError::Invalid("reserved"));
        }
        let acts = d.0.u32()?;
        if acts & !actions::ALL != 0 {
            return Err(DecodeError::Invalid("actions"));
        }
        let us = |v: u64| -> Result<i64, DecodeError> {
            let v = v as i64;
            if v == -1 || (0..=MAX_US).contains(&v) {
                Ok(v)
            } else {
                Err(DecodeError::Invalid("time"))
            }
        };
        let duration_us = us(d.0.u64()?)?;
        let position_us = us(d.0.u64()?)?;
        let rate = d.0.f32()?;
        if !rate.is_finite() || rate.abs() > 16.0 {
            return Err(DecodeError::Invalid("rate"));
        }
        let art = d.image(MAX_ART_SIDE)?;
        d.end()?;
        Ok(Media {
            package,
            app_name,
            title,
            artist,
            album,
            state: st,
            actions: acts,
            duration_us,
            position_us,
            rate,
            art,
        })
    }

    pub fn encode(&self) -> Vec<u8> {
        use crate::ipc::blob::DocWriter;
        let mut w = crate::ipc::wire::Writer::default();
        w.u32(MEDIA_VERSION)
            .text(&self.package)
            .text(&self.app_name)
            .text(&self.title)
            .text(&self.artist)
            .text(&self.album)
            .u8(self.state)
            .u8(0)
            .u16(0)
            .u32(self.actions)
            .u64(self.duration_us as u64)
            .u64(self.position_us as u64)
            .f32(self.rate)
            .image(self.art.as_ref());
        w.into_vec()
    }
}

/// A player's state, shared by its two D-Bus interfaces.
struct PState {
    media: Media,
    /// When `media.position_us` was true.
    at: Instant,
    track: u64,
    art_url: String,
}

impl PState {
    fn position(&self) -> i64 {
        let m = &self.media;
        if m.position_us < 0 {
            return 0;
        }
        let mut p = m.position_us;
        if m.state == state::PLAYING {
            p += (self.at.elapsed().as_micros() as f64 * m.rate as f64) as i64;
        }
        if m.duration_us > 0 {
            p = p.min(m.duration_us);
        }
        p.max(0)
    }
}

struct Target {
    events: Events,
    player: u32,
}

impl Target {
    fn send(&self, cmd: u8, arg: i64) {
        crate::info!("mpris: player {}: command {cmd} {arg}", self.player);
        let _ = self.events.try_send(
            ev::MEDIA_CMD,
            &super::proto::media_cmd(self.player, cmd, arg),
            &[],
        );
    }
}

struct Root {
    st: Arc<Mutex<PState>>,
    target: Arc<Target>,
}

#[zbus::interface(name = "org.mpris.MediaPlayer2")]
impl Root {
    fn raise(&self) {
        self.target.send(media_cmd::RAISE, 0);
    }
    fn quit(&self) {}
    #[zbus(property)]
    fn can_quit(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn can_raise(&self) -> bool {
        true
    }
    #[zbus(property)]
    fn has_track_list(&self) -> bool {
        false
    }
    #[zbus(property)]
    fn identity(&self) -> String {
        self.st.lock().unwrap().media.app_name.clone()
    }
    #[zbus(property)]
    fn desktop_entry(&self) -> String {
        linuxapk_core::desktop::app_id(&self.st.lock().unwrap().media.package)
    }
    #[zbus(property)]
    fn supported_uri_schemes(&self) -> Vec<String> {
        Vec::new()
    }
    #[zbus(property)]
    fn supported_mime_types(&self) -> Vec<String> {
        Vec::new()
    }
}

struct Player {
    st: Arc<Mutex<PState>>,
    target: Arc<Target>,
}

impl Player {
    fn can(&self, a: u32) -> bool {
        self.st.lock().unwrap().media.actions & a != 0
    }
}

#[zbus::interface(name = "org.mpris.MediaPlayer2.Player")]
impl Player {
    fn next(&self) {
        self.target.send(media_cmd::NEXT, 0);
    }
    fn previous(&self) {
        self.target.send(media_cmd::PREVIOUS, 0);
    }
    fn pause(&self) {
        self.target.send(media_cmd::PAUSE, 0);
    }
    fn play_pause(&self) {
        self.target.send(media_cmd::PLAY_PAUSE, 0);
    }
    fn stop(&self) {
        self.target.send(media_cmd::STOP, 0);
    }
    fn play(&self) {
        self.target.send(media_cmd::PLAY, 0);
    }
    fn seek(&self, offset: i64) {
        self.target
            .send(media_cmd::SEEK, offset.clamp(-MAX_US, MAX_US));
    }
    fn set_position(&self, track_id: ObjectPath<'_>, position: i64) {
        let st = self.st.lock().unwrap();
        // The spec: ignore stale track ids and positions out of range.
        if track_id.as_str() != track_path(st.track)
            || position < 0
            || (st.media.duration_us > 0 && position > st.media.duration_us)
        {
            return;
        }
        drop(st);
        self.target.send(media_cmd::SET_POSITION, position);
    }
    fn open_uri(&self, _uri: &str) {}

    #[zbus(signal)]
    async fn seeked(emitter: &SignalEmitter<'_>, position: i64) -> zbus::Result<()>;

    #[zbus(property)]
    fn playback_status(&self) -> String {
        match self.st.lock().unwrap().media.state {
            state::PLAYING => "Playing",
            state::PAUSED => "Paused",
            _ => "Stopped",
        }
        .into()
    }
    #[zbus(property)]
    fn rate(&self) -> f64 {
        let r = self.st.lock().unwrap().media.rate as f64;
        if r > 0.0 { r } else { 1.0 }
    }
    #[zbus(property)]
    fn set_rate(&mut self, _rate: f64) {}
    #[zbus(property)]
    fn metadata(&self) -> HashMap<String, OwnedValue> {
        let st = self.st.lock().unwrap();
        let m = &st.media;
        let mut md: HashMap<String, OwnedValue> = HashMap::new();
        let mut put = |k: &str, v: Value<'_>| {
            if let Ok(v) = v.try_to_owned() {
                md.insert(k.into(), v);
            }
        };
        if let Ok(p) = ObjectPath::try_from(track_path(st.track)) {
            put("mpris:trackid", Value::from(p));
        }
        if m.duration_us > 0 {
            put("mpris:length", Value::from(m.duration_us));
        }
        if !m.title.is_empty() {
            put("xesam:title", Value::from(m.title.as_str()));
        }
        if !m.artist.is_empty() {
            put("xesam:artist", Value::from(vec![m.artist.as_str()]));
        }
        if !m.album.is_empty() {
            put("xesam:album", Value::from(m.album.as_str()));
        }
        if !st.art_url.is_empty() {
            put("mpris:artUrl", Value::from(st.art_url.as_str()));
        }
        md
    }
    #[zbus(property)]
    fn volume(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn set_volume(&mut self, _v: f64) {}
    #[zbus(property(emits_changed_signal = "false"))]
    fn position(&self) -> i64 {
        self.st.lock().unwrap().position()
    }
    #[zbus(property)]
    fn minimum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn maximum_rate(&self) -> f64 {
        1.0
    }
    #[zbus(property)]
    fn can_go_next(&self) -> bool {
        self.can(actions::NEXT)
    }
    #[zbus(property)]
    fn can_go_previous(&self) -> bool {
        self.can(actions::PREVIOUS)
    }
    #[zbus(property)]
    fn can_play(&self) -> bool {
        self.can(actions::PLAY)
    }
    #[zbus(property)]
    fn can_pause(&self) -> bool {
        self.can(actions::PAUSE)
    }
    #[zbus(property)]
    fn can_seek(&self) -> bool {
        self.can(actions::SEEK)
    }
    #[zbus(property)]
    fn can_control(&self) -> bool {
        true
    }
}

fn track_path(n: u64) -> String {
    format!("/dev/linuxapk/track/{n}")
}

struct Handle {
    conn: zbus::blocking::Connection,
    st: Arc<Mutex<PState>>,
    art_file: Option<PathBuf>,
}

pub struct MprisHub {
    art_dir: PathBuf,
    players: Mutex<HashMap<(u64, u32), Handle>>,
}

impl MprisHub {
    pub fn new(art_dir: PathBuf) -> Arc<MprisHub> {
        Arc::new(MprisHub {
            art_dir,
            players: Mutex::default(),
        })
    }

    pub fn count(&self, client: u64) -> usize {
        let p = self.players.lock().unwrap();
        p.keys().filter(|(c, _)| *c == client).count()
    }

    pub fn contains(&self, client: u64, player: u32) -> bool {
        self.players.lock().unwrap().contains_key(&(client, player))
    }

    /// Creates or updates (client, player).
    pub fn update(&self, client: u64, events: &Events, player: u32, media: Media) -> Result<()> {
        let mut players = self.players.lock().unwrap();
        let art = media.art.clone();
        if let Some(h) = players.get_mut(&(client, player)) {
            let art_url = self.write_art(client, player, art.as_ref(), &mut h.art_file);
            let (seeked, changed) = {
                let mut st = h.st.lock().unwrap();
                let expected = st.position();
                let new_track = st.media.title != media.title
                    || st.media.artist != media.artist
                    || st.media.album != media.album;
                if new_track {
                    st.track += 1;
                }
                let jumped = media.position_us >= 0
                    && (media.position_us - expected).abs() > 1_500_000
                    && !new_track;
                let changed = new_track
                    || st.media.state != media.state
                    || st.media.actions != media.actions
                    || st.media.duration_us != media.duration_us
                    || st.media.rate != media.rate
                    || art_url != st.art_url;
                st.media = media;
                st.at = Instant::now();
                st.art_url = art_url;
                (jumped.then(|| st.position()), changed)
            };
            let iface = h
                .conn
                .object_server()
                .interface::<_, Player>(PATH)
                .context("player interface")?;
            if changed {
                let em = iface.signal_emitter();
                let p = iface.get();
                zbus::block_on(async {
                    let _ = p.playback_status_changed(em).await;
                    let _ = p.metadata_changed(em).await;
                    let _ = p.can_play_changed(em).await;
                    let _ = p.can_pause_changed(em).await;
                    let _ = p.can_go_next_changed(em).await;
                    let _ = p.can_go_previous_changed(em).await;
                    let _ = p.can_seek_changed(em).await;
                    let _ = p.rate_changed(em).await;
                });
            }
            if let Some(pos) = seeked {
                let _ = zbus::block_on(Player::seeked(iface.signal_emitter(), pos));
            }
            return Ok(());
        }
        let mut art_file = None;
        let art_url = self.write_art(client, player, art.as_ref(), &mut art_file);
        // Under the manager's id, which Flatpak lets the app own.
        let base = format!(
            "org.mpris.MediaPlayer2.{MANAGER_ID}.{}",
            media.package.replace('.', "_")
        );
        let base = crate::ipc::wire::truncate(&base, 200).to_owned();
        let st = Arc::new(Mutex::new(PState {
            media,
            at: Instant::now(),
            track: 1,
            art_url,
        }));
        let target = Arc::new(Target {
            events: events.clone(),
            player,
        });
        let build = |name: &str| -> zbus::Result<zbus::blocking::Connection> {
            zbus::blocking::connection::Builder::session()?
                .name(name.to_owned())?
                .serve_at(
                    PATH,
                    Root {
                        st: st.clone(),
                        target: target.clone(),
                    },
                )?
                .serve_at(
                    PATH,
                    Player {
                        st: st.clone(),
                        target: target.clone(),
                    },
                )?
                .build()
        };
        let conn = match build(&base) {
            Ok(c) => c,
            Err(_) => build(&format!("{base}.instance{client}_{player}"))
                .context("registering the MPRIS player")?,
        };
        crate::info!(
            "mpris: player {player} as {}",
            conn.unique_name()
                .map(|n| n.to_string())
                .unwrap_or_default()
        );
        players.insert((client, player), Handle { conn, st, art_file });
        Ok(())
    }

    pub fn remove(&self, client: u64, player: u32) {
        if let Some(h) = self.players.lock().unwrap().remove(&(client, player)) {
            if let Some(f) = h.art_file {
                let _ = std::fs::remove_file(f);
            }
            crate::info!("mpris: player {player} removed");
            drop(h.conn);
        }
    }

    pub fn remove_all(&self, client: u64) {
        let ids: Vec<u32> = {
            let p = self.players.lock().unwrap();
            p.keys()
                .filter(|(c, _)| *c == client)
                .map(|k| k.1)
                .collect()
        };
        for id in ids {
            self.remove(client, id);
        }
    }

    /// Writes the artwork as PNG (only when it changed); returns its URL.
    fn write_art(
        &self,
        client: u64,
        player: u32,
        art: Option<&Image>,
        file: &mut Option<PathBuf>,
    ) -> String {
        let Some(img) = art else {
            if let Some(f) = file.take() {
                let _ = std::fs::remove_file(f);
            }
            return String::new();
        };
        let hash = {
            use std::hash::Hasher;
            let mut h = std::collections::hash_map::DefaultHasher::new();
            img.hash_bytes(&mut h);
            h.finish()
        };
        let path = self
            .art_dir
            .join(format!("art-{client}-{player}-{hash:016x}.png"));
        if file.as_ref() != Some(&path) {
            let written = (|| -> Result<()> {
                std::fs::create_dir_all(&self.art_dir)?;
                let png = super::clipboard::encode_png(img)?;
                let tmp = path.with_extension("tmp");
                std::fs::write(&tmp, png)?;
                std::fs::rename(&tmp, &path)?;
                Ok(())
            })();
            if let Err(e) = written {
                crate::warn!("mpris: artwork: {e:#}");
                return String::new();
            }
            if let Some(old) = file.replace(path.clone()) {
                let _ = std::fs::remove_file(old);
            }
        }
        format!("file://{}", path.display())
    }
}

impl Image {
    fn hash_bytes<H: std::hash::Hasher>(&self, h: &mut H) {
        use std::hash::Hash;
        (self.width, self.height, &self.rgba).hash(h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Media {
        Media {
            package: "org.example.player".into(),
            app_name: "Player".into(),
            title: "Song".into(),
            artist: "Artist".into(),
            album: "Album".into(),
            state: state::PLAYING,
            actions: actions::PLAY | actions::PAUSE | actions::NEXT,
            duration_us: 180_000_000,
            position_us: 5_000_000,
            rate: 1.0,
            art: Some(Image {
                width: 1,
                height: 1,
                rgba: vec![1, 2, 3, 255],
            }),
        }
    }

    #[test]
    fn roundtrip_and_checks() {
        let m = sample();
        assert_eq!(Media::decode(&m.encode()), Ok(m.clone()));
        for bad in [
            Media {
                state: 3,
                ..m.clone()
            },
            Media {
                actions: 64,
                ..m.clone()
            },
            Media {
                duration_us: -5,
                ..m.clone()
            },
            Media {
                rate: f32::NAN,
                ..m.clone()
            },
            Media {
                package: "a b".into(),
                ..m.clone()
            },
        ] {
            assert!(Media::decode(&bad.encode()).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn position_extrapolates() {
        let st = PState {
            media: sample(),
            at: Instant::now() - std::time::Duration::from_secs(2),
            track: 1,
            art_url: String::new(),
        };
        let p = st.position();
        assert!((6_900_000..7_500_000).contains(&p), "{p}");
    }
}
