//! Each PCM stream has its own thread. The thread waits on the client's
//! "data" eventfd, the output's fd (the PipeWire socket or a timer) and a
//! control eventfd. It moves PCM between the ring and the output, and it
//! signals the client's "space" pipe whenever frames were consumed. The
//! client cannot use either of these two fds to block the thread. The data
//! eventfd is read with RWF_NOWAIT, and only the bridge holds the space
//! pipe's write end.

use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use anyhow::Result;
use rustix::event::{EventfdFlags, PollFd, PollFlags};
use rustix::time::{ClockId, TimerfdClockId, TimerfdFlags, TimerfdTimerFlags};

use super::proto::StreamConfig;
use super::ring::{Ring, STATE_DEAD, STATE_PLAYING};

/// Control messages from the client thread.
#[derive(Debug, Clone, PartialEq)]
pub enum Ctl {
    Volume(f32, f32),
    Standby,
    Metadata(u32, String),
    /// The app on Android's mixed playback stream, or `None` to restore the
    /// stream's default application name.
    App(Option<PlaybackApp>),
}

/// What the desktop mixer shows for a playback stream. `icon_name` is the
/// app's desktop entry id, `<Flatpak app id>.<package>`, which is also the
/// name of its `.desktop` file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackApp {
    pub name: String,
    pub icon_name: String,
    pub id: String,
    pub binary: String,
}

/// The application name a playback stream is opened with, restored when no
/// app is playing.
pub const DEFAULT_APP_NAME: &str = "Innkeeper";

/// The `application.id` and `application.icon_name` set on the Pulse client
/// before a playback stream opens. `SetClientName` does not set
/// `application.process.binary`, so that key exists on the stream only while
/// an app is playing.
pub const DEFAULT_APP_ID: &str = "dev.sargunv.innkeeper";
pub const DEFAULT_APP_ICON: &str = "android";

/// Pulse application properties for the mixer. For `Some`, `application.name`
/// is the app's label and the other three keys hold its desktop id. `None`
/// writes the name, icon and id the Pulse client was opened with. It sets
/// `application.process.binary` to empty, because the open does not set that
/// key and PipeWire drops a property that a `Replace` sets to empty. An empty
/// `application.name` would blank the mixer, so that key gets the default.
pub fn playback_app_props(app: Option<&PlaybackApp>) -> Vec<(&'static str, &str)> {
    match app {
        Some(app) => vec![
            ("application.name", &app.name),
            ("application.icon_name", &app.icon_name),
            ("application.id", &app.id),
            ("application.process.binary", &app.binary),
        ],
        None => vec![
            ("application.name", DEFAULT_APP_NAME),
            ("application.icon_name", DEFAULT_APP_ICON),
            ("application.id", DEFAULT_APP_ID),
            ("application.process.binary", ""),
        ],
    }
}

/// Moves PCM between a shared ring and the desktop, in either direction.
pub trait StreamIo: Send {
    fn poll_fd(&self) -> BorrowedFd<'_>;
    /// Called when `poll_fd` is readable. Returns whether frames were consumed.
    fn on_readable(&mut self, ring: &mut Ring) -> Result<bool>;
    /// Called when the client signals the data eventfd. Returns whether frames
    /// were consumed.
    fn on_data(&mut self, ring: &mut Ring) -> Result<bool>;
    fn control(&mut self, ring: &mut Ring, c: Ctl) -> Result<()>;
}

