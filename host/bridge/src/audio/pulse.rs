//! StreamIo to PipeWire through its PulseAudio protocol server
//! (`pipewire-pulse`), using the pure-Rust `pulseaudio` crate for the wire
//! format. One native-protocol connection per Android PCM stream, so
//! each shows up as its own stream in the desktop mixer.
//!
//! Why not libpipewire: the host has no PipeWire development headers (no
//! `-devel` packages may be installed), the pure-Rust `pipewire-native`
//! crate has no stream support yet, and the Pulse protocol is the stable,
//! well-tested way most desktop apps reach PipeWire. It also keeps the
//! bridge free of C dependencies, so it builds and lints like any Rust
//! crate.

use std::ffi::CString;
use std::io::{Cursor, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, anyhow, bail};
use pulseaudio::protocol::{self, Command, Prop, Props};

use super::proto::{SampleFormat, StreamConfig};
use super::ring::{Ring, STATE_IDLE, STATE_PLAYING};
use super::stream::{Ctl, StreamIo, monotonic_ns, playback_app_props, usage_name_role};

const SETUP_TIMEOUT: Duration = Duration::from_secs(2);
/// Largest control frame we accept from the server.
const MAX_FRAME: usize = 1 << 20;
/// Largest memblock we send at once.
const MAX_CHUNK: usize = 64 * 1024;
/// How often to refresh the timing estimate while playing.
const TIMING_INTERVAL: Duration = Duration::from_millis(20);

/// Target total latency (PipeWire queue plus device), in microseconds.
/// pipewire-pulse floors it at about 16 ms (three 5.3 ms quanta at 48 kHz),
/// so 10 ms measured the same.
pub const TARGET_LATENCY_US: u32 = 20_000;

pub struct PulseStream {
    input: bool,
    sock: UnixStream,
    rbuf: Vec<u8>,
    version: u16,
    channel: u32,
    sink_input: u32,
    seq: u32,
    frame_bytes: usize,
    rate: u32,
    channels: u8,
    /// Bytes the server has asked for and not yet received.
    pending: u64,
    corked: bool,
    standby: bool,
    timing_seq: Option<(u32, Instant)>,
    last_timing: Instant,
    volume: (f32, f32),
    master: (f32, bool),
    tmp: Vec<u8>,
    stats: Stats,
}

/// Counters logged every few seconds in verbose mode.
#[derive(Default)]
struct Stats {
    since: Option<Instant>,
    frames: u64,
    requests: u32,
    underflows: u32,
    /// Sum and count of downstream latency samples (ns) and of ring fill
    /// (frames) when each sample was taken.
    latency_ns: u64,
    fill: u64,
    samples: u64,
}

/// The native protocol's cookie length.
const COOKIE_LEN: usize = 256;

