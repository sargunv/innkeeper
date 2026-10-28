//! Property tests of the read lifecycle (see `driver.rs`): libbinder-like
//! threads drive the real driver while userspace fails under it, and the
//! driver must lose nothing:
//!
//!  * no work waits with a thread that isn't reading it (`check_invariants`
//!    after every ioctl), and every read a thread is blocked in is one the
//!    driver holds;
//!  * every synchronous transaction is answered, with its reply or a failed
//!    reply, and every oneway that was sent is received once (unless its fd
//!    couldn't be installed, which drops it, as in the kernel);
//!  * while a signal keeps a looper stopped, the other threads still drain
//!    all of the process's work;
//!  * refcounts, buffers and fds are conserved: once the threads are idle
//!    nothing is queued or in flight, and once every process is gone
//!    nothing is left.
//!
//! The threads behave like libbinder: they resend what their bwr says a
//! write didn't consume, restart interrupted calls, free every buffer they
//! get, ack node and death work, reply to every call, and start the looper
//! they are asked for. A server, the context manager, takes refs and death
//! notifications on the objects its clients send, sometimes calls them back
//! before replying, and sometimes replies with an fd. Clients make
//! synchronous and oneway calls, some with fds, and their calling thread
//! exits when it is done. The server has two threads that poll instead of
//! blocking, and the first client one. One test
//! injects random failures, signals and stops; the other fails each fallible
//! operation of a fixed run in turn.

use std::collections::BTreeSet;

use crate::binder::driver::*;
use crate::binder::uapi::*;
use crate::fake::{FakeUserspace, Fault, Rng, Status};

/// Where a thread builds its transaction data (at `DATA + tid * 0x1_0000`).
const DATA: u64 = 0x6000_0000;
const READ_SIZE: usize = 256;

struct Owed {
    code: u32,
    buffer: u64,
    /// The call accepts fds in its reply.
    accept_fds: bool,
    /// A handle to call before replying, and the code of that call once made.
    call_back: Option<u32>,
    nested: Option<u32>,
}

struct Thread {
    proc: ProcId,
    tid: u32,
    looper: bool,
    /// It polls its binder fd (a write-only BC_ENTER_LOOPER sets that up) and
    /// reads only when the fd is readable.
    poller: bool,
    /// Its poll setup is done.
    polling: bool,
    /// Calls still to make to the context manager.
    calls_left: u32,
    /// The ioctl in flight, and the commands it was issued with.
    call: Option<(Call, Vec<u8>)>,
    /// Commands not consumed yet.
    out: Vec<u8>,
    /// Codes of this thread's unanswered synchronous calls, innermost last.
    awaiting: Vec<u32>,
    /// A oneway it sent that isn't acknowledged yet.
    sending: Option<Oneway>,
    /// Calls this thread received and hasn't replied to, innermost last.
    owed: Vec<Owed>,
    /// A signal stopped it in a read (SIGSTOP, ptrace): it doesn't restart
    /// until the faults end.
    stopped: bool,
    /// Its ioctl in flight is BINDER_THREAD_EXIT.
    exiting: bool,
    /// It exited, or its process died.
    dead: bool,
}

impl Thread {
    fn new(proc: ProcId, tid: u32, first: Option<u32>, calls_left: u32) -> Thread {
        let mut out = Vec::new();
        if let Some(cmd) = first {
            put(&mut out, &cmd);
        }
        Thread {
            proc,
            tid,
            looper: first.is_some(),
            poller: false,
            polling: false,
            calls_left,
            call: None,
            out,
            awaiting: Vec::new(),
            sending: None,
            owed: Vec::new(),
            stopped: false,
            exiting: false,
            dead: false,
        }
    }

    /// Whether its next ioctl reads.
    fn reads(&self, poll_ready: bool) -> bool {
        (self.looper && (!self.poller || (self.polling && poll_ready)))
            || !self.awaiting.is_empty()
            || self.sending.is_some()
            || !self.owed.is_empty()
    }
}

