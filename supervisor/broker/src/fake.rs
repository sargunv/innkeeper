//! A deterministic in-memory [`Userspace`] for the tests and the fuzzer.
//!
//! Each process's address space is a sparse page map and its receive region a
//! bump allocator, so transactions are translated end to end without the
//! kernel. Ioctls are issued the way libbinder issues them: each
//! `BINDER_WRITE_READ` gets its bwr, write buffer and read buffer in the
//! calling process's memory, and its answer is recorded with what the
//! driver copied out. Any fallible operation can be made to fail through
//! [`FakeUserspace::fault`], and a call's wait can be interrupted like a
//! signal does, so every rollback path is reachable.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

use crate::binder::driver::{BinderDriver, Call, InstallError, ProcId, Userspace};
use crate::binder::uapi::{BINDER_WRITE_READ, BinderWriteRead, pod_bytes, read_pod};

const PAGE: u64 = 4096;
/// Where calls' bwrs and buffers live: one `CALL_SPAN` per call.
const SCRATCH: u64 = 0x7000_0000_0000;
const CALL_SPAN: u64 = 0x2_0000;
const WRITE_AT: u64 = 0x100;
const READ_AT: u64 = 0x1_0000;

/// SplitMix64: a tiny deterministic PRNG, so a failing run is reproducible
/// from its seed with no external crates.
pub struct Rng(pub u64);

impl Rng {
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next_u64() % n.max(1)
    }
    pub fn byte(&mut self) -> u8 {
        (self.next_u64() & 0xff) as u8
    }
    pub fn chance(&mut self, pct: u64) -> bool {
        self.below(100) < pct
    }
}

/// A fallible operation that [`FakeUserspace::fault`] can fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Reading user memory (`EFAULT`).
    CopyFromUser,
    /// Writing user memory (`EFAULT`).
    CopyToUser,
    /// Allocating in a receive region (`ENOSPC`).
    AllocRecv,
    /// Capturing a sender's fd (`EBADF`).
    CaptureFd,
    /// Installing an fd into a reader (its fd table is full).
    InstallFd,
    /// A signal ends the call's wait just before it is answered or an fd is
    /// installed for it.
    Signal,
}

#[derive(Default)]
pub struct FakeProc {
    pages: HashMap<u64, Box<[u8; PAGE as usize]>>,
    pub recv_base: u64,
    recv_next: u64,
    /// Receive allocations not freed yet.
    pub live_recv: BTreeSet<u64>,
    next_fd: u32,
    pub installed_fds: Vec<u32>,
    /// Address ranges where writes fail, like read-only shared pages.
    pub readonly: Vec<Range<u64>>,
    /// Its binder fd was made pollable, and whether a poll reports it ready.
    pub pollable: bool,
    pub poll_ready: bool,
}

/// What a call was answered with.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    pub errno: i32,
    /// For `BINDER_WRITE_READ`: the bytes read.
    pub read: Vec<u8>,
    /// For `BINDER_WRITE_READ`: the bwr's `write_consumed`.
    pub write_consumed: u64,
}

#[derive(Clone, Debug)]
pub enum Status {
    /// Not answered yet (blocked in a read).
    Pending,
    Answered(Outcome),
    /// Its wait ended without an answer (a signal); the thread restarts it.
    Interrupted,
    /// Its thread has issued another ioctl since (its restart, or the thread
    /// moved on).
    Abandoned,
}

struct CallRecord {
    proc: ProcId,
    tid: u32,
    cmd: u32,
    arg: u64,
    status: Status,
}

#[derive(Default)]
pub struct FakeUserspace {
    pub procs: HashMap<ProcId, FakeProc>,
    /// Captured fd tokens not dropped yet -> (source proc, source fd).
    pub captured: HashMap<u64, (ProcId, u32)>,
    next_token: u64,
    calls: BTreeMap<u64, CallRecord>,
    next_call: u64,
    /// Calls answered since the last [`FakeUserspace::take_answered`].
    answered: Vec<Call>,
    bwr_addrs: HashSet<u64>,
    /// The last bwr the driver wrote back to each call's address, whether or
    /// not the write landed.
    bwr_writes: HashMap<u64, BinderWriteRead>,
    /// Decides whether each fallible operation fails (never, if unset).
    pub fault: Option<Box<dyn FnMut(Fault) -> bool>>,
}

