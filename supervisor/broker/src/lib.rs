//! linuxapk-broker: Android's binder driver, reimplemented in userspace, plus
//! the virtual credentials of single-uid sessions.
//!
//!   * [`binder`]: the driver's state machine (`drivers/android/binder.c`) and
//!     its UAPI. It makes no syscalls: process memory, receive buffers and fds
//!     go through the [`binder::driver::Mem`] trait, so it is unit tested and
//!     fuzzed on the host against [`testmem`].
//!   * [`notify`]: the broker's serve loop. It receives the session's seccomp
//!     user notifications and drives the binder driver against real Android
//!     processes.
//!   * `creds`: single-uid mode's credential model and the syscalls it
//!     answers.
//!
//! `LINUXAPK_BROKER_DEBUG` turns on tracing (see [`init_log`]). See
//! `docs/ubinder.md` and `docs/sandbox.md`.

pub mod binder;
mod creds;
pub mod notify;

use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Verbose tracing (LINUXAPK_BROKER_DEBUG).
pub static DEBUG: AtomicBool = AtomicBool::new(false);

/// Bounded trace sink: stderr, or a file that is rotated (to `<path>.1`) every
/// 64 MiB so a long boot can never fill the disk.
struct LogSink {
    path: Option<String>,
    file: Option<std::fs::File>,
    written: u64,
}
static LOG: Mutex<LogSink> = Mutex::new(LogSink {
    path: None,
    file: None,
    written: 0,
});
const LOG_ROTATE: u64 = 64 << 20;

/// Configure tracing from LINUXAPK_BROKER_DEBUG: unset, empty or "0" = off,
/// "1" = stderr, anything else = a log file path. Tracing covers every binder
/// call and, in single-uid mode, a line of syscall counters every 10 s.
pub fn init_log() {
    let Some(v) = std::env::var_os("LINUXAPK_BROKER_DEBUG") else {
        return;
    };
    let v = v.to_string_lossy().into_owned();
    if v.is_empty() || v == "0" {
        return;
    }
    DEBUG.store(true, Ordering::Relaxed);
    if v != "1" {
        LOG.lock().unwrap().path = Some(v);
    }
}

pub fn log(args: std::fmt::Arguments) {
    let mut l = LOG.lock().unwrap();
    let Some(path) = l.path.clone() else {
        eprintln!("{args}");
        return;
    };
    if l.file.is_none() || l.written > LOG_ROTATE {
        if l.file.take().is_some() {
            let _ = std::fs::rename(&path, format!("{path}.1"));
        }
        l.file = std::fs::File::create(&path).ok();
        l.written = 0;
    }
    let line = format!("{args}\n");
    l.written += line.len() as u64;
    if let Some(f) = l.file.as_mut() {
        let _ = f.write_all(line.as_bytes());
    }
}

/// A deterministic in-memory [`Mem`] used by tests and the fuzzer. It models
/// each process's address space as a sparse map and its receive region as a
/// bump allocator, so transaction translation can be exercised end to end
/// without touching the kernel.
pub mod testmem {
    use std::collections::HashMap;

    use crate::binder::driver::{Mem, ProcId};

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
        fn recv_size(&mut self, proc: ProcId) -> usize {
            if self.procs.contains_key(&proc) {
                1 << 20
            } else {
                0
            }
        }
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