/// A oneway call: its sender, its code, and the sender's fd it carries.
#[derive(Clone, Copy)]
struct Oneway {
    from: ProcId,
    code: u32,
    fd: Option<u32>,
}

struct Faults {
    /// Percent of reads whose read buffer can't be written.
    readonly_pct: u64,
}

/// What a run exercised, so the random test can tell it covered each case.
#[derive(Default)]
struct Seen {
    spawned: u32,
    stopped: u32,
    exited: u32,
    oneways_with_fds: u32,
    replies_with_fds: u32,
}

struct Sim {
    b: BinderDriver,
    us: FakeUserspace,
    rng: Rng,
    server: ProcId,
    /// The client that may die.
    victim: Option<ProcId>,
    threads: Vec<Thread>,
    next_code: u32,
    /// Handles the server already holds refs and a death notification on.
    held: BTreeSet<u32>,
    /// Oneways their senders saw accepted, and the codes of those the
    /// server received.
    oneways_sent: Vec<Oneway>,
    oneways_received: Vec<u32>,
    faults: Option<Faults>,
    seen: Seen,
}

fn put<T: Copy>(v: &mut Vec<u8>, x: &T) {
    v.extend_from_slice(pod_bytes(x));
}

fn fd_object(fd: u32) -> BinderFdObject {
    BinderFdObject {
        hdr_type: BINDER_TYPE_FD,
        fd_or_pad: fd as u64,
        ..Default::default()
    }
}

impl Sim {
    fn new(seed: u64, clients: usize, calls: u32) -> Sim {
        let mut b = BinderDriver::new();
        let mut us = FakeUserspace::default();
        let server = b.open(1000, 1000, 0, false);
        us.add_proc(server, 0x1_0000_0000);
        let ext = FlatBinderObject {
            flags: FLAT_BINDER_FLAG_ACCEPTS_FDS,
            ..Default::default()
        };
        us.poke(server, DATA, pod_bytes(&ext));
        let c = us.ioctl(&mut b, server, 1, BINDER_SET_CONTEXT_MGR_EXT, DATA);
        assert_eq!(us.outcome(c).errno, 0);
        // The server may start one more looper.
        us.poke(server, DATA, &1u32.to_le_bytes());
        let c = us.ioctl(&mut b, server, 1, BINDER_SET_MAX_THREADS, DATA);
        assert_eq!(us.outcome(c).errno, 0);
        let poller = |proc, tid| Thread {
            poller: true,
            ..Thread::new(proc, tid, Some(BC_ENTER_LOOPER), 0)
        };
        let mut threads = vec![
            Thread::new(server, 1, Some(BC_ENTER_LOOPER), 0),
            Thread::new(server, 2, Some(BC_ENTER_LOOPER), 0),
            poller(server, 3),
            poller(server, 4),
        ];
        let mut victim = None;
        for i in 0..clients {
            let p = b.open(2000 + i as i32, 10_000 + i as u32, 0, false);
            us.add_proc(p, 0x1_0000_0000 * (i as u64 + 2));
            threads.push(Thread::new(p, 1, Some(BC_ENTER_LOOPER), 0));
            threads.push(Thread::new(p, 2, None, calls));
            if i == 0 {
                threads.push(poller(p, 3));
            } else {
                victim = Some(p);
            }
        }
        Sim {
            b,
            us,
            rng: Rng(seed),
            server,
            victim,
            threads,
            next_code: 1,
            held: BTreeSet::new(),
            oneways_sent: Vec::new(),
            oneways_received: Vec::new(),
            faults: None,
            seen: Seen::default(),
        }
    }

    fn code(&mut self) -> u32 {
        self.next_code += 1;
        self.next_code
    }

