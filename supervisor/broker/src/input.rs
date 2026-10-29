//! Session-local evdev devices. EventHub polls a socket endpoint installed by
//! ADDFD; its peer receives the bridge's input_event records. Nothing reaches
//! the host input subsystem. The ordinary tmpfs nodes exist for inotify only.
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::sync::{Arc, Mutex};

use crate::notify::{
    SECCOMP_ADDFD_FLAG_SEND, SeccompNotif, SeccompNotifAddfd, mem_write, notif_addfd_cmd,
    notif_id_valid, open_mem, read_path, send_resp,
};

pub const EVENT_SIZE: usize = 24;
pub const KEYBOARD: u8 = 0;
pub const MOUSE: u8 = 1;

/// x86_64 Linux input_event, with a CLOCK_MONOTONIC timeval. Kept explicit:
/// Rust struct padding is not part of the bridge protocol.
pub fn event(micros: i64, ty: u16, code: u16, value: i32) -> [u8; EVENT_SIZE] {
    let mut b = [0; EVENT_SIZE];
    b[..8].copy_from_slice(&(micros / 1_000_000).to_le_bytes());
    b[8..16].copy_from_slice(&(micros % 1_000_000).to_le_bytes());
    b[16..18].copy_from_slice(&ty.to_le_bytes());
    b[18..20].copy_from_slice(&code.to_le_bytes());
    b[20..].copy_from_slice(&value.to_le_bytes());
    b
}

struct Client {
    device: u8,
    peer: OwnedFd,
    /// Keep the installed endpoint alive for its inode identity. A peer with
    /// no Android references is detected by comparing its installed /proc fd.
    endpoint: OwnedFd,
    pid: u32,
    fd: i32,
}
impl Client {
    fn live(&self) -> bool {
        let link = std::fs::read_link(format!("/proc/{}/fd/{}", self.pid, self.fd));
        let ours = std::fs::read_link(format!("/proc/self/fd/{}", self.endpoint.as_raw_fd()));
        link.is_ok() && link.ok() == ours.ok()
    }

