//! Property tests of the read lifecycle (see `driver.rs`): libbinder-like
//! threads drive the real driver while userspace fails under it, and the
//! driver must lose nothing:
//!
//!  * no work waits with a thread that isn't reading it (`check_invariants`
//!    after every ioctl), and every read a thread is blocked in is one the
//!    driver holds;
//!  * every synchronous transaction is answered, with its reply or a failed
//!    reply;
//!  * refcounts, buffers and fds are conserved: once the threads are idle
//!    nothing is queued or in flight, and once every process is gone
//!    nothing is left.
//!
//! The threads behave like libbinder: they resend what a write didn't
//! consume, restart interrupted calls, free every buffer they get, ack node
//! and death work, and reply to every call. A server, the context manager,
//! takes refs and death notifications on the objects its clients send and
//! sometimes calls them back before replying. One test injects random
//! failures; the other fails each fallible operation of a fixed run in turn.

use std::collections::BTreeSet;

use crate::binder::driver::*;
use crate::binder::uapi::*;
use crate::fake::{FakeUserspace, Fault, Rng, Status};

/// Where a caller builds its transaction data.
const DATA: u64 = 0x6000_0000;
const READ_SIZE: usize = 256;

struct Owed {
    code: u32,
    buffer: u64,
    /// A handle to call before replying, and the code of that call once made.
    call_back: Option<u32>,
    nested: Option<u32>,
}

struct Thread {
    proc: ProcId,
    tid: u32,
    looper: bool,
    /// Synchronous calls still to make to the context manager.
    calls_left: u32,
    /// The ioctl in flight, and the commands it was issued with.
    call: Option<(Call, Vec<u8>)>,
    /// Commands not consumed yet.
    out: Vec<u8>,
    /// Codes of this thread's unanswered synchronous calls, innermost last.
    awaiting: Vec<u32>,
    /// Calls this thread received and hasn't replied to, innermost last.
    owed: Vec<Owed>,
    dead: bool,
}

struct Faults {
    /// Percent of reads whose read buffer can't be written.
    readonly_pct: u64,
}

struct Sim {
    b: BinderDriver,
    us: FakeUserspace,
    rng: Rng,
    server: ProcId,
    threads: Vec<Thread>,
    next_code: u32,
    /// Handles the server already holds refs and a death notification on.
    held: BTreeSet<u32>,
    faults: Option<Faults>,
}