impl FakeUserspace {
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
        for (i, &b) in bytes.iter().enumerate() {
            let a = addr + i as u64;
            p.pages
                .entry(a / PAGE)
                .or_insert_with(|| Box::new([0; PAGE as usize]))[(a % PAGE) as usize] = b;
        }
    }

    pub fn peek(&self, id: ProcId, addr: u64, len: usize) -> Vec<u8> {
        let p = &self.procs[&id];
        (0..len as u64)
            .map(|i| {
                let a = addr + i;
                p.pages
                    .get(&(a / PAGE))
                    .map_or(0, |pg| pg[(a % PAGE) as usize])
            })
            .collect()
    }

    fn fails(&mut self, f: Fault) -> bool {
        self.fault.as_mut().is_some_and(|fault| fault(f))
    }

    /// Issue ioctl `cmd` with argument `arg` from thread `tid` of `proc`.
    pub fn ioctl(
        &mut self,
        b: &mut BinderDriver,
        proc: ProcId,
        tid: u32,
        cmd: u32,
        arg: u64,
    ) -> Call {
        // A thread runs one syscall at a time.
        for r in self.calls.values_mut() {
            if (r.proc, r.tid) == (proc, tid)
                && matches!(r.status, Status::Pending | Status::Interrupted)
            {
                r.status = Status::Abandoned;
            }
        }
        self.next_call += 1;
        let call = Call {
            id: self.next_call,
            arg,
        };
        self.calls.insert(
            call.id,
            CallRecord {
                proc,
                tid,
                cmd,
                arg,
                status: Status::Pending,
            },
        );
        b.ioctl(proc, tid, cmd, call, self);
        call
    }

    /// `BINDER_WRITE_READ` from thread `tid` of `proc` with the command
    /// stream `write` and a read buffer of `read_size` bytes, set up in the
    /// process's memory the way `IPCThreadState::talkWithDriver` does.
    pub fn write_read(
        &mut self,
        b: &mut BinderDriver,
        proc: ProcId,
        tid: u32,
        write: &[u8],
        read_size: usize,
    ) -> Call {
        assert!(
            write.len() as u64 <= READ_AT - WRITE_AT && read_size as u64 <= CALL_SPAN - READ_AT
        );
        let base = self.next_call_memory().start;
        let bwr = BinderWriteRead {
            write_size: write.len() as u64,
            write_consumed: 0,
            write_buffer: base + WRITE_AT,
            read_size: read_size as u64,
            read_consumed: 0,
            read_buffer: base + READ_AT,
        };
        self.poke(proc, base + WRITE_AT, write);
        self.poke(proc, base, pod_bytes(&bwr));
        self.bwr_addrs.insert(base);
        self.ioctl(b, proc, tid, BINDER_WRITE_READ, base)
    }

    /// Where the next [`FakeUserspace::write_read`] puts its bwr and buffers.
    pub fn next_call_memory(&self) -> Range<u64> {
        let base = SCRATCH + (self.next_call + 1) * CALL_SPAN;
        base..base + CALL_SPAN
    }

    /// Restart an interrupted call, as the kernel does after a signal: the
    /// same ioctl with the same argument.
    pub fn restart(&mut self, b: &mut BinderDriver, call: Call) -> Call {
        let r = &self.calls[&call.id];
        assert!(matches!(r.status, Status::Interrupted));
        let (proc, tid, cmd, arg) = (r.proc, r.tid, r.cmd, r.arg);
        self.ioctl(b, proc, tid, cmd, arg)
    }

    /// A signal ends `call`'s wait: it can no longer be answered.
    pub fn interrupt(&mut self, call: Call) {
        let r = self.calls.get_mut(&call.id).unwrap();
        if matches!(r.status, Status::Pending) {
            r.status = Status::Interrupted;
        }
    }

    pub fn status(&self, call: Call) -> &Status {
        &self.calls[&call.id].status
    }

    pub fn is_pending(&self, call: Call) -> bool {
        matches!(self.status(call), Status::Pending)
    }

    /// The answer of `call`, which must have been answered.
    pub fn outcome(&self, call: Call) -> &Outcome {
        match self.status(call) {
            Status::Answered(o) => o,
            s => panic!("call {} not answered: {s:?}", call.id),
        }
    }

    /// The thread that issued `call`.
    pub fn caller(&self, call: Call) -> (ProcId, u32) {
        let r = &self.calls[&call.id];
        (r.proc, r.tid)
    }

    /// The last bwr the driver wrote back for `call` (or the call it
    /// restarts), even if the write failed: what libbinder would have seen.
    pub fn last_bwr(&self, call: Call) -> Option<BinderWriteRead> {
        self.bwr_writes.get(&call.arg).copied()
    }

    /// The calls waiting for an answer (blocked in a read).
    pub fn pending_calls(&self) -> Vec<Call> {
        self.calls_where(|s| matches!(s, Status::Pending))
    }

    /// The calls a signal interrupted, not restarted yet.
    pub fn interrupted_calls(&self) -> Vec<Call> {
        self.calls_where(|s| matches!(s, Status::Interrupted))
    }

    fn calls_where(&self, f: impl Fn(&Status) -> bool) -> Vec<Call> {
        self.calls
            .iter()
            .filter(|(_, r)| f(&r.status))
            .map(|(&id, r)| Call { id, arg: r.arg })
            .collect()
    }

    /// The calls answered since the last time, in answer order.
    pub fn take_answered(&mut self) -> Vec<Call> {
        std::mem::take(&mut self.answered)
    }

    fn pending_call(&mut self, call: Call) -> Option<&mut CallRecord> {
        let r = self.calls.get_mut(&call.id)?;
        matches!(r.status, Status::Pending).then_some(r)
    }

    /// A signal arriving now, if the fault hook says so.
    fn signal(&mut self, call: Call) {
        if self.pending_call(call).is_some() && self.fails(Fault::Signal) {
            self.interrupt(call);
        }
    }
}

