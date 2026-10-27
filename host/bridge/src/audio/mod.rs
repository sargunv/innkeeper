//! The audio service: output streams from the Android audio HAL to PipeWire.

pub mod proto;
pub mod pulse;
pub mod ring;
pub mod stream;

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rustix::event::EventfdFlags;

use crate::ipc::server::{Peer, Reply, Service};
use crate::ipc::wire::errno::*;
use proto::{OpenOutputReply, OutputConfig, Request};
use ring::{DATA_OFFSET, Ring};
use stream::{Ctl, NullOut, Output, StreamHandle};

/// Where streams play.
#[derive(Debug, Clone)]
pub enum Backend {
    /// PipeWire through its Pulse protocol server; `sink` is a sink name
    /// (default: the desktop's default output).
    Pulse { sink: Option<String> },
    /// Nowhere, at the nominal rate.
    Null,
}

/// Per-connection and global caps. A connection is one Android process.
#[derive(Debug, Clone)]
pub struct Limits {
    pub streams_per_client: usize,
    pub ring_bytes_per_client: usize,
    pub streams_total: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            streams_per_client: 8,
            ring_bytes_per_client: 8 << 20,
            streams_total: 32,
        }
    }
}

pub struct AudioService {
    backend: Backend,
    limits: Limits,
    total: Arc<AtomicUsize>,
    streams: HashMap<u32, StreamHandle>,
    next_id: u32,
    master: (f32, bool),
    peer: String,
}

impl AudioService {
    /// `total` counts streams across all connections (for the global cap).
    pub fn new(backend: Backend, limits: Limits, total: Arc<AtomicUsize>, peer: &Peer) -> Self {
        AudioService {
            backend,
            limits,
            total,
            streams: HashMap::new(),
            next_id: 1,
            master: (1.0, false),
            peer: peer.label.clone(),
        }
    }

    fn remove(&mut self, id: u32) -> bool {
        if self.streams.remove(&id).is_some() {
            self.total.fetch_sub(1, Ordering::AcqRel);
            true
        } else {
            false
        }
    }

    fn open_output(&mut self, cfg: OutputConfig) -> Reply {
        // Streams whose thread died (PipeWire went away) still count until
        // the client closes them; reap them here so they can reconnect.
        let dead: Vec<u32> = self
            .streams
            .iter()
            .filter(|(_, s)| s.is_finished())
            .map(|(id, _)| *id)
            .collect();
        for id in dead {
            self.remove(id);
        }
        let Some(map_size) = Ring::map_size(cfg.frame_bytes(), cfg.capacity_frames()) else {
            return Reply::err(-EINVAL);
        };
        let used: usize = self.streams.values().map(|s| s.ring_bytes).sum();
        if self.streams.len() >= self.limits.streams_per_client
            || used + map_size > self.limits.ring_bytes_per_client
        {
            return Reply::err(-EBUSY);
        }
        if self.total.fetch_add(1, Ordering::AcqRel) >= self.limits.streams_total {
            self.total.fetch_sub(1, Ordering::AcqRel);
            return Reply::err(-EBUSY);
        }
        match self.spawn(&cfg, map_size) {
            Ok(r) => r,
            Err(e) => {
                self.total.fetch_sub(1, Ordering::AcqRel);
                crate::warn!("{}: open output failed: {e:#}", self.peer);
                Reply::err(-EIO)
            }
        }
    }

