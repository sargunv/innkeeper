//! Translation stays in the system app (host layout, shortcuts, gestures).
//! Eligible translated events return here to the broker's session-local evdev
//! endpoints; this bridge is the only writer of input_event records.
use linuxapk_broker::input::event;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;

pub struct Evdev {
    socket: UnixDatagram,
    path: PathBuf,
}
impl Evdev {
    pub fn new(path: PathBuf) -> std::io::Result<Self> {
        let socket = UnixDatagram::unbound()?;
        socket.set_nonblocking(true)?;
        Ok(Self { socket, path })
    }
    pub fn send(&self, device: u8, micros: i64, events: &[(u16, u16, i32)]) -> std::io::Result<()> {
        let mut b = vec![device];
        for &(ty, code, value) in events {
            b.extend_from_slice(&event(micros, ty, code, value));
        }
        b.extend_from_slice(&event(micros, 0, 0, 0));
        self.socket.send_to(&b, &self.path)?;
        Ok(())
    }
}
