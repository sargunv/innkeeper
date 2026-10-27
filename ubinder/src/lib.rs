//! ubinder: a userspace reimplementation of the Android binder driver.
//!
//! The crate has two layers:
//!   * [`broker`] — the pure driver state machine. No syscalls; all process
//!     access goes through the [`broker::Mem`] trait, so it is fully unit
//!     tested and fuzzed on the host.
//!   * `sys` (feature-gated, Linux only) — the seccomp user-notification
//!     binding that drives the broker against real Android processes in a
//!     Linux.apk session, plus the C ABI the session supervisor links against.
//!
//! See `docs/ubinder.md`.

pub mod broker;
pub mod defs;
#[cfg(target_os = "linux")]
pub mod sys;

pub use broker::{Broker, Mem, ProcId, Wakeup, WriteRead};

/// A deterministic in-memory [`Mem`] used by tests and the fuzzer. It models
/// each process's address space as a sparse map and its receive region as a
/// bump allocator, so transaction translation can be exercised end to end
/// without touching the kernel.
pub mod testmem {
    use std::collections::HashMap;

    use crate::broker::{Mem, ProcId};

    #[derive(Default)]
    pub struct FakeProc {
        pub mem: HashMap<u64, u8>,
        pub recv_base: u64,
        pub recv_next: u64,
        pub freed: Vec<u64>,
        pub next_fd: u32,
        pub installed_fds: Vec<u32>,
    }

    #[derive(Default)]
    pub struct FakeMem {
        pub procs: HashMap<ProcId, FakeProc>,
        /// captured-but-not-installed fd tokens -> (source proc, source fd)
        pub captured: HashMap<u64, (ProcId, u32)>,
        pub next_token: u64,
    }

    impl FakeMem {
        pub fn add_proc(&mut self, id: ProcId, recv_base: u64) {
            self.procs.insert(
                id,
                FakeProc {
                    recv_base,
                    recv_next: recv_base,
                    next_fd: 100,
                    ..Default::default()
                },
            );
        }
        pub fn poke(&mut self, id: ProcId, addr: u64, bytes: &[u8]) {
            let p = self.procs.get_mut(&id).unwrap();
            for (i, b) in bytes.iter().enumerate() {
                p.mem.insert(addr + i as u64, *b);
            }
        }
        pub fn peek(&self, id: ProcId, addr: u64, len: usize) -> Vec<u8> {
            let p = &self.procs[&id];
            (0..len as u64)
                .map(|i| *p.mem.get(&(addr + i)).unwrap_or(&0))
                .collect()
        }
    }

    impl Mem for FakeMem {
        fn read(&mut self, proc: ProcId, addr: u64, len: usize) -> Option<Vec<u8>> {
            let p = self.procs.get(&proc)?;
            Some(
                (0..len as u64)
                    .map(|i| *p.mem.get(&(addr + i)).unwrap_or(&0))
                    .collect(),
            )
        }
        fn alloc_recv(&mut self, proc: ProcId, size: usize, _is_async: bool) -> Option<u64> {
            let p = self.procs.get_mut(&proc)?;
            let a = p.recv_next;
            p.recv_next += ((size + 7) & !7) as u64;
            Some(a)
        }
        fn write_recv(&mut self, proc: ProcId, addr: u64, data: &[u8]) {
            self.poke(proc, addr, data);
        }
        fn free_recv(&mut self, proc: ProcId, addr: u64) {
            if let Some(p) = self.procs.get_mut(&proc) {
                p.freed.push(addr);
            }
        }
        fn capture_fd(&mut self, from: ProcId, from_fd: u32) -> Option<u64> {
            self.procs.get(&from)?;
            self.next_token += 1;
            let tok = self.next_token;
            self.captured.insert(tok, (from, from_fd));
            Some(tok)
        }
        fn install_fd(&mut self, to: ProcId, token: u64) -> Option<u32> {
            // Token must have been captured and not yet dropped.
            self.captured.get(&token)?;
            let p = self.procs.get_mut(&to)?;
            let fd = p.next_fd;
            p.next_fd += 1;
            p.installed_fds.push(fd);
            Some(fd)
        }
        fn drop_fd(&mut self, token: u64) {
            self.captured.remove(&token);
        }
    }
}

#[cfg(test)]
mod tests;