    /// The thread's next ioctl, or the handling of the last one's answer.
    /// False if it has nothing to do.
    fn step(&mut self, i: usize) -> bool {
        let t = &self.threads[i];
        if t.dead || t.stopped {
            return false;
        }
        if let Some((call, sent)) = t.call.clone() {
            match self.us.status(call).clone() {
                Status::Pending => return false,
                Status::Interrupted => {
                    let c = self.us.restart(&mut self.b, call);
                    self.threads[i].call = Some((c, sent));
                }
                Status::Answered(o) => {
                    // What the write consumed, as its bwr says.
                    let consumed = o.write_consumed as usize;
                    let t = &mut self.threads[i];
                    t.call = None;
                    let mut out = sent[consumed..].to_vec();
                    out.append(&mut t.out);
                    t.out = out;
                    if t.exiting {
                        t.dead = true;
                        self.seen.exited += 1;
                    } else if o.errno == 0 {
                        t.polling |= t.poller;
                        self.handle_read(i, &o.read);
                    }
                }
                Status::Abandoned => unreachable!(),
            }
            return true;
        }
        self.plan(i);
        let t = &self.threads[i];
        let (proc, tid) = (t.proc, t.tid);
        let reads = t.reads(self.us.procs[&proc].poll_ready);
        if !reads && t.out.is_empty() {
            if t.looper || t.calls_left > 0 {
                return false;
            }
            // A calling thread that is done exits.
            let c = self.us.ioctl(&mut self.b, proc, tid, BINDER_THREAD_EXIT, 0);
            let t = &mut self.threads[i];
            t.exiting = true;
            t.call = Some((c, Vec::new()));
            return true;
        }
        if let Some(f) = &self.faults
            && self.rng.chance(f.readonly_pct)
        {
            // The read buffer. (Not the bwr: if libbinder can't see what its
            // write consumed, it resends it, which the kernel can't tell
            // apart from a new write either.)
            let span = self.us.next_call_memory();
            let read_buffer = span.start + 0x1_0000..span.end;
            self.us
                .procs
                .get_mut(&proc)
                .unwrap()
                .readonly
                .push(read_buffer);
        }
        let out = std::mem::take(&mut self.threads[i].out);
        let size = if reads { READ_SIZE } else { 0 };
        let c = self.us.write_read(&mut self.b, proc, tid, &out, size);
        self.threads[i].call = Some((c, out));
        true
    }

