//! A deterministic in-memory [`Userspace`] for the tests.
//!
//! Each process's address space is a sparse page map and its receive region a
//! bump allocator, so transactions are translated end to end without the
//! kernel. Ioctls are issued the way libbinder issues them. Each
//! `BINDER_WRITE_READ` gets its bwr, write buffer and read buffer in the
//! calling process's memory, and its answer is recorded with what the
//! driver copied out. Any fallible operation can be made to fail through
//! [`FakeUserspace::fault`], and a call's wait can be interrupted like a
//! signal does, so every rollback path is reachable. Once a call has ended
//! (answered, interrupted or abandoned), the driver must not write into its
//! bwr or buffers in any later ioctl, because the thread may be stopped or
//! may already be reusing that memory.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

use crate::binder::driver::{BinderDriver, Call, InstallError, ProcId, Userspace};
use crate::binder::uapi::{BINDER_WRITE_READ, BinderWriteRead, pod_bytes, read_pod};

const PAGE: u64 = 4096;
/// The base address of the calls' bwrs and buffers. Each call gets one
/// `CALL_SPAN`.
const SCRATCH: u64 = 0x7000_0000_0000;
const CALL_SPAN: u64 = 0x2_0000;
const WRITE_AT: u64 = 0x100;
const READ_AT: u64 = 0x1_0000;

/// A fallible operation that [`FakeUserspace::fault`] can fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Reading user memory (`EFAULT`).
    CopyFromUser,
    /// Writing user memory other than a call's bwr (`EFAULT`).
    CopyToUser,
    /// Writing a call's bwr back (`EFAULT`). libbinder then can't tell how
    /// much of its write was consumed and resends all of it. This also happens
    /// with the real kernel.
    CopyBwr,
    /// Allocating in a receive region (`ENOSPC`).
    AllocRecv,
    /// Capturing a sender's fd (`EBADF`).
    CaptureFd,
    /// Installing an fd into a reader, as when its fd table is full.
    InstallFd,
    /// A signal ends the call's wait right before it is answered or an fd is
    /// installed for it.
    Signal,
}

#[derive(Default)]
pub struct FakeProc {
    pages: HashMap<u64, Box<[u8; PAGE as usize]>>,
    recv_next: u64,
    /// Receive allocations not freed yet.
    pub live_recv: BTreeSet<u64>,
    next_fd: u32,
    pub installed_fds: Vec<u32>,
    /// Address ranges where writes fail, like read-only shared pages.
    pub readonly: Vec<Range<u64>>,
    /// How many times its binder fd was swapped for a pollable one (once at
    /// most, as in notify.rs), and whether a poll reports it ready.
    pub poll_swaps: u32,
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
    /// A signal ended its wait without an answer. The thread restarts it.
    Interrupted,
    /// Its thread has since issued another ioctl, either the restart or an
    /// unrelated one.
    Abandoned,
}

struct CallRecord {
    proc: ProcId,
    tid: u32,
    cmd: u32,
    arg: u64,
    status: Status,
    /// When it stopped being pending (see `FakeUserspace::now`).
    ended: Option<u64>,
}

#[derive(Default)]
pub struct FakeUserspace {
    pub procs: HashMap<ProcId, FakeProc>,
    /// Captured fd tokens not dropped yet -> (source proc, source fd).
    pub captured: HashMap<u64, (ProcId, u32)>,
    /// Captured tokens installed into a reader at least once.
    installed: HashSet<u64>,
    /// Captured fds (source proc, source fd) dropped without ever being
    /// installed, because their transaction failed or was dropped.
    pub never_installed: Vec<(ProcId, u32)>,
    next_token: u64,
    calls: BTreeMap<u64, CallRecord>,
    next_call: u64,
    bwr_addrs: HashSet<u64>,
    /// The latest call whose bwr and buffers are at each `bwr_addrs` address.
    /// A restart reuses the memory of the call it restarts.
    call_at: HashMap<u64, u64>,
    /// A clock that advances at every ioctl and every external signal. An
    /// ioctl may still write into a call that ended during it, to roll a read
    /// back, but not into one that ended earlier.
    now: u64,
    /// The driver's clock in milliseconds ([`FakeUserspace::advance`]), and
    /// the earliest time it asked to be woken at.
    pub clock_ms: u64,
    pub timer: Option<u64>,
    /// Decides whether each fallible operation fails. If unset, none fail.
    pub fault: Option<Box<dyn FnMut(Fault) -> bool>>,
    /// Each process's OpenHarmony (token, first token) pair.
    pub tokens: HashMap<ProcId, (u64, u64)>,
}