impl PulseStream {
    pub fn connect(cfg: &StreamConfig, device: Option<&str>, input: bool) -> Result<PulseStream> {
        let path = pulseaudio::socket_path_from_env().context("no PulseAudio/PipeWire socket")?;
        let sock =
            UnixStream::connect(&path).with_context(|| format!("connect {}", path.display()))?;
        sock.set_read_timeout(Some(SETUP_TIMEOUT))?;
        sock.set_write_timeout(Some(SETUP_TIMEOUT))?;
        let fb = cfg.frame_bytes() as usize;
        let mut p = PulseStream {
            input,
            sock,
            rbuf: Vec::with_capacity(4096),
            version: protocol::MAX_VERSION,
            channel: 0,
            sink_input: 0,
            seq: 0,
            frame_bytes: fb,
            rate: cfg.rate,
            channels: cfg.channels,
            pending: 0,
            corked: false,
            standby: false,
            timing_seq: None,
            last_timing: Instant::now(),
            volume: (1.0, 1.0),
            master: (1.0, false),
            tmp: vec![0; MAX_CHUNK - MAX_CHUNK % fb],
            stats: Stats::default(),
        };

        // Inside Flatpak there is no cookie file. pipewire-pulse refuses a
        // cookie that isn't 256 bytes but doesn't check its value (local
        // clients are authenticated by uid), so send zeros.
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|p| std::fs::read(p).ok())
            .filter(|c| c.len() == COOKIE_LEN)
            .unwrap_or_else(|| vec![0; COOKIE_LEN]);
        let seq = p.send(&Command::Auth(protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        }))?;
        let auth: protocol::AuthReply = p.wait_reply(seq)?;
        p.version = protocol::MAX_VERSION.min(auth.version);

        let mut props = Props::new();
        // The client defaults. A later Ctl::App(None) writes these back; the
        // empty process.binary in that set is only for clearing a stream key.
        for (key, value) in playback_app_props(None) {
            if value.is_empty() {
                continue;
            }
            set_app_prop(&mut props, key, value)?;
        }
        let seq = p.send(&Command::SetClientName(props))?;
        let _: protocol::SetClientNameReply = p.wait_reply(seq)?;

        let bytes_per_us = |us: u32| (us as u64 * cfg.rate as u64 / 1_000_000) as u32 * fb as u32;
        let (name, role) = usage_name_role(1);
        let mut sprops = Props::new();
        let label = if cfg.name.is_empty() {
            name.to_owned()
        } else {
            cfg.name.clone()
        };
        sprops.set(Prop::MediaName, CString::new(label)?);
        sprops.set(Prop::MediaRole, CString::new(role)?);
        let channel_map = if cfg.channels == 1 {
            protocol::ChannelMap::mono()
        } else {
            protocol::ChannelMap::stereo()
        };
        let sample_spec = protocol::SampleSpec {
            format: match cfg.format {
                SampleFormat::S16Le => protocol::SampleFormat::S16Le,
                SampleFormat::F32Le => protocol::SampleFormat::Float32Le,
            },
            channels: cfg.channels,
            sample_rate: cfg.rate,
        };
        if input {
            let seq = p.send(&Command::CreateRecordStream(protocol::RecordStreamParams {
                sample_spec,
                channel_map,
                source_name: device.map(CString::new).transpose()?,
                buffer_attr: protocol::stream::BufferAttr {
                    max_length: bytes_per_us(TARGET_LATENCY_US * 2),
                    fragment_size: bytes_per_us(TARGET_LATENCY_US / 2),
                    ..Default::default()
                },
                flags: protocol::stream::StreamFlags {
                    start_corked: true,
                    adjust_latency: true,
                    ..Default::default()
                },
                props: sprops,
                ..Default::default()
            }))?;
            let reply: protocol::CreateRecordStreamReply = p.wait_reply(seq)?;
            validate_spec(sample_spec, reply.sample_spec)?;
            p.channel = reply.channel;
            p.corked = true;
        } else {
            let seq = p.send(&Command::CreatePlaybackStream(
                protocol::PlaybackStreamParams {
                    sample_spec,
                    channel_map,
                    sink_name: Some(match device {
                        Some(s) => CString::new(s)?,
                        None => protocol::DEFAULT_SINK.to_owned(),
                    }),
                    buffer_attr: protocol::stream::BufferAttr {
                        max_length: u32::MAX,
                        target_length: bytes_per_us(TARGET_LATENCY_US),
                        pre_buffering: u32::MAX,
                        minimum_request_length: bytes_per_us(TARGET_LATENCY_US / 4),
                        fragment_size: u32::MAX,
                    },
                    props: sprops,
                    flags: protocol::stream::StreamFlags {
                        adjust_latency: true,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ))?;
            let reply: protocol::CreatePlaybackStreamReply = p.wait_reply(seq)?;
            validate_spec(sample_spec, reply.sample_spec)?;
            p.channel = reply.channel;
            p.sink_input = reply.stream_index;
            p.pending = reply.requested_bytes as u64;
        }
        p.sock.set_read_timeout(None)?;
        Ok(p)
    }

    fn send(&mut self, cmd: &Command) -> Result<u32> {
        self.seq = self.seq.wrapping_add(1);
        let mut buf = Vec::with_capacity(256);
        protocol::write_command_message(&mut buf, self.seq, cmd, self.version)?;
        self.sock.write_all(&buf)?;
        Ok(self.seq)
    }

    /// Blocking read of one frame (setup only). Returns descriptor + payload.
    fn read_frame_blocking(&mut self) -> Result<Vec<u8>> {
        let mut desc = [0u8; protocol::DESCRIPTOR_SIZE];
        self.sock.read_exact(&mut desc)?;
        let len = u32::from_be_bytes(desc[0..4].try_into().unwrap()) as usize;
        if len > MAX_FRAME {
            bail!("oversized frame from server ({len} bytes)");
        }
        let mut frame = desc.to_vec();
        frame.resize(protocol::DESCRIPTOR_SIZE + len, 0);
        self.sock
            .read_exact(&mut frame[protocol::DESCRIPTOR_SIZE..])?;
        Ok(frame)
    }

    fn wait_reply<T: protocol::CommandReply>(&mut self, seq: u32) -> Result<T> {
        loop {
            let frame = self.read_frame_blocking()?;
            if !is_control(&frame) {
                continue;
            }
            let (s, _) = Command::read_tag_prefixed(
                &mut Cursor::new(&frame[protocol::DESCRIPTOR_SIZE..]),
                self.version,
            )?;
            if s != seq {
                continue;
            }
            return protocol::read_reply_message::<T>(&mut Cursor::new(&frame[..]), self.version)
                .map(|(_, r)| r)
                .map_err(|e| anyhow!("server: {e:?}"));
        }
    }

    fn handle_frame(&mut self, ring: &mut Ring, frame: &[u8]) -> Result<()> {
        if !is_control(frame) {
            if self.input
                && !self.corked
                && u32::from_be_bytes(frame[4..8].try_into().unwrap()) == self.channel
            {
                let pcm = &frame[protocol::DESCRIPTOR_SIZE..];
                if !pcm.len().is_multiple_of(self.frame_bytes) {
                    bail!("unaligned capture data");
                }
                ring.write(pcm);
                ring.set_state(STATE_PLAYING);
            }
            return Ok(());
        }
        let mut cur = Cursor::new(&frame[protocol::DESCRIPTOR_SIZE..]);
        let (seq, cmd) = match Command::read_tag_prefixed(&mut cur, self.version) {
            Ok(v) => v,
            Err(e) => {
                crate::debug!("pulse: unparsed message: {e:?}");
                return Ok(());
            }
        };
        match cmd {
            Command::Request(r) if r.channel == self.channel => {
                self.stats.requests += 1;
                self.pending = self.pending.saturating_add(r.length as u64);
                self.pump(ring)?;
            }
            Command::Reply if self.timing_seq.is_some_and(|(s, _)| s == seq) => {
                let (_, sent) = self.timing_seq.take().unwrap();
                let lat: protocol::PlaybackLatency =
                    protocol::TagStructReader::new(&mut cur, self.version).read()?;
                self.on_timing(ring, &lat, sent);
            }
            Command::Error(e) => {
                if self.timing_seq.is_some_and(|(s, _)| s == seq) {
                    self.timing_seq = None;
                }
                crate::warn!("pulse: error reply to {seq}: {e:?}");
            }
            Command::Underflow(u) if u.channel == self.channel => {
                self.stats.underflows += 1;
                if self.standby && !self.corked && ring.available() == 0 {
                    self.cork(ring, true)?;
                }
            }
            Command::PlaybackStreamKilled(ch) | Command::RecordStreamKilled(ch)
                if ch == self.channel =>
            {
                bail!("stream killed by the server")
            }
            _ => {}
        }
        Ok(())
    }

    fn on_timing(&mut self, ring: &Ring, lat: &protocol::PlaybackLatency, sent: Instant) {
        let now = Instant::now();
        // The server sampled its state roughly halfway through the round trip.
        let mid_ns = monotonic_ns().saturating_sub((now - sent).as_nanos() as u64 / 2);
        let fb = self.frame_bytes as i64;
        let sink_frames = (lat.sink_usec as i64).saturating_mul(self.rate as i64) / 1_000_000;
        let played = (lat.read_offset / fb - sink_frames).max(0) as u64;
        let queued_bytes = (lat.write_offset - lat.read_offset).max(0);
        let latency_ns = lat.sink_usec.saturating_mul(1000)
            + (queued_bytes as u64 / self.frame_bytes as u64) * 1_000_000_000 / self.rate as u64;
        ring.publish_timing(played.min(ring.read_pos()), mid_ns, latency_ns);
        self.stats.latency_ns += latency_ns;
        self.stats.fill += ring.available();
        self.stats.samples += 1;
        ring.set_state(if lat.playing && !self.corked {
            STATE_PLAYING
        } else {
            STATE_IDLE
        });
    }

    fn cork(&mut self, ring: &Ring, cork: bool) -> Result<()> {
        let params = protocol::stream::CorkStreamParams {
            channel: self.channel,
            cork,
        };
        self.send(&if self.input {
            Command::CorkRecordStream(params)
        } else {
            Command::CorkPlaybackStream(params)
        })?;
        self.corked = cork;
        if cork {
            ring.set_state(STATE_IDLE);
        }
        crate::debug!("pulse: {}corked", if cork { "" } else { "un" });
        Ok(())
    }

    /// Moves as much as the server wants and the ring has.
    fn pump(&mut self, ring: &mut Ring) -> Result<bool> {
        let fb = self.frame_bytes;
        let mut moved = false;
        loop {
            let want = (self.pending / fb as u64).min(ring.available()) as usize;
            if want == 0 {
                break;
            }
            let n = want.min(self.tmp.len() / fb);
            let got = ring.read(&mut self.tmp[..n * fb]);
            let mut buf = Vec::with_capacity(protocol::DESCRIPTOR_SIZE + got * fb);
            protocol::write_memblock(&mut buf, self.channel, &self.tmp[..got * fb], 0)?;
            self.sock.write_all(&buf)?;
            self.pending -= (got * fb) as u64;
            self.stats.frames += got as u64;
            moved = true;
        }
        let since = *self.stats.since.get_or_insert_with(Instant::now);
        if crate::log::verbose() && since.elapsed() >= Duration::from_secs(5) {
            let s = &self.stats;
            let n = s.samples.max(1);
            crate::debug!(
                "pulse ch {}: {} frames in {:.1} s, {} requests, {} underflows; avg ring fill {:.1} ms + downstream {:.1} ms{}",
                self.channel,
                s.frames,
                since.elapsed().as_secs_f32(),
                s.requests,
                s.underflows,
                (s.fill / n) as f64 * 1000.0 / self.rate as f64,
                (s.latency_ns / n) as f64 / 1e6,
                if self.corked { ", corked" } else { "" }
            );
            self.stats = Stats::default();
        }
        if moved && self.timing_seq.is_none() && self.last_timing.elapsed() >= TIMING_INTERVAL {
            self.last_timing = Instant::now();
            let seq = self.send(&Command::GetPlaybackLatency(protocol::LatencyParams {
                channel: self.channel,
                now: SystemTime::now(),
            }))?;
            self.timing_seq = Some((seq, Instant::now()));
        }
        Ok(moved)
    }

    fn replace_props(&mut self, props: Props) -> Result<()> {
        self.send(&Command::UpdatePlaybackStreamProplist(
            protocol::UpdatePropsParams {
                index: self.channel,
                mode: protocol::props::PropsUpdateMode::Replace,
                props,
            },
        ))?;
        Ok(())
    }

    fn apply_volume(&mut self) -> Result<()> {
        let (mv, mute) = self.master;
        let mut cv = protocol::ChannelVolume::empty();
        let (l, r) = self.volume;
        let vols: &[f32] = if self.channels == 1 {
            &[(l + r) / 2.0]
        } else {
            &[l, r]
        };
        for v in vols {
            cv.push(protocol::Volume::from_linear(v * mv));
        }
        self.send(&Command::SetSinkInputVolume(
            protocol::SetStreamVolumeParams {
                index: self.sink_input,
                volume: cv,
            },
        ))?;
        self.send(&Command::SetSinkInputMute(protocol::SetStreamMuteParams {
            index: self.sink_input,
            mute,
        }))?;
        Ok(())
    }
}