    /// Queue the thread's next commands: a call back or a reply it owes, or
    /// its next call.
    fn plan(&mut self, i: usize) {
        let t = &self.threads[i];
        let (proc, tid) = (t.proc, t.tid);
        let at = DATA + tid as u64 * 0x1_0000;
        if let Some(o) = t.owed.last() {
            match (o.call_back, o.nested) {
                (Some(h), None) => {
                    let code = self.code();
                    let td = BinderTransactionData {
                        target: h as u64,
                        code,
                        ..Default::default()
                    };
                    let t = &mut self.threads[i];
                    put(&mut t.out, &BC_TRANSACTION);
                    put(&mut t.out, &td);
                    t.awaiting.push(code);
                    t.owed.last_mut().unwrap().nested = Some(code);
                }
                (_, Some(n)) if t.awaiting.contains(&n) => {}
                _ => {
                    let o = self.threads[i].owed.pop().unwrap();
                    let mut td = BinderTransactionData {
                        code: o.code,
                        ..Default::default()
                    };
                    // Sometimes with an fd.
                    if o.accept_fds && self.rng.chance(40) {
                        self.us.poke(proc, at, pod_bytes(&fd_object(4)));
                        self.us.poke(proc, at + 0x8000, &0u64.to_le_bytes());
                        td.data_size = size_of::<BinderFdObject>() as u64;
                        td.offsets_size = 8;
                        td.data_buffer = at;
                        td.data_offsets = at + 0x8000;
                        self.seen.replies_with_fds += 1;
                    }
                    let t = &mut self.threads[i];
                    put(&mut t.out, &BC_REPLY);
                    put(&mut t.out, &td);
                    put(&mut t.out, &BC_FREE_BUFFER);
                    put(&mut t.out, &o.buffer);
                }
            }
            return;
        }
        if t.calls_left > 0 && t.awaiting.is_empty() && t.sending.is_none() {
            let code = self.code();
            let oneway = self.rng.chance(30);
            // Mostly with an object of the caller's, sometimes with an fd.
            let mut data = Vec::new();
            let mut offsets = Vec::new();
            if self.rng.chance(70) {
                offsets.push(data.len() as u64);
                put(
                    &mut data,
                    &FlatBinderObject {
                        hdr_type: BINDER_TYPE_BINDER,
                        flags: 0,
                        binder_or_handle: 0xB000 + proc as u64 * 0x10,
                        cookie: proc as u64,
                    },
                );
            }
            // A oneway's fd number is its own, to tell which were dropped.
            let fd = self
                .rng
                .chance(40)
                .then_some(if oneway { 0x1_0000 + code } else { 3 });
            if let Some(fd) = fd {
                offsets.push(data.len() as u64);
                put(&mut data, &fd_object(fd));
            }
            let offs: Vec<u8> = offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
            self.us.poke(proc, at, &data);
            self.us.poke(proc, at + 0x8000, &offs);
            let td = BinderTransactionData {
                target: 0,
                code,
                flags: TF_ACCEPT_FDS | if oneway { TF_ONE_WAY } else { 0 },
                data_size: data.len() as u64,
                offsets_size: offs.len() as u64,
                data_buffer: at,
                data_offsets: at + 0x8000,
                ..Default::default()
            };
            let t = &mut self.threads[i];
            put(&mut t.out, &BC_TRANSACTION);
            put(&mut t.out, &td);
            if oneway {
                t.sending = Some(Oneway {
                    from: proc,
                    code,
                    fd,
                });
            } else {
                t.awaiting.push(code);
            }
            t.calls_left -= 1;
        }
    }