fn put<T: Copy>(v: &mut Vec<u8>, x: &T) {
    v.extend_from_slice(pod_bytes(x));
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
        let mut threads = Vec::new();
        let mut thread = |proc, tid, looper, calls_left| {
            let mut out = Vec::new();
            if looper {
                put(&mut out, &BC_ENTER_LOOPER);
            }
            threads.push(Thread {
                proc,
                tid,
                looper,
                calls_left,
                call: None,
                out,
                awaiting: Vec::new(),
                owed: Vec::new(),
                dead: false,
            });
        };
        thread(server, 1, true, 0);
        thread(server, 2, true, 0);
        for i in 0..clients {
            let p = b.open(2000 + i as i32, 10_000 + i as u32, 0, false);
            us.add_proc(p, 0x1_0000_0000 * (i as u64 + 2));
            thread(p, 1, true, 0);
            thread(p, 2, false, calls);
        }
        Sim {
            b,
            us,
            rng: Rng(seed),
            server,
            threads,
            next_code: 1,
            held: BTreeSet::new(),
            faults: None,
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
        if t.dead {
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
                    // What the write consumed, as the driver reported it.
                    let consumed = self.us.last_bwr(call).map_or(0, |b| b.write_consumed) as usize;
                    let t = &mut self.threads[i];
                    t.call = None;
                    let mut out = sent[consumed..].to_vec();
                    out.append(&mut t.out);
                    t.out = out;
                    if o.errno == 0 {
                        self.handle_read(i, &o.read);
                    }
                }
                Status::Abandoned => unreachable!(),
            }
            return true;
        }
        self.plan(i);
        let t = &self.threads[i];
        let reads = t.looper || !t.awaiting.is_empty() || !t.owed.is_empty();
        if !reads && t.out.is_empty() {
            return false;
        }
        let (proc, tid) = (t.proc, t.tid);
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
                    let t = &mut self.threads[i];
                    let o = t.owed.pop().unwrap();
                    let td = BinderTransactionData {
                        code: o.code,
                        ..Default::default()
                    };
                    put(&mut t.out, &BC_REPLY);
                    put(&mut t.out, &td);
                    put(&mut t.out, &BC_FREE_BUFFER);
                    put(&mut t.out, &o.buffer);
                }
            }
            return;
        }
        if t.calls_left > 0 && t.awaiting.is_empty() {
            let (proc, tid) = (t.proc, t.tid);
            let code = self.code();
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
            if self.rng.chance(40) {
                offsets.push(data.len() as u64);
                put(
                    &mut data,
                    &BinderFdObject {
                        hdr_type: BINDER_TYPE_FD,
                        fd_or_pad: 3,
                        ..Default::default()
                    },
                );
            }
            let offs: Vec<u8> = offsets.iter().flat_map(|o| o.to_le_bytes()).collect();
            let at = DATA + tid as u64 * 0x1_0000;
            self.us.poke(proc, at, &data);
            self.us.poke(proc, at + 0x8000, &offs);
            let td = BinderTransactionData {
                target: 0,
                code,
                flags: TF_ACCEPT_FDS,
                data_size: data.len() as u64,
                offsets_size: offs.len() as u64,
                data_buffer: at,
                data_offsets: at + 0x8000,
                ..Default::default()
            };
            let t = &mut self.threads[i];
            put(&mut t.out, &BC_TRANSACTION);
            put(&mut t.out, &td);
            t.awaiting.push(code);
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
                BR_NOOP | BR_SPAWN_LOOPER | BR_TRANSACTION_COMPLETE | BR_OK => {}
                BR_TRANSACTION | BR_TRANSACTION_SEC_CTX => {
                    let td: BinderTransactionData = read_pod(read, at).unwrap();
                    assert_eq!(td.flags & TF_ONE_WAY, 0);
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
                    let t = &mut self.threads[i];
                    assert!(t.awaiting.pop().is_some(), "failed reply to no call");
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
    /// `fault_steps` steps also have signals interrupt blocked reads and a
    /// client may die; then all faults stop.
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
                    self.us
                        .interrupt(pending[self.rng.below(pending.len() as u64) as usize]);
                }
            }
            if faulty && self.rng.chance(1) && self.threads.len() > 4 {
                let victim = self.threads.last().unwrap().proc;
                if !self.threads.last().unwrap().dead {
                    self.kill(victim);
                    self.check();
                }
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
                return;
            }
        }
    }

    /// Every call answered, nothing queued or in flight; then, once every
    /// process is gone, nothing left at all.
    fn finish(mut self) {
        for t in self.threads.iter().filter(|t| !t.dead) {
            assert_eq!(t.calls_left, 0);
            assert!(
                t.awaiting.is_empty(),
                "proc {} thread {}: calls unanswered",
                t.proc,
                t.tid
            );
            assert!(t.owed.is_empty() && t.out.is_empty());
        }
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
    }
}

#[test]
fn nothing_lost_under_random_failures() {
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
                Fault::CopyToUser => 0,
            };
            fr.below(1000) < pct
        }));
        s.faults = Some(Faults { readonly_pct: 5 });
        s.run(600);
        s.finish();
    }
}

#[test]
fn nothing_lost_when_any_one_operation_fails() {
    // For each scenario: a fault-free run, then the same run failing its
    // k-th fallible operation, for every k.
    for seed in 0..8 {
        let mut runs = 0;
        for k in 0.. {
            let fired = std::rc::Rc::new(std::cell::Cell::new(false));
            let mut s = Sim::new(seed, 1, 3);
            let (mut n, f) = (0, fired.clone());
            s.us.fault = Some(Box::new(move |_| {
                n += 1;
                let hit = n == k + 1;
                f.set(f.get() || hit);
                hit
            }));
            s.run(0);
            s.finish();
            runs += 1;
            if !fired.get() {
                break;
            }
        }
        assert!(runs > 50, "seed {seed}: only {runs} fault points");
    }
}