fn is_control(frame: &[u8]) -> bool {
    frame.len() >= protocol::DESCRIPTOR_SIZE && frame[4..8] == [0xff; 4]
}

impl StreamIo for PulseStream {
    fn poll_fd(&self) -> BorrowedFd<'_> {
        self.sock.as_fd()
    }

    fn on_readable(&mut self, ring: &mut Ring) -> Result<bool> {
        let mut buf = [0u8; 8192];
        loop {
            match rustix::net::recv(&self.sock, &mut buf, rustix::net::RecvFlags::DONTWAIT) {
                Ok((0, _)) => bail!("server closed the connection"),
                Ok((n, _)) => self.rbuf.extend_from_slice(&buf[..n]),
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(e) => return Err(e.into()),
            }
            if self.rbuf.len() > MAX_FRAME + protocol::DESCRIPTOR_SIZE {
                break;
            }
        }
        let before = ring.read_pos();
        let mut off = 0;
        while self.rbuf.len() - off >= protocol::DESCRIPTOR_SIZE {
            let len = u32::from_be_bytes(self.rbuf[off..off + 4].try_into().unwrap()) as usize;
            if len > MAX_FRAME {
                bail!("oversized frame from server ({len} bytes)");
            }
            let end = off + protocol::DESCRIPTOR_SIZE + len;
            if self.rbuf.len() < end {
                break;
            }
            let frame = self.rbuf[off..end].to_vec();
            self.handle_frame(ring, &frame)?;
            off = end;
        }
        self.rbuf.drain(..off);
        Ok(self.input || ring.read_pos() != before)
    }

    fn on_data(&mut self, ring: &mut Ring) -> Result<bool> {
        if self.corked {
            self.cork(ring, false)?;
        }
        self.standby = false;
        if self.input {
            Ok(false)
        } else {
            self.pump(ring)
        }
    }

    fn control(&mut self, ring: &mut Ring, c: Ctl) -> Result<()> {
        if self.input {
            if c == Ctl::Standby {
                self.cork(ring, true)?;
            }
            return Ok(());
        }
        match c {
            Ctl::Volume(l, r) => {
                self.volume = (l, r);
                self.apply_volume()?;
            }
            Ctl::Master(v, m) => {
                self.master = (v, m);
                self.apply_volume()?;
            }
            Ctl::Standby => {
                self.standby = true;
                if ring.available() == 0 && !self.corked {
                    // Let what's queued play out; the Underflow event corks.
                    crate::debug!("pulse: standby");
                }
            }
            Ctl::Metadata(usage, label) => {
                // Record streams have no playback proplist to update. Capture
                // metadata is dropped above, with the other playback controls.
                let (name, role) = usage_name_role(usage);
                let mut props = Props::new();
                let label = if label.is_empty() {
                    name.to_owned()
                } else {
                    label
                };
                props.set(Prop::MediaName, CString::new(label)?);
                props.set(Prop::MediaRole, CString::new(role)?);
                self.replace_props(props)?;
            }
            Ctl::App(app) => {
                let mut props = Props::new();
                for (key, value) in playback_app_props(app.as_ref()) {
                    set_app_prop(&mut props, key, value)?;
                }
                self.replace_props(props)?;
            }
        }
        Ok(())
    }
}