    /// Act on a BR_* stream the thread read, as libbinder does.
    fn handle_read(&mut self, i: usize, read: &[u8]) {
        let proc = self.threads[i].proc;
        let mut off = 0;
        while off + 4 <= read.len() {
            let cmd: u32 = read_pod(read, off).unwrap();
            off += 4;
            let at = off;
            off += ((cmd >> 16) & 0x3fff) as usize;
            match cmd {
                BR_NOOP | BR_OK => {}
                BR_SPAWN_LOOPER => {
                    // IPCThreadState::spawnPooledThread.
                    let tid = 100 + self.threads.len() as u32;
                    self.threads
                        .push(Thread::new(proc, tid, Some(BC_REGISTER_LOOPER), 0));
                    self.seen.spawned += 1;
                }
                BR_TRANSACTION_COMPLETE => {
                    // A oneway's acknowledgement (a synchronous call's comes
                    // with its reply).
                    if let Some(sent) = self.threads[i].sending.take() {
                        self.seen.oneways_with_fds += sent.fd.is_some() as u32;
                        self.oneways_sent.push(sent);
                    }
                }
                BR_TRANSACTION | BR_TRANSACTION_SEC_CTX => {
                    let td: BinderTransactionData = read_pod(read, at).unwrap();
                    if td.flags & TF_ONE_WAY != 0 {
                        self.oneways_received.push(td.code);
                        let out = &mut self.threads[i].out;
                        put(out, &BC_FREE_BUFFER);
                        put(out, &td.data_buffer);
                        continue;
                    }
                    let handle = self.first_handle(proc, &td);
                    let mut call_back = None;
                    if let Some(h) = handle
                        && proc == self.server
                    {
                        if self.held.insert(h) {
                            // Hold it, like a BpBinder, and watch it.
                            let out = &mut self.threads[i].out;
                            for c in [BC_INCREFS, BC_ACQUIRE] {
                                put(out, &c);
                                put(out, &h);
                            }
                            put(out, &BC_REQUEST_DEATH_NOTIFICATION);
                            put(out, &h);
                            put(out, &(h as u64));
                        }
                        if self.rng.chance(40) {
                            call_back = Some(h);
                        }
                    }
                    self.threads[i].owed.push(Owed {
                        code: td.code,
                        buffer: td.data_buffer,
                        accept_fds: td.flags & TF_ACCEPT_FDS != 0,
                        call_back,
                        nested: None,
                    });
                }
                BR_REPLY => {
                    let td: BinderTransactionData = read_pod(read, at).unwrap();
                    let t = &mut self.threads[i];
                    assert_eq!(t.awaiting.pop(), Some(td.code), "reply to the wrong call");
                    put(&mut t.out, &BC_FREE_BUFFER);
                    put(&mut t.out, &td.data_buffer);
                }
                BR_DEAD_REPLY | BR_FAILED_REPLY => {
                    // A oneway that wasn't sent, or the innermost call failed.
                    let t = &mut self.threads[i];
                    if t.sending.take().is_none() {
                        assert!(t.awaiting.pop().is_some(), "failed reply to no call");
                    }
                }
                BR_INCREFS | BR_ACQUIRE => {
                    let pc: BinderPtrCookie = read_pod(read, at).unwrap();
                    let done = if cmd == BR_INCREFS {
                        BC_INCREFS_DONE
                    } else {
                        BC_ACQUIRE_DONE
                    };
                    let out = &mut self.threads[i].out;
                    put(out, &done);
                    put(out, &pc);
                }
                BR_RELEASE | BR_DECREFS | BR_CLEAR_DEATH_NOTIFICATION_DONE => {}
                BR_DEAD_BINDER => {
                    let cookie: u64 = read_pod(read, at).unwrap();
                    let out = &mut self.threads[i].out;
                    put(out, &BC_DEAD_BINDER_DONE);
                    put(out, &cookie);
                }
                _ => panic!("unexpected {cmd:#x}"),
            }
        }
    }

    /// The first handle among a received transaction's objects.
    fn first_handle(&self, proc: ProcId, td: &BinderTransactionData) -> Option<u32> {
        let offs = self
            .us
            .peek(proc, td.data_offsets, td.offsets_size as usize);
        offs.chunks(8).find_map(|o| {
            let o = u64::from_le_bytes(o.try_into().unwrap());
            let obj = self
                .us
                .peek(proc, td.data_buffer + o, size_of::<FlatBinderObject>());
            let fbo: FlatBinderObject = read_pod(&obj, 0).unwrap();
            (fbo.hdr_type == BINDER_TYPE_HANDLE).then_some(fbo.binder_or_handle as u32)
        })
    }

    fn kill(&mut self, proc: ProcId) {
        self.b.release_proc(proc, &mut self.us);
        for t in self.threads.iter_mut().filter(|t| t.proc == proc) {
            t.dead = true;
        }
    }

    /// A signal ends a blocked read's wait. Sometimes it stops the thread, if
    /// it is a server looper waiting for work and no other is stopped.
    fn signal(&mut self, call: Call) {
        self.us.interrupt(call);
        let i = self
            .threads
            .iter()
            .position(|t| t.call.as_ref().is_some_and(|(c, _)| *c == call))
            .unwrap();
        let t = &self.threads[i];
        let idle_looper = t.proc == self.server
            && t.looper
            && !t.poller
            && t.owed.is_empty()
            && t.awaiting.is_empty();
        if idle_looper && !self.threads.iter().any(|t| t.stopped) && self.rng.chance(50) {
            self.threads[i].stopped = true;
            self.seen.stopped += 1;
        }
    }