impl FakeUserspace {
    pub fn add_proc(&mut self, id: ProcId, recv_base: u64) {
        self.procs.insert(
            id,
            FakeProc {
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
        self.now += 1;
        // A thread runs one syscall at a time.
        for r in self.calls.values_mut() {
            if (r.proc, r.tid) == (proc, tid)
                && matches!(r.status, Status::Pending | Status::Interrupted)
            {
                r.status = Status::Abandoned;
                r.ended.get_or_insert(self.now);
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
                ended: None,
            },
        );
        if self.bwr_addrs.contains(&arg) {
            self.call_at.insert(arg, call.id);
        }
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

    /// Restarts an interrupted call with the same ioctl and argument, as the
    /// kernel does after a signal.
    pub fn restart(&mut self, b: &mut BinderDriver, call: Call) -> Call {
        let r = &self.calls[&call.id];
        assert!(matches!(r.status, Status::Interrupted));
        let (proc, tid, cmd, arg) = (r.proc, r.tid, r.cmd, r.arg);
        self.ioctl(b, proc, tid, cmd, arg)
    }

    /// Let `ms` milliseconds pass, firing the driver's timer if it is due.
    pub fn advance(&mut self, b: &mut BinderDriver, ms: u64) {
        self.clock_ms += ms;
        if self.timer.is_some_and(|t| t <= self.clock_ms) {
            self.now += 1;
            self.timer = None;
            b.expire(self);
        }
    }

    /// A signal ends `call`'s wait, so it can no longer be answered.
    pub fn interrupt(&mut self, call: Call) {
        self.now += 1;
        self.end(call, Status::Interrupted);
    }

    fn end(&mut self, call: Call, status: Status) {
        let r = self.calls.get_mut(&call.id).unwrap();
        if matches!(r.status, Status::Pending) {
            r.status = status;
            r.ended = Some(self.now);
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

    fn pending_call(&mut self, call: Call) -> Option<&mut CallRecord> {
        let r = self.calls.get_mut(&call.id)?;
        matches!(r.status, Status::Pending).then_some(r)
    }

    /// Interrupts `call` now if the fault hook says so.
    fn signal(&mut self, call: Call) {
        if self.pending_call(call).is_some() && self.fails(Fault::Signal) {
            self.end(call, Status::Interrupted);
        }
    }

    /// The call whose bwr or buffers are at `addr`, if it ended before the
    /// current ioctl.
    fn ended_call_at(&self, addr: u64) -> Option<u64> {
        let base = SCRATCH + addr.checked_sub(SCRATCH)? / CALL_SPAN * CALL_SPAN;
        let id = *self.call_at.get(&base)?;
        let ended = self.calls[&id].ended?;
        (ended != self.now).then_some(id)
    }
}

impl Userspace for FakeUserspace {
    fn access_token(&mut self, proc: ProcId, _tid: u32) -> (u64, u64) {
        self.tokens.get(&proc).copied().unwrap_or_default()
    }
    fn read(&mut self, proc: ProcId, addr: u64, len: usize) -> Option<Vec<u8>> {
        if !self.procs.contains_key(&proc) || self.fails(Fault::CopyFromUser) {
            return None;
        }
        Some(self.peek(proc, addr, len))
    }

    fn write(&mut self, proc: ProcId, addr: u64, data: &[u8]) -> bool {
        if let Some(id) = self.ended_call_at(addr) {
            panic!("the driver wrote into the memory of call {id}, which has ended");
        }
        let end = addr.saturating_add(data.len() as u64);
        let Some(p) = self.procs.get(&proc) else {
            return false;
        };
        let bwr = data.len() == size_of::<BinderWriteRead>() && self.bwr_addrs.contains(&addr);
        let fault = if bwr {
            Fault::CopyBwr
        } else {
            Fault::CopyToUser
        };
        if p.readonly.iter().any(|r| r.start < end && addr < r.end) || self.fails(fault) {
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
        self.installed.insert(token);
        Ok(fd)
    }

    fn drop_fd(&mut self, token: u64) {
        let source = self.captured.remove(&token);
        assert!(source.is_some(), "fd token {token} dropped twice");
        if !self.installed.remove(&token) {
            self.never_installed.extend(source);
        }
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
        self.end(call, Status::Answered(out));
        true
    }

    fn pending(&mut self, call: Call) -> bool {
        self.signal(call);
        self.pending_call(call).is_some()
    }

    fn make_pollable(&mut self, proc: ProcId, call: Call) -> bool {
        self.signal(call);
        if self.pending_call(call).is_none() {
            return false;
        }
        let Some(p) = self.procs.get_mut(&proc) else {
            return false;
        };
        // The fd is swapped once per process, as in notify.rs.
        if p.poll_swaps == 0 {
            p.poll_swaps = 1;
        }
        true
    }

    fn set_poll_ready(&mut self, proc: ProcId, ready: bool) {
        if let Some(p) = self.procs.get_mut(&proc)
            && p.poll_swaps > 0
        {
            p.poll_ready = ready;
        }
    }

    fn now_ms(&mut self) -> u64 {
        self.clock_ms
    }

    fn arm_timer(&mut self, at_ms: u64) {
        self.timer = Some(self.timer.map_or(at_ms, |t| t.min(at_ms)));
    }
}