impl Userspace for FakeUserspace {
    fn read(&mut self, proc: ProcId, addr: u64, len: usize) -> Option<Vec<u8>> {
        if !self.procs.contains_key(&proc) || self.fails(Fault::CopyFromUser) {
            return None;
        }
        Some(self.peek(proc, addr, len))
    }

    fn write(&mut self, proc: ProcId, addr: u64, data: &[u8]) -> bool {
        if data.len() == size_of::<BinderWriteRead>() && self.bwr_addrs.contains(&addr) {
            self.bwr_writes.insert(addr, read_pod(data, 0).unwrap());
        }
        let end = addr.saturating_add(data.len() as u64);
        let Some(p) = self.procs.get(&proc) else {
            return false;
        };
        if p.readonly.iter().any(|r| r.start < end && addr < r.end) || self.fails(Fault::CopyToUser)
        {
            return false;
        }
        self.poke(proc, addr, data);
        true
    }

    fn recv_size(&mut self, proc: ProcId) -> usize {
        if self.procs.contains_key(&proc) {
            1 << 20
        } else {
            0
        }
    }

    fn alloc_recv(&mut self, proc: ProcId, size: usize, _is_async: bool) -> Option<u64> {
        if self.fails(Fault::AllocRecv) {
            return None;
        }
        let p = self.procs.get_mut(&proc)?;
        let a = p.recv_next;
        p.recv_next += size.next_multiple_of(8) as u64;
        p.live_recv.insert(a);
        Some(a)
    }

    fn write_recv(&mut self, proc: ProcId, addr: u64, data: &[u8]) {
        self.poke(proc, addr, data);
    }

    fn free_recv(&mut self, proc: ProcId, addr: u64) {
        if let Some(p) = self.procs.get_mut(&proc) {
            assert!(
                p.live_recv.remove(&addr),
                "receive buffer {addr:#x} freed twice"
            );
        }
    }

    fn capture_fd(&mut self, from: ProcId, from_fd: u32) -> Option<u64> {
        if !self.procs.contains_key(&from) || self.fails(Fault::CaptureFd) {
            return None;
        }
        self.next_token += 1;
        self.captured.insert(self.next_token, (from, from_fd));
        Some(self.next_token)
    }

    fn install_fd(&mut self, to: ProcId, call: Call, token: u64) -> Result<u32, InstallError> {
        assert!(
            self.captured.contains_key(&token),
            "fd token {token} used after drop"
        );
        self.signal(call);
        if self.pending_call(call).is_none() {
            return Err(InstallError::Interrupted);
        }
        if self.fails(Fault::InstallFd) {
            return Err(InstallError::Failed);
        }
        let p = self.procs.get_mut(&to).ok_or(InstallError::Failed)?;
        let fd = p.next_fd;
        p.next_fd += 1;
        p.installed_fds.push(fd);
        Ok(fd)
    }

    fn drop_fd(&mut self, token: u64) {
        assert!(
            self.captured.remove(&token).is_some(),
            "fd token {token} dropped twice"
        );
    }

    fn answer(&mut self, call: Call, errno: i32) -> bool {
        self.signal(call);
        let Some(r) = self.pending_call(call) else {
            return false;
        };
        let (proc, cmd, arg) = (r.proc, r.cmd, r.arg);
        let mut out = Outcome {
            errno,
            ..Default::default()
        };
        if cmd == BINDER_WRITE_READ {
            let bwr: BinderWriteRead =
                read_pod(&self.peek(proc, arg, size_of::<BinderWriteRead>()), 0).unwrap();
            out.read = self.peek(proc, bwr.read_buffer, bwr.read_consumed as usize);
            out.write_consumed = bwr.write_consumed;
        }
        self.calls.get_mut(&call.id).unwrap().status = Status::Answered(out);
        self.answered.push(call);
        true
    }

    fn pending(&mut self, call: Call) -> bool {
        self.signal(call);
        self.pending_call(call).is_some()
    }

    fn make_pollable(&mut self, proc: ProcId, call: Call) {
        if self.pending_call(call).is_some()
            && let Some(p) = self.procs.get_mut(&proc)
        {
            p.pollable = true;
        }
    }

    fn set_poll_ready(&mut self, proc: ProcId, ready: bool) {
        if let Some(p) = self.procs.get_mut(&proc) {
            p.poll_ready = ready;
        }
    }
}