    /// After every ioctl: the driver's own invariants, and every read a
    /// thread is blocked in is one the driver holds.
    fn check(&self) {
        self.b.check_invariants();
        let held = self.b.blocked_threads();
        for c in self.us.pending_calls() {
            assert!(held.contains(&self.us.caller(c)), "call {} forgotten", c.id);
        }
    }

    /// Run until every thread is idle. With random faults, the first
    /// `fault_steps` steps also have signals interrupt (or stop) blocked
    /// reads, and a client may die; then all faults stop. Stopped threads
    /// resume once the others are idle.
    fn run(&mut self, fault_steps: usize) {
        for n in 0.. {
            assert!(n < 100_000, "no quiescence: livelock");
            let faulty = n < fault_steps && self.faults.is_some();
            if n == fault_steps && self.faults.take().is_some() {
                self.us.fault = None;
            }
            if faulty && self.rng.chance(5) {
                let pending = self.us.pending_calls();
                if !pending.is_empty() {
                    let call = pending[self.rng.below(pending.len() as u64) as usize];
                    self.signal(call);
                }
            }
            if faulty
                && self.rng.chance(1)
                && let Some(victim) = self.victim.take()
            {
                self.kill(victim);
                self.check();
            }
            let ready: Vec<usize> = (0..self.threads.len())
                .filter(|&i| {
                    let t = &self.threads[i];
                    !t.dead
                        && match &t.call {
                            Some((c, _)) => !self.us.is_pending(*c),
                            None => true,
                        }
                })
                .collect();
            let mut stepped = false;
            let start = self.rng.below(ready.len() as u64) as usize;
            for k in 0..ready.len() {
                if self.step(ready[(start + k) % ready.len()]) {
                    stepped = true;
                    break;
                }
            }
            self.check();
            if !stepped && n >= fault_steps {
                if !self.threads.iter().any(|t| t.stopped) {
                    return;
                }
                self.check_drained();
                for t in &mut self.threads {
                    t.stopped = false;
                }
            }
        }
    }

    /// With a looper stopped and the others idle, nothing waits for it: every
    /// call is done and no work is queued.
    fn check_drained(&self) {
        assert_eq!(self.b.queued_work(), 0, "work waits for a stopped thread");
        for t in self.threads.iter().filter(|t| !t.dead && !t.stopped) {
            assert!(
                t.calls_left == 0 && t.awaiting.is_empty() && t.sending.is_none(),
                "proc {} thread {}: calls wait for a stopped thread",
                t.proc,
                t.tid
            );
        }
    }

    /// Every call answered, every oneway received, nothing queued or in
    /// flight; then, once every process is gone, nothing left at all.
    fn finish(mut self) -> Seen {
        for t in self.threads.iter().filter(|t| !t.dead) {
            assert_eq!(t.calls_left, 0);
            assert!(
                t.awaiting.is_empty() && t.sending.is_none(),
                "proc {} thread {}: calls unanswered",
                t.proc,
                t.tid
            );
            assert!(t.owed.is_empty() && t.out.is_empty());
            assert!(!t.poller || t.polling);
        }
        // Each process with polling threads swapped its fd once.
        for t in self.threads.iter().filter(|t| t.poller && !t.dead) {
            assert_eq!(self.us.procs[&t.proc].poll_swaps, 1);
        }
        let mut received = self.oneways_received.clone();
        received.sort();
        assert!(
            received.windows(2).all(|w| w[0] != w[1]),
            "a oneway received twice"
        );
        // Every accepted oneway was received, but one whose fd was never
        // installed: it failed for good in its reader, which drops it.
        for &Oneway { from, code, fd } in &self.oneways_sent {
            let dropped = fd.is_some_and(|fd| self.us.never_installed.contains(&(from, fd)));
            assert!(
                dropped || received.binary_search(&code).is_ok(),
                "oneway {code} lost"
            );
        }
        // The server asked for one looper at most, and only once: a request
        // a rollback took back was asked for again.
        assert!(
            self.seen.spawned <= 1,
            "more loopers requested than allowed"
        );
        assert_eq!(
            self.b.requested_threads(self.server),
            0,
            "looper requests lost"
        );
        let blocked: Vec<(ProcId, u32)> = self
            .threads
            .iter()
            .filter(|t| !t.dead && t.call.is_some())
            .map(|t| (t.proc, t.tid))
            .collect();
        let mut held = self.b.blocked_threads();
        held.retain(|t| blocked.contains(t));
        assert_eq!(held.len(), blocked.len());
        assert_eq!(self.b.queued_work(), 0, "work left queued");
        let (txns, buffers, _) = self.b.counts();
        assert_eq!((txns, buffers), (0, 0), "transactions or buffers left");
        let procs: BTreeSet<ProcId> = self
            .threads
            .iter()
            .filter(|t| !t.dead)
            .map(|t| t.proc)
            .collect();
        for p in procs {
            self.b.release_proc(p, &mut self.us);
            self.b.check_invariants();
        }
        assert_eq!(
            self.b.counts(),
            (0, 0, 0),
            "leaked transactions/buffers/nodes"
        );
        assert!(self.us.captured.is_empty(), "leaked captured fds");
        for (p, fp) in &self.us.procs {
            assert!(fp.live_recv.is_empty(), "proc {p} leaked receive buffers");
        }
        self.seen
    }
}