    fn spawn(&mut self, cfg: &OutputConfig, map_size: usize) -> anyhow::Result<Reply> {
        let ring = Ring::new(cfg.frame_bytes(), cfg.capacity_frames(), cfg.rate)?;
        debug_assert_eq!(ring.map_len(), map_size);
        // The client shares the data eventfd's open file description, so it
        // can clear O_NONBLOCK: the stream thread reads it with RWF_NOWAIT.
        // Space is a pipe: the client gets the read end, and the write end's
        // flags are ours alone (an eventfd write can't be made non-blocking
        // per call, and a saturated counter would block it).
        let flags = EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK;
        let data = rustix::event::eventfd(0, flags)?;
        let (space_rd, space) = rustix::pipe::pipe_with(
            rustix::pipe::PipeFlags::CLOEXEC | rustix::pipe::PipeFlags::NONBLOCK,
        )?;
        let send: Vec<OwnedFd> = vec![
            rustix::io::fcntl_dupfd_cloexec(ring.memfd(), 3)?,
            rustix::io::fcntl_dupfd_cloexec(data.as_fd(), 3)?,
            space_rd,
        ];
        let (out, latency_us): (Box<dyn Output>, u32) = match &self.backend {
            Backend::Pulse { sink } => match pulse::PulseOut::connect(cfg, sink.as_deref()) {
                Ok(p) => (Box::new(p), pulse::TARGET_LATENCY_US),
                Err(e) => {
                    crate::warn!(
                        "{}: PipeWire unavailable ({e:#}); stream plays nowhere",
                        self.peer
                    );
                    (Box::new(NullOut::new(cfg)?), 0)
                }
            },
            Backend::Null => (Box::new(NullOut::new(cfg)?), 0),
        };
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        let capacity = cfg.capacity_frames();
        let handle = stream::spawn(format!("{}#{id}", self.peer), ring, data, space, out)?;
        if self.master != (1.0, false) {
            handle.send(Ctl::Master(self.master.0, self.master.1));
        }
        self.streams.insert(id, handle);
        crate::info!(
            "{}: output #{id} {} Hz x{} {:?}, {} x {} frames, \"{}\"",
            self.peer,
            cfg.rate,
            cfg.channels,
            cfg.format,
            cfg.periods,
            cfg.period_frames,
            cfg.name
        );
        let body = OpenOutputReply {
            stream: id,
            capacity_frames: capacity,
            data_offset: DATA_OFFSET as u32,
            map_size: map_size as u32,
            latency_us,
        }
        .encode();
        Ok(Reply {
            status: 0,
            body,
            fds: send,
        })
    }

    fn with_stream(&self, id: u32, c: Ctl) -> Reply {
        match self.streams.get(&id) {
            Some(s) => {
                s.send(c);
                Reply::ok()
            }
            None => Reply::err(-ENOENT),
        }
    }
}

impl Drop for AudioService {
    fn drop(&mut self) {
        let ids: Vec<u32> = self.streams.keys().copied().collect();
        for id in ids {
            self.remove(id);
        }
    }
}

impl Service for AudioService {
    fn handle(&mut self, ty: u16, body: &[u8], fds: Vec<OwnedFd>) -> Reply {
        if !fds.is_empty() {
            return Reply::err(-EPROTO); // no audio request carries fds
        }
        let req = match proto::decode(ty, body) {
            Ok(r) => r,
            Err(e) => {
                crate::debug!("{}: bad request {ty:#06x}: {e:?}", self.peer);
                return Reply::err(e.status());
            }
        };
        match req {
            Request::OpenOutput(cfg) => self.open_output(cfg),
            Request::Close { stream } => {
                if self.remove(stream) {
                    crate::info!("{}: output #{stream} closed", self.peer);
                    Reply::ok()
                } else {
                    Reply::err(-ENOENT)
                }
            }
            Request::Standby { stream } => self.with_stream(stream, Ctl::Standby),
            Request::SetVolume {
                stream,
                left,
                right,
            } => self.with_stream(stream, Ctl::Volume(left, right)),
            Request::SetMetadata {
                stream,
                usage,
                label,
            } => self.with_stream(stream, Ctl::Metadata(usage, label)),
            Request::SetMaster { volume, muted } => {
                self.master = (volume, muted);
                for s in self.streams.values() {
                    s.send(Ctl::Master(volume, muted));
                }
                Reply::ok()
            }
        }
    }
}
