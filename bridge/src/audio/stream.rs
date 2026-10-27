//! One thread per output stream: waits on the client's "data" eventfd, the
//! output's own fd (PipeWire socket or a timer) and a control eventfd, moves
//! PCM from the ring to the output, and signals the client's "space"
//! eventfd whenever it consumed frames.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use anyhow::Result;
use rustix::event::{EventfdFlags, PollFd, PollFlags};
use rustix::time::{ClockId, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags};

use super::proto::OutputConfig;
use super::ring::{Ring, STATE_DEAD, STATE_PLAYING};

/// Control messages from the client thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Ctl {
    Volume(f32, f32),
    Master(f32, bool),
    Standby,
    Metadata(u32, String),
}

/// Where a stream's PCM goes.
pub trait Output: Send {
    fn poll_fd(&self) -> BorrowedFd<'_>;
    /// `poll_fd` is readable. Returns whether frames were consumed.
    fn on_readable(&mut self, ring: &mut Ring) -> Result<bool>;
    /// The client produced frames. Returns whether frames were consumed.
    fn on_data(&mut self, ring: &mut Ring) -> Result<bool>;
    fn control(&mut self, ring: &mut Ring, c: Ctl) -> Result<()>;
}

pub fn monotonic_ns() -> u64 {
    let t = rustix::time::clock_gettime(ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Desktop name and PulseAudio `media.role` for an Android `audio_usage_t`.
pub fn usage_name_role(usage: u32) -> (&'static str, &'static str) {
    match usage {
        1 => ("Media", "music"),
        2 | 3 | 17 => ("Voice call", "phone"),
        4 => ("Alarm", "event"),
        5..=10 => ("Notification", "event"),
        11 => ("Accessibility", "a11y"),
        12 => ("Navigation", "event"),
        13 => ("System sounds", "event"),
        14 => ("Game", "game"),
        16 => ("Assistant", "music"),
        _ => ("Android", "music"),
    }
}

/// Consumes at the stream's nominal rate and plays nowhere (no PipeWire, or
/// tests). Timing is exact: presented == consumed.
pub struct NullOut {
    timer: OwnedFd,
    period: u64,
}

impl NullOut {
    pub fn new(cfg: &OutputConfig) -> Result<NullOut> {
        let timer = rustix::time::timerfd_create(
            TimerfdClockId::Monotonic,
            TimerfdFlags::CLOEXEC | TimerfdFlags::NONBLOCK,
        )?;
        let ns = cfg.period_frames as u64 * 1_000_000_000 / cfg.rate as u64;
        let ts = rustix::time::Timespec {
            tv_sec: (ns / 1_000_000_000) as i64,
            tv_nsec: (ns % 1_000_000_000) as i64,
        };
        rustix::time::timerfd_settime(
            &timer,
            TimerfdTimerFlags::empty(),
            &rustix::time::Itimerspec {
                it_interval: ts,
                it_value: ts,
            },
        )?;
        Ok(NullOut {
            timer,
            period: cfg.period_frames as u64,
        })
    }
}

impl Output for NullOut {
    fn poll_fd(&self) -> BorrowedFd<'_> {
        self.timer.as_fd()
    }
    fn on_readable(&mut self, ring: &mut Ring) -> Result<bool> {
        let mut b = [0u8; 8];
        let ticks = match rustix::io::read(&self.timer, &mut b) {
            Ok(8) => u64::from_ne_bytes(b),
            _ => return Ok(false),
        };
        let mut sink =
            vec![0u8; (self.period * ticks).min(ring.capacity()) as usize * ring.frame_bytes()];
        let n = ring.read(&mut sink);
        ring.publish_timing(ring.read_pos(), monotonic_ns(), 0);
        if n > 0 {
            ring.set_state(STATE_PLAYING);
        }
        Ok(n > 0)
    }
    fn on_data(&mut self, _ring: &mut Ring) -> Result<bool> {
        Ok(false)
    }
    fn control(&mut self, _ring: &mut Ring, _c: Ctl) -> Result<()> {
        Ok(())
    }
}

/// The client thread's handle on a stream thread. Dropping it stops the
/// thread and frees the ring.
pub struct StreamHandle {
    tx: Sender<Ctl>,
    wake: Arc<OwnedFd>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
    pub ring_bytes: usize,
}

impl StreamHandle {
    pub fn send(&self, c: Ctl) {
        if self.tx.send(c).is_ok() {
            let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        }
    }
    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(|j| j.is_finished())
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

/// Starts a stream thread. `data` is the eventfd the client signals,
/// `space` the one we signal.
pub fn spawn(
    name: String,
    ring: Ring,
    data: OwnedFd,
    space: OwnedFd,
    out: Box<dyn Output>,
) -> Result<StreamHandle> {
    let wake = Arc::new(rustix::event::eventfd(
        0,
        EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK,
    )?);
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = channel();
    let ring_bytes = ring.map_len();
    let (w, s) = (wake.clone(), stop.clone());
    let join = std::thread::Builder::new()
        .name(format!("stream {name}"))
        .spawn(move || {
            if let Err(e) = run(ring, data, space, w, s, rx, out) {
                crate::warn!("stream {name}: {e:#}");
            }
        })?;
    Ok(StreamHandle {
        tx,
        wake,
        stop,
        join: Some(join),
        ring_bytes,
    })
}

fn drain(fd: &OwnedFd) {
    let mut b = [0u8; 8];
    let _ = rustix::io::read(fd, &mut b);
}

fn run(
    mut ring: Ring,
    data: OwnedFd,
    space: OwnedFd,
    wake: Arc<OwnedFd>,
    stop: Arc<AtomicBool>,
    rx: Receiver<Ctl>,
    mut out: Box<dyn Output>,
) -> Result<()> {
    let res = (|| -> Result<()> {
        loop {
            let (r_out, r_data, r_wake) = {
                let ofd = out.poll_fd();
                let mut fds = [
                    PollFd::new(&ofd, PollFlags::IN),
                    PollFd::new(&data, PollFlags::IN),
                    PollFd::new(&*wake, PollFlags::IN),
                ];
                match rustix::event::poll(&mut fds, None) {
                    Ok(_) | Err(rustix::io::Errno::INTR) => {}
                    Err(e) => return Err(e.into()),
                }
                let ev = |i: usize| !fds[i].revents().is_empty();
                (ev(0), ev(1), ev(2))
            };
            if stop.load(Ordering::Acquire) {
                return Ok(());
            }
            let mut consumed = false;
            if r_wake {
                drain(&wake);
                while let Ok(c) = rx.try_recv() {
                    out.control(&mut ring, c)?;
                }
            }
            if r_data {
                drain(&data);
                consumed |= out.on_data(&mut ring)?;
            }
            if r_out {
                consumed |= out.on_readable(&mut ring)?;
            }
            if consumed {
                // Non-blocking: a client that saturated the counter only
                // loses its own wakeups.
                let _ = rustix::io::write(&space, &1u64.to_ne_bytes());
            }
        }
    })();
    // Tell the client to reconnect, and wake it if it's waiting for space.
    ring.set_state(STATE_DEAD);
    let _ = rustix::io::write(&space, &1u64.to_ne_bytes());
    res
}