#[test]
fn nothing_lost_under_random_failures() {
    let mut total = Seen::default();
    for seed in 0..300 {
        eprintln!("seed {seed}"); // shown if it fails
        let mut s = Sim::new(seed, 2, 4);
        let mut fr = Rng(seed ^ 0x5eed);
        s.us.fault = Some(Box::new(move |f| {
            let pct = match f {
                Fault::Signal => 30,
                Fault::InstallFd => 50,
                Fault::AllocRecv | Fault::CaptureFd => 20,
                Fault::CopyFromUser => 10,
                // Copies out fail through read-only memory, which stays so.
                // (A bwr libbinder can't read back makes it resend its write.)
                Fault::CopyToUser | Fault::CopyBwr => 0,
            };
            fr.below(1000) < pct
        }));
        s.faults = Some(Faults { readonly_pct: 5 });
        s.run(600);
        let seen = s.finish();
        total.spawned += seen.spawned;
        total.stopped += seen.stopped;
        total.exited += seen.exited;
        total.oneways_with_fds += seen.oneways_with_fds;
        total.replies_with_fds += seen.replies_with_fds;
    }
    // Every case was exercised.
    for (what, n) in [
        ("spawned loopers", total.spawned),
        ("stopped loopers", total.stopped),
        ("thread exits", total.exited),
        ("oneways with fds", total.oneways_with_fds),
        ("replies with fds", total.replies_with_fds),
    ] {
        assert!(n > 0, "no {what}");
    }
}

#[test]
fn nothing_lost_when_any_one_operation_fails() {
    // For each scenario: a fault-free run, then the same run failing its
    // k-th fallible operation, for every k. (Not bwr write-backs: libbinder
    // would resend the whole write.)
    for seed in 0..8 {
        let mut runs = 0;
        for k in 0.. {
            let fired = std::rc::Rc::new(std::cell::Cell::new(false));
            let mut s = Sim::new(seed, 1, 3);
            let (mut n, f) = (0, fired.clone());
            s.us.fault = Some(Box::new(move |op| {
                if op == Fault::CopyBwr {
                    return false;
                }
                n += 1;
                let hit = n == k + 1;
                f.set(f.get() || hit);
                hit
            }));
            s.run(0);
            // Without a fault, no oneway may be dropped for its fd.
            assert!(fired.get() || s.us.never_installed.is_empty());
            s.finish();
            runs += 1;
            if !fired.get() {
                break;
            }
        }
        assert!(runs > 50, "seed {seed}: only {runs} fault points");
    }
}
