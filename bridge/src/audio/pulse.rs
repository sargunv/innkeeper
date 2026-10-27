//! Output to PipeWire through its PulseAudio protocol server
//! (`pipewire-pulse`), using the pure-Rust `pulseaudio` crate for the wire
//! format. One native-protocol connection per Android output stream, so
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

use super::proto::{OutputConfig, SampleFormat};
use super::ring::{Ring, STATE_IDLE, STATE_PLAYING};
use super::stream::{Ctl, Output, monotonic_ns, usage_name_role};

const SETUP_TIMEOUT: Duration = Duration::from_secs(2);
/// Largest control frame we accept from the server.
const MAX_FRAME: usize = 1 << 20;
/// Largest memblock we send at once.
const MAX_CHUNK: usize = 64 * 1024;
/// How often to refresh the timing estimate while playing.
const TIMING_INTERVAL: Duration = Duration::from_millis(20);

/// Target total latency (PipeWire queue plus device), in microseconds.
pub const TARGET_LATENCY_US: u32 = 20_000;

pub struct PulseOut {
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
struct Stats {
    since: Instant,
    frames: u64,
    requests: u32,
    underflows: u32,
}

impl PulseOut {
    pub fn connect(cfg: &OutputConfig, sink: Option<&str>) -> Result<PulseOut> {
        let path = pulseaudio::socket_path_from_env().context("no PulseAudio/PipeWire socket")?;
        let sock =
            UnixStream::connect(&path).with_context(|| format!("connect {}", path.display()))?;
        sock.set_read_timeout(Some(SETUP_TIMEOUT))?;
        sock.set_write_timeout(Some(SETUP_TIMEOUT))?;
        let fb = cfg.frame_bytes() as usize;
        let mut p = PulseOut {
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
            stats: Stats {
                since: Instant::now(),
                frames: 0,
                requests: 0,
                underflows: 0,
            },
        };

        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|p| std::fs::read(p).ok())
            .unwrap_or_default();
        let seq = p.send(&Command::Auth(protocol::AuthParams {
            version: protocol::MAX_VERSION,
            supports_shm: false,
            supports_memfd: false,
            cookie,
        }))?;
        let auth: protocol::AuthReply = p.wait_reply(seq)?;
        p.version = protocol::MAX_VERSION.min(auth.version);

        let mut props = Props::new();
        props.set(Prop::ApplicationName, c"Linux.apk");
        props.set(Prop::ApplicationId, c"dev.linuxapk");
        props.set(Prop::ApplicationIconName, c"android");
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
        let seq = p.send(&Command::CreatePlaybackStream(
            protocol::PlaybackStreamParams {
                sample_spec: protocol::SampleSpec {
                    format: match cfg.format {
                        SampleFormat::S16Le => protocol::SampleFormat::S16Le,
                        SampleFormat::F32Le => protocol::SampleFormat::Float32Le,
                    },
                    channels: cfg.channels,
                    sample_rate: cfg.rate,
                },
                channel_map,
                sink_name: Some(match sink {
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
        p.channel = reply.channel;
        p.sink_input = reply.stream_index;
        p.pending = reply.requested_bytes as u64;
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
            Command::PlaybackStreamKilled(ch) if ch == self.channel => {
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
        ring.set_state(if lat.playing && !self.corked {
            STATE_PLAYING
        } else {
            STATE_IDLE
        });
    }

    fn cork(&mut self, ring: &Ring, cork: bool) -> Result<()> {
        self.send(&Command::CorkPlaybackStream(
            protocol::stream::CorkStreamParams {
                channel: self.channel,
                cork,
            },
        ))?;
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
        if crate::log::verbose() && self.stats.since.elapsed() >= Duration::from_secs(5) {
            let s = &self.stats;
            crate::debug!(
                "pulse ch {}: {} frames in {:.1} s, {} requests, {} underflows, pending {} B, ring {}/{}{}",
                self.channel,
                s.frames,
                s.since.elapsed().as_secs_f32(),
                s.requests,
                s.underflows,
                self.pending,
                ring.available(),
                ring.capacity(),
                if self.corked { ", corked" } else { "" }
            );
            self.stats = Stats {
                since: Instant::now(),
                frames: 0,
                requests: 0,
                underflows: 0,
            };
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

impl Output for PulseOut {
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
        Ok(ring.read_pos() != before)
    }

    fn on_data(&mut self, ring: &mut Ring) -> Result<bool> {
        if self.corked {
            self.cork(ring, false)?;
        }
        self.standby = false;
        self.pump(ring)
    }

    fn control(&mut self, ring: &mut Ring, c: Ctl) -> Result<()> {
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
                let (name, role) = usage_name_role(usage);
                let mut props = Props::new();
                let label = if label.is_empty() {
                    name.to_owned()
                } else {
                    label
                };
                props.set(Prop::MediaName, CString::new(label)?);
                props.set(Prop::MediaRole, CString::new(role)?);
                self.send(&Command::UpdatePlaybackStreamProplist(
                    protocol::UpdatePropsParams {
                        index: self.channel,
                        mode: protocol::props::PropsUpdateMode::Replace,
                        props,
                    },
                ))?;
            }
        }
        Ok(())
    }
}