pub fn monotonic_ns() -> u64 {
    let t = rustix::time::clock_gettime(ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Desktop name and PulseAudio `media.role` for an Android `audio_usage_t`.
///
/// The playback HAL reports the highest-priority active usage. An empty
/// label uses this name. Record streams ignore the request.
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

/// An output that consumes frames at the stream's nominal rate and discards
/// them. It is used when PipeWire is unavailable and in tests. The presented
/// position it reports always equals the consumed position.
pub struct NullOut {
    timer: OwnedFd,
    period: u64,
}

impl NullOut {
    pub fn new(cfg: &StreamConfig) -> Result<NullOut> {
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

impl StreamIo for NullOut {
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
    /// A copy of the control channel. Dropping it does not stop the stream,
    /// so a caller can send without holding the connection's stream map.
    pub fn sender(&self) -> StreamSender {
        StreamSender {
            tx: self.tx.clone(),
            wake: self.wake.clone(),
        }
    }

    pub fn send(&self, c: Ctl) {
        self.sender().send(c);
    }

    pub fn is_finished(&self) -> bool {
        self.join.as_ref().is_none_or(|j| j.is_finished())
    }
}

/// Sends control messages to a stream thread. Unlike [`StreamHandle`],
/// dropping this leaves the stream thread running.
pub struct StreamSender {
    tx: Sender<Ctl>,
    wake: Arc<OwnedFd>,
}

impl StreamSender {
    pub fn send(&self, c: Ctl) {
        if self.tx.send(c).is_ok() {
            let _ = rustix::io::write(&*self.wake, &1u64.to_ne_bytes());
        }
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

/// Starts a stream thread. `data` is the eventfd the client signals, and
/// `space` is the write end of the pipe the bridge signals.
pub fn spawn(
    name: String,
    ring: Ring,
    data: OwnedFd,
    space: OwnedFd,
    out: Box<dyn StreamIo>,
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

/// Clears an eventfd without waiting, even if a peer that shares it cleared
/// O_NONBLOCK and drained it first.
fn drain(fd: &OwnedFd) {
    let mut b = [0u8; 8];
    let mut io = [std::io::IoSliceMut::new(&mut b)];
    let _ = rustix::io::preadv2(fd, &mut io, u64::MAX, rustix::io::ReadWriteFlags::NOWAIT);
}

/// Makes the space pipe readable unless it already is. A client that does not
/// read gets one pending byte instead of a backlog of stale wakeups.
fn signal(space: &OwnedFd) {
    if rustix::io::ioctl_fionread(space).unwrap_or(0) == 0 {
        let _ = rustix::io::write(space, &[1]);
    }
}

fn run(
    mut ring: Ring,
    data: OwnedFd,
    space: OwnedFd,
    wake: Arc<OwnedFd>,
    stop: Arc<AtomicBool>,
    rx: Receiver<Ctl>,
    mut out: Box<dyn StreamIo>,
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
                signal(&space);
            }
        }
    })();
    // Tell the client to reconnect, and wake it if it's waiting for space.
    ring.set_state(STATE_DEAD);
    signal(&space);
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client sharing the data eventfd can clear O_NONBLOCK and drain it
    /// between our poll and our read. The read must not wait.
    #[test]
    fn drain_never_waits() {
        let fd = rustix::event::eventfd(0, EventfdFlags::CLOEXEC).unwrap();
        drain(&fd);
    }

    #[test]
    fn playback_app_properties() {
        let app = PlaybackApp {
            name: "Example Player".into(),
            icon_name: "dev.sargunv.innkeeper.org.example.player".into(),
            id: "dev.sargunv.innkeeper.org.example.player".into(),
            binary: "dev.sargunv.innkeeper.org.example.player".into(),
        };
        let props = playback_app_props(Some(&app));
        assert_eq!(props[0], ("application.name", "Example Player"));
        assert_eq!(
            props[1],
            (
                "application.icon_name",
                "dev.sargunv.innkeeper.org.example.player"
            )
        );
        assert_eq!(props[2].0, "application.id");
        assert_eq!(props[3].0, "application.process.binary");
        assert_eq!(props[2].1, props[1].1);
        assert_eq!(props[3].1, props[1].1);
        // None writes back the values the stream was opened with. An empty
        // name would blank the mixer.
        let restored = playback_app_props(None);
        assert_eq!(
            restored,
            vec![
                ("application.name", DEFAULT_APP_NAME),
                ("application.icon_name", DEFAULT_APP_ICON),
                ("application.id", DEFAULT_APP_ID),
                // An empty value makes PipeWire drop this key, which the open
                // never set.
                ("application.process.binary", ""),
            ]
        );
        assert_ne!(restored[0].1, "");
    }
}