fn set_app_prop(props: &mut Props, key: &str, value: &str) -> Result<()> {
    let value = CString::new(value)?;
    match key {
        "application.name" => props.set(Prop::ApplicationName, value),
        "application.icon_name" => props.set(Prop::ApplicationIconName, value),
        "application.id" => props.set(Prop::ApplicationId, value),
        "application.process.binary" => props.set(Prop::ApplicationProcessBinary, value),
        _ => bail!("unknown application property {key}"),
    }
    Ok(())
}

/// Pulse must resample/remix to the requested PCM ABI, never silently change it.
fn validate_spec(want: protocol::SampleSpec, got: protocol::SampleSpec) -> Result<()> {
    if want != got {
        bail!("server changed PCM specification: {want:?} -> {got:?}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_changed_capture_spec() {
        let want = protocol::SampleSpec {
            format: protocol::SampleFormat::S16Le,
            channels: 1,
            sample_rate: 48000,
        };
        assert!(validate_spec(want, want).is_ok());
        for got in [
            protocol::SampleSpec {
                sample_rate: 44100,
                ..want
            },
            protocol::SampleSpec {
                channels: 2,
                ..want
            },
            protocol::SampleSpec {
                format: protocol::SampleFormat::Float32Le,
                ..want
            },
        ] {
            assert!(validate_spec(want, got).is_err());
        }
    }
}