    fn deliver(&self, packet: &[u8]) -> std::io::Result<()> {
        while self.live() {
            let n = unsafe {
                libc::send(
                    self.peer.as_raw_fd(),
                    packet.as_ptr().cast(),
                    packet.len(),
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            if n >= 0 {
                // EventHub reads one packet per ready device, in epoll order.
                // Wait for this batch to be read before making another device
                // readable, preserving modifier/mouse order through InputReader.
                while self.live() {
                    let mut queued = 0i32;
                    if unsafe { libc::ioctl(self.peer.as_raw_fd(), libc::TIOCOUTQ, &mut queued) }
                        < 0
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                    if queued == 0 {
                        break;
                    }
                    // Writable does not mean consumed on a Unix socket.
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
                return Ok(());
            } // SOCK_SEQPACKET preserves the whole record batch.
            let e = std::io::Error::last_os_error();
            match e.kind() {
                std::io::ErrorKind::Interrupted => continue,
                std::io::ErrorKind::WouldBlock => {
                    let mut poll = libc::pollfd {
                        fd: self.peer.as_raw_fd(),
                        events: libc::POLLOUT,
                        revents: 0,
                    };
                    // Recheck the installed fd when a reader closes without
                    // draining: we retain its endpoint for ioctl identity.
                    let n = unsafe { libc::poll(&mut poll, 1, 100) };
                    if n < 0
                        && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted
                    {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                _ => return Err(e),
            }
        }
        Ok(()) // The Android reader closed this device.
    }
}
struct Devices {
    clients: Vec<Arc<Client>>,
    keys: [[u8; 96]; 2],
    leds: [u8; 8],
}

pub(crate) struct Input {
    listener: RawFd,
    resp_sz: usize,
    devices: Arc<Mutex<Devices>>,
}

fn bitmap(size: usize, bits: impl IntoIterator<Item = usize>) -> Vec<u8> {
    let mut b = vec![0; size];
    for bit in bits {
        if bit / 8 < b.len() {
            b[bit / 8] |= 1 << (bit % 8);
        }
    }
    b
}

/// EVIOCG* answers, including the caller's requested bitmap/string length.
/// Return value is bytes copied for variable-length queries, zero for fixed
/// structs, as evdev does. Unknown ioctls stay ENOTTY.
fn answer(device: u8, cmd: u32, keys: &[u8], leds: &[u8]) -> Result<(Vec<u8>, i64), i32> {
    let nr = cmd & 0xff;
    let size = ((cmd >> 16) & 0x3fff) as usize;
    let read = cmd >> 30 & 2 != 0;
    let mut variable = false;
    let mut b = match (read, nr) {
        (true, 0x01) => 0x010001i32.to_le_bytes().to_vec(), // EV_VERSION
        (true, 0x02) => [6u16, 0x1209, 0x4c00 + device as u16, 1]
            .into_iter()
            .flat_map(u16::to_le_bytes)
            .collect(),
        (true, 0x03) => [0i32, 0].into_iter().flat_map(i32::to_le_bytes).collect(),
        (true, 0x06..=0x08) => {
            variable = true;
            let s = match nr {
                0x06 => {
                    if device == KEYBOARD {
                        "Linux.apk Keyboard"
                    } else {
                        "Linux.apk Mouse"
                    }
                }
                0x07 => {
                    if device == KEYBOARD {
                        "linuxapk/keyboard"
                    } else {
                        "linuxapk/mouse"
                    }
                }
                _ => {
                    if device == KEYBOARD {
                        "linuxapk-keyboard"
                    } else {
                        "linuxapk-mouse"
                    }
                }
            };
            [s.as_bytes(), &[0]].concat()
        }
        (true, 0x09) => {
            variable = true;
            vec![0; size]
        } // EVIOCGPROP
        (true, 0x18) => {
            variable = true;
            keys.to_vec()
        }
        (true, 0x19) => {
            variable = true;
            leds.to_vec()
        }
        (true, 0x1b) => {
            variable = true;
            vec![0; size]
        } // EVIOCGSW
        (true, 0x20..=0x3f) => {
            variable = true;
            match nr - 0x20 {
                0 => bitmap(
                    size,
                    if device == KEYBOARD {
                        vec![0, 1, 17]
                    } else {
                        vec![0, 1, 2]
                    },
                ),
                1 => bitmap(
                    size,
                    if device == KEYBOARD {
                        (1..=248).chain([464]).collect::<Vec<_>>()
                    } else {
                        (272..=279).collect()
                    },
                ),
                2 if device == MOUSE => bitmap(size, [0, 1, 6, 8, 11, 12]),
                17 if device == KEYBOARD => bitmap(size, [0, 1, 2]),
                _ => vec![0; size],
            }
        }
        (false, 0x03 | 0x90 | 0xa0) => vec![], // repeat, grab, monotonic clock
        _ => return Err(libc::ENOTTY),
    };
    if variable {
        b.resize(size.min(b.len()), 0);
        let n = b.len() as i64;
        Ok((b, n))
    } else {
        Ok((b, 0))
    }
}

impl Default for Devices {
    fn default() -> Self {
        Self {
            clients: Vec::new(),
            keys: [[0; 96]; 2],
            leds: [0; 8],
        }
    }
}

impl Input {
    pub(crate) fn new(listener: RawFd, resp_sz: usize) -> Option<Self> {
        if std::env::var("LINUXAPK_BIONIC_HARNESS").as_deref() == Ok("1") {
            return None;
        }
        let dir = std::path::Path::new("/dev/input");
        std::fs::create_dir_all(dir).ok()?;
        let path = "/dev/socket/linuxapk/evdev.sock";
        let _ = std::fs::remove_file(path);
        let socket = UnixDatagram::bind(path).ok()?;
        let devices = Arc::new(Mutex::new(Devices::default()));
        let state = Arc::clone(&devices);
        std::thread::spawn(move || {
            let mut buf = [0; 1 + 64 * EVENT_SIZE];
            loop {
                let Ok(n) = socket.recv(&mut buf) else { return };
                if n < 1 || buf[0] > MOUSE || (n - 1) % EVENT_SIZE != 0 {
                    continue;
                }
                let device = buf[0];
                let mut s = state.lock().unwrap();
                for ev in buf[1..n].as_chunks::<EVENT_SIZE>().0 {
                    let ty = u16::from_le_bytes(ev[16..18].try_into().unwrap());
                    let code = u16::from_le_bytes(ev[18..20].try_into().unwrap()) as usize;
                    let value = i32::from_le_bytes(ev[20..24].try_into().unwrap());
                    if ty == 1 && code < 768 {
                        let b = &mut s.keys[device as usize][code / 8];
                        if value == 0 {
                            *b &= !(1 << (code % 8));
                        } else {
                            *b |= 1 << (code % 8);
                        }
                    }
                }
                let mut leds = s.leds;
                s.clients.retain_mut(|c| {
                    if !c.live() {
                        return false;
                    }
                    // Drain EV_LED writes. No host LEDs are changed.
                    let mut led = [0u8; EVENT_SIZE * 8];
                    loop {
                        let n = unsafe {
                            libc::recv(
                                c.peer.as_raw_fd(),
                                led.as_mut_ptr().cast(),
                                led.len(),
                                libc::MSG_DONTWAIT,
                            )
                        };
                        if n <= 0 {
                            break;
                        }
                        for ev in led[..n as usize].as_chunks::<EVENT_SIZE>().0 {
                            let ty = u16::from_le_bytes(ev[16..18].try_into().unwrap());
                            let code = u16::from_le_bytes(ev[18..20].try_into().unwrap()) as usize;
                            let value = i32::from_le_bytes(ev[20..24].try_into().unwrap());
                            if ty == 17 && code < 64 {
                                if value == 0 {
                                    leds[code / 8] &= !(1 << (code % 8));
                                } else {
                                    leds[code / 8] |= 1 << (code % 8);
                                }
                            }
                        }
                    }
                    true
                });
                s.leds = leds;
                let clients: Vec<_> = s
                    .clients
                    .iter()
                    .filter(|c| c.device == device)
                    .cloned()
                    .collect();
                drop(s);
                // Do not hold device state while waiting: EventHub may need an
                // ioctl answer before it can drain its socket.
                for client in clients {
                    if let Err(e) = client.deliver(&buf[1..n]) {
                        eprintln!("evdev: delivery failed: {e}");
                    }
                }
            }
        });
        // Create after the input socket is ready; EventHub observes these via
        // its initial scan or inotify. /dev is the sandbox's private tmpfs.
        for n in 0..2 {
            std::fs::write(dir.join(format!("event{n}")), []).ok()?;
        }
        Some(Self {
            listener,
            resp_sz,
            devices,
        })
    }

    pub(crate) fn serve(&self, req: &SeccompNotif) -> bool {
        let device = if req.data.nr as i64 == libc::SYS_openat {
            match read_path(req.pid as i32, req.data.args[1]).as_deref() {
                Some(b"/dev/input/event0") => KEYBOARD,
                Some(b"/dev/input/event1") => MOUSE,
                _ => return false,
            }
        } else if req.data.nr as i64 == libc::SYS_ioctl
            && req.data.args[1] as u32 & 0xff00 == 0x4500
        {
            let path = format!("/proc/{}/fd/{}", req.pid, req.data.args[0]);
            let Ok(link) = std::fs::read_link(path) else {
                return false;
            };
            let state = self.devices.lock().unwrap();
            let Some(c) = state.clients.iter().find(|c| {
                std::fs::read_link(format!("/proc/self/fd/{}", c.endpoint.as_raw_fd()))
                    .ok()
                    .as_ref()
                    == Some(&link)
            }) else {
                return false;
            };
            let Some(mem) = open_mem(req.pid as i32) else {
                send_resp(self.listener, self.resp_sz, req.id, 0, -libc::EFAULT, false);
                return true;
            };
            if !notif_id_valid(self.listener, req.id) {
                return true;
            }
            let result = answer(
                c.device,
                req.data.args[1] as u32,
                &state.keys[c.device as usize],
                &state.leds,
            );
            match result {
                Ok((bytes, val)) if mem_write(mem.raw(), req.data.args[2], &bytes) => {
                    send_resp(self.listener, self.resp_sz, req.id, val, 0, false)
                }
                Ok(_) => send_resp(self.listener, self.resp_sz, req.id, 0, -libc::EFAULT, false),
                Err(e) => send_resp(self.listener, self.resp_sz, req.id, 0, -e, false),
            };
            return true;
        } else {
            return false;
        };
        let mut fds = [-1; 2];
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
                fds.as_mut_ptr(),
            )
        } != 0
        {
            send_resp(self.listener, self.resp_sz, req.id, 0, -libc::ENOMEM, false);
            return true;
        }
        let endpoint = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let peer = unsafe { OwnedFd::from_raw_fd(fds[1]) };
        let add = SeccompNotifAddfd {
            id: req.id,
            flags: SECCOMP_ADDFD_FLAG_SEND,
            srcfd: endpoint.as_raw_fd() as u32,
            newfd: 0,
            newfd_flags: req.data.args[2] as u32 & libc::O_CLOEXEC as u32,
        };
        let fd = unsafe {
            libc::ioctl(
                self.listener,
                notif_addfd_cmd(size_of::<SeccompNotifAddfd>()) as _,
                &add,
            )
        };
        if fd >= 0 {
            self.devices.lock().unwrap().clients.push(Arc::new(Client {
                device,
                peer,
                endpoint,
                pid: req.pid,
                fd,
            }));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(device: u8) -> (Client, OwnedFd) {
        let mut fds = [-1; 2];
        assert_eq!(
            unsafe {
                libc::socketpair(
                    libc::AF_UNIX,
                    libc::SOCK_SEQPACKET | libc::SOCK_NONBLOCK,
                    0,
                    fds.as_mut_ptr(),
                )
            },
            0
        );
        let reader = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let client = Client {
            device,
            peer: unsafe { OwnedFd::from_raw_fd(fds[1]) },
            endpoint: reader.try_clone().unwrap(),
            pid: std::process::id(),
            fd: reader.as_raw_fd(),
        };
        (client, reader)
    }

    fn receive(fd: &OwnedFd) -> Vec<u8> {
        let mut poll = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 1000) }, 1);
        let mut packet = vec![0; EVENT_SIZE];
        assert_eq!(
            unsafe { libc::recv(fd.as_raw_fd(), packet.as_mut_ptr().cast(), packet.len(), 0) },
            EVENT_SIZE as isize
        );
        packet
    }

    #[test]
    fn modifier_release_is_consumed_before_mouse_report() {
        let (keyboard, keys) = client(KEYBOARD);
        let (mouse, motion) = client(MOUSE);
        let (done, completed) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            keyboard.deliver(&event(1, 1, 42, 1)).unwrap();
            keyboard.deliver(&event(2, 1, 42, 0)).unwrap();
            mouse.deliver(&event(3, 2, 0, 10)).unwrap();
            done.send(()).unwrap();
        });
        assert!(
            completed
                .recv_timeout(std::time::Duration::from_millis(20))
                .is_err()
        );
        assert_eq!(receive(&keys), event(1, 1, 42, 1));
        // The queued release must be read before mouse delivery can begin.
        let mut poll = libc::pollfd {
            fd: motion.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(unsafe { libc::poll(&mut poll, 1, 20) }, 0);
        assert_eq!(receive(&keys), event(2, 1, 42, 0));
        assert_eq!(receive(&motion), event(3, 2, 0, 10));
        completed
            .recv_timeout(std::time::Duration::from_secs(1))
            .unwrap();
        worker.join().unwrap();
    }
    fn get(nr: u32, size: u32) -> u32 {
        0x80004500 | nr | size << 16
    }
    #[test]
    fn eventhub_identifies_keyboard_and_mouse() {
        assert_eq!(
            answer(KEYBOARD, get(2, 8), &[], &[]).unwrap().0,
            [6, 0, 9, 18, 0, 76, 1, 0]
        );
        assert_eq!(
            answer(KEYBOARD, get(6, 8), &[], &[]).unwrap(),
            (b"Linux.ap".to_vec(), 8)
        );
        let keys = answer(KEYBOARD, get(0x21, 96), &[], &[]).unwrap().0;
        assert_ne!(keys[30 / 8] & (1 << (30 % 8)), 0); // KEY_A: alphabetic keyboard
        assert_eq!(keys[272 / 8] & 1, 0);
        let rel = answer(MOUSE, get(0x22, 8), &[], &[]).unwrap().0;
        assert_eq!(rel[0] & 3, 3); // REL_X/Y: cursor device
        assert_ne!(rel[11 / 8] & (1 << (11 % 8)), 0); // high-resolution wheel
        assert!(answer(MOUSE, get(0x40, 24), &[], &[]).is_err());
    }
    #[test]
    fn records_have_the_kernel_abi_and_sync_boundary() {
        let b = event(12_345_678, 1, 30, 1);
        assert_eq!(&b[..8], &12i64.to_le_bytes());
        assert_eq!(&b[8..16], &345_678i64.to_le_bytes());
        assert_eq!(&b[16..], &[1, 0, 30, 0, 1, 0, 0, 0]);
        assert_eq!(&event(12_345_678, 0, 0, 0)[16..], &[0; 8]);
    }
}
