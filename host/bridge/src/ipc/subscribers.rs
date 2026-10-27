//! Fan-out of one kind of event to the connections that subscribed to it.

use std::os::fd::BorrowedFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use super::server::Events;

/// Connections that want one kind of event. A client whose socket is full
/// misses the event; one that is gone is dropped.
#[derive(Default)]
pub struct Subscribers {
    next: AtomicU64,
    list: Mutex<Vec<(u64, Events)>>,
}

impl Subscribers {
    pub fn add(&self, e: Events) -> u64 {
        let k = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.list.lock().unwrap().push((k, e));
        k
    }

    pub fn remove(&self, k: u64) {
        self.list.lock().unwrap().retain(|(i, _)| *i != k);
    }

    pub fn is_empty(&self) -> bool {
        self.list.lock().unwrap().is_empty()
    }

    /// Sends to everyone except `skip` (the connection that caused it).
    pub fn broadcast(&self, ty: u16, body: &[u8], fds: &[BorrowedFd<'_>], skip: Option<u64>) {
        self.list
            .lock()
            .unwrap()
            .retain(|(k, e)| Some(*k) == skip || alive(e.try_send(ty, body, fds)));
    }
}

/// Did an event send leave the connection usable (sent, or only dropped
/// because the client isn't reading)?
fn alive(r: anyhow::Result<()>) -> bool {
    match r {
        Ok(()) => true,
        Err(e) => e.downcast_ref::<rustix::io::Errno>() == Some(&rustix::io::Errno::AGAIN),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;

    /// A subscriber that stops reading can't stall a broadcast (which holds
    /// the subscriber lock), and keeps its subscription; one that is gone is
    /// dropped.
    #[test]
    fn broadcast_never_waits_for_a_slow_client() {
        let (a, b) = rustix::net::socketpair(
            rustix::net::AddressFamily::UNIX,
            rustix::net::SocketType::SEQPACKET,
            rustix::net::SocketFlags::CLOEXEC,
            None,
        )
        .unwrap();
        let e = Events::new(Arc::new(a));
        // Fill the client's socket.
        let full = loop {
            if let Err(x) = e.try_send(0x4001, &[0; 256], &[]) {
                break x;
            }
        };
        assert!(!alive(Err(anyhow::anyhow!("other"))));
        assert!(alive(Err(full)), "filling the socket failed otherwise");
        let subs = Arc::new(Subscribers::default());
        subs.add(e);
        let (tx, rx) = std::sync::mpsc::channel();
        let s = subs.clone();
        std::thread::spawn(move || {
            s.broadcast(0x4001, b"x", &[], None);
            tx.send(()).unwrap();
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("broadcast blocked on a full socket");
        assert!(!subs.is_empty());
        // Once the client is gone, a send fails with something other than
        // EAGAIN (the kernel may free the queued packets a moment later).
        drop(b);
        for _ in 0..100 {
            subs.broadcast(0x4001, b"x", &[], None);
            if subs.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(subs.is_empty());
    }
}
