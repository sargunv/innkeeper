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
        Ok(Self { socket, path })
    }
    pub fn send(&self, device: u8, micros: i64, events: &[(u16, u16, i32)]) -> std::io::Result<()> {
        let mut b = vec![device];
        for &(ty, code, value) in events {
            b.extend_from_slice(&event(micros, ty, code, value));
        }
        b.extend_from_slice(&event(micros, 0, 0, 0));
        // The kernel's bounded receive queue supplies backpressure. Retry
        // interruptions, and wait for room rather than dropping transitions.
        loop {
            match self.socket.send_to(&b, &self.path) {
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                result => {
                    result?;
                    break;
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn a_release_waits_for_receive_queue_space() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir
            .canonicalize()
            .unwrap()
            .join(format!("evdev-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let receiver = UnixDatagram::bind(&path).unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let writer = Evdev::new(path.clone()).unwrap();
        writer.socket.set_nonblocking(true).unwrap();
        let mut count = 0;
        loop {
            match writer.socket.send_to(b"full", &path) {
                Ok(_) => count += 1,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) => panic!("{e}"),
            }
        }
        writer.socket.set_nonblocking(false).unwrap();
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let send = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            done_tx.send(writer.send(1, 123, &[(1, 272, 0)])).unwrap();
        });
        started_rx.recv().unwrap();
        assert!(matches!(
            done_rx.recv_timeout(Duration::from_millis(50)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ));
        let mut packet = [0u8; 128];
        for _ in 0..count {
            let n = receiver.recv(&mut packet).unwrap();
            assert_eq!(&packet[..n], b"full");
        }
        let n = receiver.recv(&mut packet).unwrap();
        let expected = [&[1][..], &event(123, 1, 272, 0), &event(123, 0, 0, 0)].concat();
        assert_eq!(&packet[..n], expected);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        send.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
