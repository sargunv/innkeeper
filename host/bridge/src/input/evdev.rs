//! Translation stays in the system app (host layout, shortcuts, gestures).
//! Eligible translated events return here to the broker's session-local evdev
//! endpoints; this bridge is the only writer of input_event records.
use innkeeper_evdev_proto::{Message, event};
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
    pub fn send(
        &self,
        device: u32,
        micros: i64,
        events: &[(u16, u16, i32)],
    ) -> std::io::Result<()> {
        let mut records =
            Vec::with_capacity((events.len() + 1) * innkeeper_evdev_proto::EVENT_SIZE);
        for &(ty, code, value) in events {
            records.extend_from_slice(&event(micros, ty, code, value));
        }
        records.extend_from_slice(&event(micros, 0, 0, 0));
        self.packet(
            &Message::Events {
                device,
                records: &records,
            }
            .encode(),
        )
    }

    pub fn device(&self, device: u32, enabled: bool) -> std::io::Result<()> {
        self.packet(&Message::Device { device, enabled }.encode())
    }

    pub fn gamepad(
        &self,
        device: u32,
        identity: &str,
        feedback: Option<&std::os::fd::OwnedFd>,
        synthetic: bool,
    ) -> std::io::Result<()> {
        use std::os::fd::AsRawFd;
        let mut b = Message::Gamepad {
            device,
            identity,
            synthetic,
        }
        .encode();
        // Connect once for sendmsg's ancillary fd; normal records still use
        // send_to on the same destination.
        self.socket.connect(&self.path)?;
        let mut iov = libc::iovec {
            iov_base: b.as_mut_ptr().cast(),
            iov_len: b.len(),
        };
        let mut control = [0usize; 4];
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_iov = &mut iov;
        msg.msg_iovlen = 1;
        if let Some(fd) = feedback {
            msg.msg_control = control.as_mut_ptr().cast();
            msg.msg_controllen = unsafe { libc::CMSG_SPACE(size_of::<i32>() as _) } as usize;
            unsafe {
                let c = libc::CMSG_FIRSTHDR(&msg);
                (*c).cmsg_level = libc::SOL_SOCKET;
                (*c).cmsg_type = libc::SCM_RIGHTS;
                (*c).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as _) as usize;
                std::ptr::write_unaligned(libc::CMSG_DATA(c).cast::<i32>(), fd.as_raw_fd());
            }
        }
        let n = unsafe { libc::sendmsg(self.socket.as_raw_fd(), &msg, 0) };
        if n < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    fn packet(&self, b: &[u8]) -> std::io::Result<()> {
        // The kernel's bounded receive queue supplies backpressure. Retry
        // interruptions, and wait for room rather than dropping transitions.
        loop {
            match self.socket.send_to(b, &self.path) {
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
        let expected = [
            &1u32.to_le_bytes()[..],
            &event(123, 1, 272, 0),
            &event(123, 0, 0, 0),
        ]
        .concat();
        assert_eq!(&packet[..n], expected);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        send.join().unwrap();
        std::fs::remove_file(path).unwrap();
    }
}
