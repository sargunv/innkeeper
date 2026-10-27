//! Userspace reimplementation of the Android binder driver's semantics.
//!
//! This is a pure state machine: it never issues a syscall. All access to a
//! process's memory, its receive buffer, and its file descriptors goes through
//! the [`Mem`] trait, so the whole driver is exercised by `cargo test` against
//! an in-memory fake (see the tests at the bottom and `bin/fuzz.rs`). The
//! syscall/seccomp binding that wires this to real Android processes lives in
//! `sys.rs`.
//!
//! The reference is the kernel's `drivers/android/binder.c` (and its Rust
//! sibling `drivers/android/binder/`). We reproduce: per-process handle tables
//! with smallest-free descriptor allocation, strong/weak node refcounts and the
//! INCREFS/ACQUIRE/RELEASE/DECREFS handshake, object translation inside
//! transactions (binder/handle, fd, fd-array, and scatter-gather PTR buffers),
//! synchronous reply and nested-call routing via a per-thread transaction
//! stack, per-node oneway queues, thread-pool spawn requests, death
//! notifications, the context manager, and sender pid/euid (virtualizable).

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::defs::*;

/// Verbose tracing of transaction failures, toggled by the syscall layer.
pub static DEBUG: AtomicBool = AtomicBool::new(false);
macro_rules! tlog {
    ($($a:tt)*) => {
        if DEBUG.load(Ordering::Relaxed) {
            eprint!("ubinder/broker: ");
            eprintln!($($a)*);
        }
    };
}

pub type ProcId = u32;
pub type NodeId = u64;
pub type TxnId = u64;

/// Access to process memory, receive buffers and fds. The real implementation
/// uses `process_vm_{readv,writev}`, a per-process memfd, and
/// `SECCOMP_IOCTL_NOTIF_ADDFD`/`pidfd_getfd`; tests use plain `Vec`s.
pub trait Mem {
    /// Read `len` bytes from `proc`'s address space at `addr`.
    fn read(&mut self, proc: ProcId, addr: u64, len: usize) -> Option<Vec<u8>>;
    /// Reserve `size` bytes in `proc`'s receive region and return the target
    /// user address of the start, or `None` on ENOSPC.
    fn alloc_recv(&mut self, proc: ProcId, size: usize, is_async: bool) -> Option<u64>;
    /// Write translated transaction bytes into a previously-reserved region.
    fn write_recv(&mut self, proc: ProcId, addr: u64, data: &[u8]);
    /// Release a receive region previously returned by `alloc_recv`.
    fn free_recv(&mut self, proc: ProcId, addr: u64);
    /// Capture `fd` out of `from` into the broker, returning an opaque token.
    /// The real implementation uses `pidfd_getfd`, so the fd survives even if
    /// the sender closes it before the target reads the transaction. Returns
    /// `None` if the fd is invalid.
    fn capture_fd(&mut self, from: ProcId, from_fd: u32) -> Option<u64>;
    /// Install a previously captured fd into `to`, returning the new fd number.
    /// Done at delivery time, when the target has an outstanding seccomp
    /// notification (`SECCOMP_IOCTL_NOTIF_ADDFD`). Returns `None` on failure.
    fn install_fd(&mut self, to: ProcId, token: u64) -> Option<u32>;
    /// Drop a captured fd that was never (or no longer) installed.
    fn drop_fd(&mut self, token: u64);
}

// Looper state flags (subset we act on).
const LOOPER_REGISTERED: u32 = 0x01;
const LOOPER_ENTERED: u32 = 0x02;
const LOOPER_EXITED: u32 = 0x04;
const LOOPER_WAITING: u32 = 0x10;

#[derive(Clone, Debug)]
enum Work {
    Transaction(TxnId),
    TransactionComplete,
    OnewaySpamSuspect,
    NodeChange(NodeId),
    DeadBinder(u64),         // cookie
    ClearDeathDone(u64),     // cookie
    DeadBinderAndClear(u64), // cookie
    ReturnError(u32),        // BR_* error command
}

#[derive(Default)]
#[allow(dead_code)] // kernel-fidelity fields; some are for freeze/debug paths
struct Thread {
    looper: u32,
    /// top of the synchronous call stack this thread is servicing/awaiting
    transaction_stack: Option<TxnId>,
    todo: VecDeque<Work>,
    process_todo: bool,
    looper_need_return: bool,
    is_dead: bool,
    /// set while the thread is parked in a blocking read
    waiting: bool,
    ee: BinderExtendedError,
}

#[allow(dead_code)] // kernel-fidelity fields; some are for freeze/debug paths
struct Node {
    id: NodeId,
    proc: ProcId,
    ptr: u64,
    cookie: u64,
    internal_strong: i32,
    local_strong: i32,
    local_weak: i32,
    has_strong_ref: bool,
    has_weak_ref: bool,
    pending_strong: bool,
    pending_weak: bool,
    tmp_refs: i32,
    accept_fds: bool,
    min_priority: u32,
    txn_security_ctx: bool,
    has_async_transaction: bool,
    async_todo: VecDeque<TxnId>,
    /// incoming refs: procs that hold a handle to this node
    refs: u32,
    work_queued: bool,
}

impl Node {
    fn wants_strong(&self) -> bool {
        self.internal_strong > 0 || self.local_strong > 0
    }
    fn wants_weak(&self) -> bool {
        self.refs > 0 || self.local_weak > 0 || self.tmp_refs > 0 || self.wants_strong()
    }
}

#[allow(dead_code)] // kernel-fidelity fields; some are for freeze/debug paths
struct Ref {
    node: NodeId,
    strong: i32,
    weak: i32,
    death_cookie: Option<u64>,
    death_queued: bool,
    freeze_cookie: Option<u64>,
}

#[derive(Default)]
#[allow(dead_code)] // kernel-fidelity fields; some are for freeze/debug paths
struct Process {
    pid: i32,
    /// Android uid the broker reports as this process's sender_euid.
    euid: u32,
    context: u8,
    dead: bool,
    frozen: bool,
    max_threads: u32,
    requested_threads: u32,
    requested_threads_started: u32,
    oneway_spam_detection: bool,
    threads: HashMap<u32, Thread>,
    todo: VecDeque<Work>,
    waiting_threads: VecDeque<u32>,
    nodes: HashMap<u64, NodeId>, // ptr -> node id
    refs_by_desc: BTreeMap<u32, Ref>,
    node_to_desc: HashMap<NodeId, u32>,
    delivered_death: Vec<u64>,
    /// free-async-buffer accounting (kernel: free_async_space; buffer_size/2)
    free_async_space: i64,
    async_buffer_size: i64,
}

struct Context {
    mgr_proc: Option<ProcId>,
    mgr_node: Option<NodeId>,
    mgr_uid: Option<u32>,
}

/// A thread that was blocked in a read and now has work to deliver. The
/// supervisor maps `(proc, tid)` back to a stored seccomp notification id and
/// re-runs the read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wakeup {
    pub proc: ProcId,
    pub tid: u32,
}

/// Result of a `BINDER_WRITE_READ`.
#[derive(Debug, Default)]
pub struct WriteRead {
    /// Bytes to copy into the caller's `read_buffer` (its `read_consumed`).
    pub read: Vec<u8>,
    /// Blocking read with nothing ready yet: the supervisor must defer the
    /// seccomp response until a later [`Wakeup`] names this thread.
    pub blocked: bool,
    /// Non-blocking read with nothing ready: return `-EAGAIN`.
    pub eagain: bool,
    /// Threads (possibly in other processes) that became runnable.
    pub wakeups: Vec<Wakeup>,
    /// errno for a failed write phase (0 = ok).
    pub write_err: i32,
}

pub struct Broker {
    contexts: [Context; 3],
    procs: HashMap<ProcId, Process>,
    nodes: HashMap<NodeId, Node>,
    txns: HashMap<TxnId, Txn>,
    next_proc: ProcId,
    next_node: NodeId,
    next_txn: TxnId,
    /// virtual-uid mode: report Android uids rather than the real caller uid
    pub virtual_uids: bool,
}

struct Txn {
    id: TxnId,
    from_proc: Option<ProcId>,
    from_tid: Option<u32>,
    to_proc: ProcId,
    to_thread: Option<u32>,
    target_node: Option<NodeId>,
    code: u32,
    flags: u32,
    sender_pid: i32,
    sender_euid: u32,
    buffer_addr: u64,
    data_size: usize,
    offsets_size: usize,
    buffer_total: usize,
    is_reply: bool,
    is_async: bool,
    from_parent: Option<TxnId>,
    to_parent: Option<TxnId>,
    /// nodes/refs to release if the transaction buffer is freed or dropped
    release: Vec<ReleaseItem>,
    /// fds captured from the sender, to be installed into the target at
    /// delivery: (offset into the recv buffer, capture token).
    fd_fixups: Vec<(usize, u64)>,
    /// target address of the appended security context string (0 if none);
    /// delivered via BR_TRANSACTION_SEC_CTX to nodes that requested it.
    security_ctx: u64,
}

#[derive(Clone)]
enum ReleaseItem {
    Node { node: NodeId, strong: bool },
    Handle { desc: u32, strong: bool },
}

const BINDER_VM_SIZE: i64 = 1024 * 1024; // libbinder maps ~1MB; async = half

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

impl Broker {
    pub fn new() -> Self {
        Broker {
            contexts: [
                Context {
                    mgr_proc: None,
                    mgr_node: None,
                    mgr_uid: None,
                },
                Context {
                    mgr_proc: None,
                    mgr_node: None,
                    mgr_uid: None,
                },
                Context {
                    mgr_proc: None,
                    mgr_node: None,
                    mgr_uid: None,
                },
            ],
            procs: HashMap::new(),
            nodes: HashMap::new(),
            txns: HashMap::new(),
            next_proc: 1,
            next_node: 1,
            next_txn: 1,
            virtual_uids: false,
        }
    }

    /// Open a binder device; `context` is 0=binder, 1=hwbinder, 2=vndbinder.
    pub fn open(&mut self, pid: i32, euid: u32, context: u8) -> ProcId {
        let id = self.next_proc;
        self.next_proc += 1;
        let mut p = Process {
            pid,
            euid,
            context,
            free_async_space: BINDER_VM_SIZE / 2,
            async_buffer_size: BINDER_VM_SIZE / 2,
            ..Default::default()
        };
        p.max_threads = 0;
        self.procs.insert(id, p);
        id
    }

    pub fn protocol_version(&self) -> i32 {
        BINDER_CURRENT_PROTOCOL_VERSION
    }

    pub fn set_max_threads(&mut self, proc: ProcId, n: u32) {
        if let Some(p) = self.procs.get_mut(&proc) {
            p.max_threads = n;
        }
    }

    pub fn enable_oneway_spam_detection(&mut self, proc: ProcId, enable: bool) {
        if let Some(p) = self.procs.get_mut(&proc) {
            p.oneway_spam_detection = enable;
        }
    }

    /// `BINDER_SET_CONTEXT_MGR(_EXT)`. `flat` is the optional flat_binder_object
    /// (EXT form); pass `None` for the plain form.
    pub fn set_context_mgr(
        &mut self,
        proc: ProcId,
        flat: Option<FlatBinderObject>,
    ) -> Result<(), i32> {
        let (ctx, euid) = {
            let p = self.procs.get(&proc).ok_or(-libc::EINVAL)?;
            (p.context as usize, p.euid)
        };
        if self.contexts[ctx].mgr_node.is_some() {
            return Err(-libc::EBUSY);
        }
        if let Some(want) = self.contexts[ctx].mgr_uid {
            if want != euid {
                return Err(-libc::EPERM);
            }
        } else {
            self.contexts[ctx].mgr_uid = Some(euid);
        }
        // The context manager's node has ptr/cookie 0 (plain) or from `flat`.
        let (ptr, cookie, flags) = match flat {
            Some(f) => (f.binder_or_handle, f.cookie, f.flags),
            None => (0, 0, 0),
        };
        let nid = self.new_node(proc, ptr, cookie, flags);
        {
            let n = self.nodes.get_mut(&nid).unwrap();
            n.local_strong += 1;
            n.local_weak += 1;
            n.has_strong_ref = true;
            n.has_weak_ref = true;
        }
        self.contexts[ctx].mgr_proc = Some(proc);
        self.contexts[ctx].mgr_node = Some(nid);
        Ok(())
    }

    fn new_node(&mut self, proc: ProcId, ptr: u64, cookie: u64, flags: u32) -> NodeId {
        // Reuse existing node for this ptr.
        if let Some(p) = self.procs.get(&proc)
            && let Some(&nid) = p.nodes.get(&ptr)
        {
            return nid;
        }
        let id = self.next_node;
        self.next_node += 1;
        let node = Node {
            id,
            proc,
            ptr,
            cookie,
            internal_strong: 0,
            local_strong: 0,
            local_weak: 0,
            has_strong_ref: false,
            has_weak_ref: false,
            pending_strong: false,
            pending_weak: false,
            tmp_refs: 0,
            accept_fds: flags & FLAT_BINDER_FLAG_ACCEPTS_FDS != 0,
            min_priority: flags & FLAT_BINDER_FLAG_PRIORITY_MASK,
            txn_security_ctx: flags & FLAT_BINDER_FLAG_TXN_SECURITY_CTX != 0,
            has_async_transaction: false,
            async_todo: VecDeque::new(),
            refs: 0,
            work_queued: false,
        };
        self.nodes.insert(id, node);
        self.procs.get_mut(&proc).unwrap().nodes.insert(ptr, id);
        id
    }

    /// Does this process have any work queued (proc-wide or on any thread)?
    pub fn proc_has_pending(&self, proc: ProcId) -> bool {
        match self.procs.get(&proc) {
            Some(p) => !p.todo.is_empty() || p.threads.values().any(|t| !t.todo.is_empty()),
            None => false,
        }
    }

    /// Would a Looper thread polling the binder fd see work right now? Mirrors
    /// the kernel's `binder_poll`/`binder_has_work`: thread-directed ready work
    /// (process_todo) counts always; process-wide work counts only if some
    /// thread is available to service it (no transaction stack, empty todo).
    /// Used to drive the poll eventfd so it doesn't busy-loop when proc work is
    /// queued but every thread is mid-transaction.
    pub fn proc_pollable(&self, proc: ProcId) -> bool {
        let Some(p) = self.procs.get(&proc) else {
            return false;
        };
        if p.threads.values().any(|t| t.process_todo) {
            return true;
        }
        !p.todo.is_empty()
            && p.threads
                .values()
                .any(|t| t.transaction_stack.is_none() && t.todo.is_empty())
    }

    // ---- handle table --------------------------------------------------

    fn ref_node_of(&self, proc: ProcId, desc: u32) -> Option<NodeId> {
        self.procs
            .get(&proc)?
            .refs_by_desc
            .get(&desc)
            .map(|r| r.node)
    }

    /// Smallest free descriptor (desc 0 reserved for the context manager).
    fn alloc_desc(p: &Process, is_ctx_mgr: bool) -> u32 {
        let mut desc = if is_ctx_mgr { 0 } else { 1 };
        for &d in p.refs_by_desc.keys() {
            if d > desc {
                break;
            }
            if d == desc {
                desc += 1;
            }
        }
        desc
    }

    /// Get (or create) a handle in `proc` for `node`, adjusting its strong/weak
    /// counts; returns the descriptor. The node's internal refcount is bumped
    /// the first time a strong/weak ref appears.
    fn inc_ref_for_node(&mut self, proc: ProcId, node: NodeId, strong: bool) -> u32 {
        let is_mgr = {
            let ctx = self.nodes[&node].proc; // owner; manager check below
            let _ = ctx;
            let octx = self.procs[&proc].context as usize;
            self.contexts[octx].mgr_node == Some(node)
        };
        let desc = {
            let p = self.procs.get(&proc).unwrap();
            if let Some(&d) = p.node_to_desc.get(&node) {
                d
            } else {
                let d = Self::alloc_desc(p, is_mgr);
                let p = self.procs.get_mut(&proc).unwrap();
                p.refs_by_desc.insert(
                    d,
                    Ref {
                        node,
                        strong: 0,
                        weak: 0,
                        death_cookie: None,
                        death_queued: false,
                        freeze_cookie: None,
                    },
                );
                p.node_to_desc.insert(node, d);
                self.nodes.get_mut(&node).unwrap().refs += 1;
                d
            }
        };
        let p = self.procs.get_mut(&proc).unwrap();
        let r = p.refs_by_desc.get_mut(&desc).unwrap();
        if strong {
            if r.strong == 0 {
                self.nodes.get_mut(&node).unwrap().internal_strong += 1;
            }
            r.strong += 1;
        } else {
            r.weak += 1;
        }
        desc
    }

    fn dec_ref_for_handle(&mut self, proc: ProcId, desc: u32, strong: bool) -> Result<(), i32> {
        let node;
        let delete;
        {
            let p = self.procs.get_mut(&proc).ok_or(-libc::EINVAL)?;
            let r = p.refs_by_desc.get_mut(&desc).ok_or(-libc::EINVAL)?;
            node = r.node;
            if strong {
                if r.strong == 0 {
                    return Err(-libc::EINVAL);
                }
                r.strong -= 1;
                if r.strong == 0 {
                    self.nodes.get_mut(&node).unwrap().internal_strong -= 1;
                }
            } else {
                if r.weak == 0 {
                    return Err(-libc::EINVAL);
                }
                r.weak -= 1;
            }
            let r = p.refs_by_desc.get(&desc).unwrap();
            delete = r.strong == 0 && r.weak == 0;
            if delete {
                p.refs_by_desc.remove(&desc);
                p.node_to_desc.remove(&node);
            }
        }
        if delete && let Some(n) = self.nodes.get_mut(&node) {
            n.refs = n.refs.saturating_sub(1);
        }
        // Node state change may need to be signalled to the owner.
        self.queue_node_change(node);
        Ok(())
    }

    fn update_ref_for_handle(
        &mut self,
        proc: ProcId,
        desc: u32,
        inc: bool,
        strong: bool,
    ) -> Result<(), i32> {
        if inc {
            // desc must already exist (userspace BC_INCREFS on a known handle)
            let node = self.ref_node_of(proc, desc).ok_or(-libc::EINVAL)?;
            let p = self.procs.get_mut(&proc).unwrap();
            let r = p.refs_by_desc.get_mut(&desc).unwrap();
            if strong {
                if r.strong == 0 {
                    self.nodes.get_mut(&node).unwrap().internal_strong += 1;
                }
                r.strong += 1;
            } else {
                r.weak += 1;
            }
            self.queue_node_change(node);
            Ok(())
        } else {
            self.dec_ref_for_handle(proc, desc, strong)
        }
    }

    // ---- node ref-change signalling (BINDER_WORK_NODE) -----------------

    /// Like [`Self::queue_node_change`] but, when the owner is the acting
    /// sender thread (object translation during a transaction), enqueue the
    /// ref handshake onto that thread's *deferred* todo so it is delivered
    /// together with the reply, exactly as the kernel does via
    /// `binder_inc_node_nilocked(..., &thread->todo)`.
    fn queue_node_change_deferred(&mut self, node: NodeId, tid: u32) {
        let (proc, need) = {
            let n = match self.nodes.get(&node) {
                Some(n) => n,
                None => return,
            };
            let need = (n.wants_weak() && !n.has_weak_ref)
                || (n.wants_strong() && !n.has_strong_ref)
                || (!n.wants_strong() && n.has_strong_ref)
                || (!n.wants_weak() && n.has_weak_ref);
            (n.proc, need)
        };
        if !need {
            return;
        }
        let n = self.nodes.get_mut(&node).unwrap();
        if n.work_queued {
            return;
        }
        n.work_queued = true;
        if let Some(p) = self.procs.get_mut(&proc) {
            let t = p.threads.entry(tid).or_default();
            t.todo.push_back(Work::NodeChange(node)); // deferred: no process_todo
        }
    }

    fn queue_node_change(&mut self, node: NodeId) {
        let (proc, need) = {
            let n = match self.nodes.get(&node) {
                Some(n) => n,
                None => return,
            };
            let want_strong = n.wants_strong();
            let want_weak = n.wants_weak();
            let need = (want_weak && !n.has_weak_ref)
                || (want_strong && !n.has_strong_ref)
                || (!want_strong && n.has_strong_ref)
                || (!want_weak && n.has_weak_ref);
            (n.proc, need)
        };
        if !need {
            return;
        }
        let n = self.nodes.get_mut(&node).unwrap();
        if n.work_queued {
            return;
        }
        n.work_queued = true;
        if let Some(p) = self.procs.get_mut(&proc) {
            p.todo.push_back(Work::NodeChange(node));
            Self::wakeup_proc(p);
        }
    }

    // ---- wakeups -------------------------------------------------------

    fn wakeup_proc(p: &mut Process) {
        // Move one waiting thread off the wait list so the supervisor re-runs
        // its read. If none are parked, a busy thread will pick it up.
        // (We just mark; the actual selection is in collect_wakeups.)
        let _ = p;
    }

    /// After mutating work lists, figure out which parked threads can now run.
    fn collect_wakeups(&mut self, out: &mut Vec<Wakeup>) {
        for (&pid, p) in self.procs.iter_mut() {
            if p.todo.is_empty() && !p.threads.values().any(|t| t.process_todo) {
                continue;
            }
            // thread-directed work
            for (&tid, t) in p.threads.iter_mut() {
                if t.waiting && (t.process_todo || !t.todo.is_empty()) {
                    t.waiting = false;
                    out.push(Wakeup { proc: pid, tid });
                }
            }
            // proc work: wake one waiting thread, if any is parked.
            if !p.todo.is_empty() {
                let parked = p
                    .waiting_threads
                    .iter()
                    .position(|&tid| p.threads.get(&tid).map(|t| t.waiting).unwrap_or(false));
                if let Some(pos) = parked {
                    let tid = p.waiting_threads.remove(pos).unwrap();
                    if let Some(t) = p.threads.get_mut(&tid) {
                        t.waiting = false;
                    }
                    if !out.iter().any(|w| w.proc == pid && w.tid == tid) {
                        out.push(Wakeup { proc: pid, tid });
                    }
                } else if !out.iter().any(|w| w.proc == pid) {
                    // No parked thread (e.g. a Looper/epoll process that polls
                    // the binder fd). Emit a proc-level wakeup (tid 0) so the
                    // syscall layer can signal that process's poll eventfd.
                    out.push(Wakeup { proc: pid, tid: 0 });
                }
            }
        }
    }

    fn thread(&mut self, proc: ProcId, tid: u32) -> &mut Thread {
        let p = self.procs.get_mut(&proc).unwrap();
        p.threads.entry(tid).or_default()
    }

    // ---- top-level BINDER_WRITE_READ -----------------------------------

    pub fn write_read(
        &mut self,
        proc: ProcId,
        tid: u32,
        write: &[u8],
        read_size: usize,
        non_block: bool,
        mem: &mut dyn Mem,
    ) -> WriteRead {
        let mut res = WriteRead::default();
        self.thread(proc, tid); // ensure exists
        if !write.is_empty()
            && let Err(e) = self.parse_write(proc, tid, write, mem, &mut res)
        {
            res.write_err = e;
        }
        if read_size > 0 {
            self.do_read(proc, tid, read_size, non_block, mem, &mut res);
        }
        self.collect_wakeups(&mut res.wakeups);
        res
    }

    // Parse the BC_* command stream (fully consumed).
    fn parse_write(
        &mut self,
        proc: ProcId,
        tid: u32,
        buf: &[u8],
        mem: &mut dyn Mem,
        res: &mut WriteRead,
    ) -> Result<(), i32> {
        let mut off = 0usize;
        while off + 4 <= buf.len() {
            let cmd: u32 = read_pod(buf, off).ok_or(libc::EINVAL)?;
            off += 4;
            match cmd {
                BC_ENTER_LOOPER => {
                    self.thread(proc, tid).looper |= LOOPER_ENTERED;
                }
                BC_REGISTER_LOOPER => {
                    let p = self.procs.get_mut(&proc).unwrap();
                    if p.requested_threads > 0 {
                        p.requested_threads -= 1;
                        p.requested_threads_started += 1;
                    }
                    p.threads.entry(tid).or_default().looper |= LOOPER_REGISTERED;
                }
                BC_EXIT_LOOPER => {
                    self.thread(proc, tid).looper |= LOOPER_EXITED;
                }
                BC_INCREFS | BC_ACQUIRE | BC_RELEASE | BC_DECREFS => {
                    let target: u32 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 4;
                    let strong = cmd == BC_ACQUIRE || cmd == BC_RELEASE;
                    let inc = cmd == BC_INCREFS || cmd == BC_ACQUIRE;
                    if inc && target == 0 {
                        // Acquire a ref on the context manager node at desc 0.
                        let ctx = self.procs[&proc].context as usize;
                        if let Some(node) = self.contexts[ctx].mgr_node {
                            self.inc_ref_for_node(proc, node, strong);
                            self.queue_node_change(node);
                            continue;
                        }
                    }
                    let _ = self.update_ref_for_handle(proc, target, inc, strong);
                }
                BC_INCREFS_DONE | BC_ACQUIRE_DONE => {
                    let node_ptr: u64 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 8;
                    let _cookie: u64 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 8;
                    if let Some(&nid) = self.procs[&proc].nodes.get(&node_ptr) {
                        let n = self.nodes.get_mut(&nid).unwrap();
                        if cmd == BC_ACQUIRE_DONE {
                            n.pending_strong = false;
                        } else {
                            n.pending_weak = false;
                        }
                    }
                }
                BC_FREE_BUFFER => {
                    let data_ptr: u64 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 8;
                    self.free_buffer(proc, data_ptr, mem);
                }
                BC_TRANSACTION | BC_REPLY => {
                    let td: BinderTransactionData = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += size_of::<BinderTransactionData>();
                    self.transaction(proc, tid, &td, cmd == BC_REPLY, 0, mem, res);
                }
                BC_TRANSACTION_SG | BC_REPLY_SG => {
                    let td: BinderTransactionData = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += size_of::<BinderTransactionData>();
                    let extra: u64 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 8;
                    self.transaction(proc, tid, &td, cmd == BC_REPLY_SG, extra as usize, mem, res);
                }
                BC_REQUEST_DEATH_NOTIFICATION | BC_CLEAR_DEATH_NOTIFICATION => {
                    let target: u32 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 4;
                    let cookie: u64 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 8;
                    self.death_notification(
                        proc,
                        tid,
                        target,
                        cookie,
                        cmd == BC_REQUEST_DEATH_NOTIFICATION,
                    );
                }
                BC_DEAD_BINDER_DONE => {
                    let cookie: u64 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 8;
                    if let Some(p) = self.procs.get_mut(&proc) {
                        p.delivered_death.retain(|&c| c != cookie);
                    }
                }
                BC_REQUEST_FREEZE_NOTIFICATION | BC_CLEAR_FREEZE_NOTIFICATION => {
                    let _target: u32 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 4;
                    let _cookie: u64 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 8;
                    // Accepted; frozen-state delivery is a no-op for now.
                }
                BC_FREEZE_NOTIFICATION_DONE => {
                    let _cookie: u64 = read_pod(buf, off).ok_or(libc::EINVAL)?;
                    off += 8;
                }
                BC_ACQUIRE_RESULT | BC_ATTEMPT_ACQUIRE => {
                    return Err(libc::EINVAL);
                }
                _ => {
                    return Err(libc::EINVAL);
                }
            }
        }
        Ok(())
    }

    // ---- the transaction path -----------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn transaction(
        &mut self,
        proc: ProcId,
        tid: u32,
        td: &BinderTransactionData,
        reply: bool,
        extra_buffers_size: usize,
        mem: &mut dyn Mem,
        res: &mut WriteRead,
    ) {
        let r = self.transaction_inner(proc, tid, td, reply, extra_buffers_size, mem, res);
        if let Err(err) = r {
            tlog!(
                "txn FAIL proc={proc} tid={tid} reply={reply} code={} handle={} dsz={} osz={} err={:#x}",
                td.code,
                td.target as u32,
                td.data_size,
                td.offsets_size,
                err
            );
            // Deliver the failure to the caller thread as a return error, and
            // if this was a reply, fail the original caller too.
            self.thread(proc, tid)
                .todo
                .push_back(Work::ReturnError(err));
            self.thread(proc, tid).process_todo = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn transaction_inner(
        &mut self,
        proc: ProcId,
        tid: u32,
        td: &BinderTransactionData,
        reply: bool,
        extra_buffers_size: usize,
        mem: &mut dyn Mem,
        _res: &mut WriteRead,
    ) -> Result<(), u32> {
        let oneway = td.flags & TF_ONE_WAY != 0;
        let (target_proc, target_node, mut target_thread, in_reply_to);

        if reply {
            // Pop our transaction stack; route the reply to the waiter.
            let top = self.thread(proc, tid).transaction_stack;
            let tx = top.ok_or(BR_FAILED_REPLY)?;
            let (from_proc, from_tid, to_parent) = {
                let t = self.txns.get(&tx).ok_or(BR_FAILED_REPLY)?;
                (t.from_proc, t.from_tid, t.to_parent)
            };
            self.thread(proc, tid).transaction_stack = to_parent;
            in_reply_to = Some(tx);
            target_proc = from_proc.ok_or(BR_DEAD_REPLY)?;
            target_thread = from_tid;
            target_node = None;
            if self.procs.get(&target_proc).map(|p| p.dead).unwrap_or(true) {
                return Err(BR_DEAD_REPLY);
            }
        } else {
            in_reply_to = None;
            let handle = td.target as u32;
            let node = if handle == 0 {
                let ctx = self.procs[&proc].context as usize;
                self.contexts[ctx].mgr_node.ok_or(BR_DEAD_REPLY)?
            } else {
                self.ref_node_of(proc, handle).ok_or(BR_FAILED_REPLY)?
            };
            let tp = self.nodes[&node].proc;
            if tp == proc {
                return Err(BR_FAILED_REPLY); // self transaction
            }
            if self.procs.get(&tp).map(|p| p.dead).unwrap_or(true) {
                return Err(BR_DEAD_REPLY);
            }
            target_proc = tp;
            target_node = Some(node);
            // Nested sync call: if we're already servicing a txn and the
            // target proc is in our stack's from-chain, route to that thread.
            target_thread = None;
            if !oneway {
                let mut cur = self.thread(proc, tid).transaction_stack;
                while let Some(txid) = cur {
                    let t = &self.txns[&txid];
                    if let Some(ftid) = t.from_tid
                        && t.from_proc == Some(target_proc)
                    {
                        target_thread = Some(ftid);
                        break;
                    }
                    cur = t.from_parent;
                }
            }
        }

        // Copy and translate the data buffer.
        let data_size = td.data_size as usize;
        let offsets_size = td.offsets_size as usize;
        let mut data = mem
            .read(proc, td.data_buffer, data_size)
            .ok_or(BR_FAILED_REPLY)?;
        let offsets_raw = mem
            .read(proc, td.data_offsets, offsets_size)
            .ok_or(BR_FAILED_REPLY)?;
        if !offsets_size.is_multiple_of(8) {
            return Err(BR_FAILED_REPLY);
        }

        // Scatter-gather region lives after data+offsets, 8-aligned.
        let off_start = align_up(data_size, 8);
        let sg_start = align_up(off_start + offsets_size, 8);
        let sg_end = sg_start + align_up(extra_buffers_size, 8);
        let mut sg_bytes = vec![0u8; align_up(extra_buffers_size, 8)];
        let mut sg_cursor = 0usize;

        // Nodes with FLAT_BINDER_FLAG_TXN_SECURITY_CTX (e.g. servicemanager)
        // receive the sender's SELinux context appended after the buffer. The
        // session hides SELinux, so synthesise a plausible, non-null context;
        // servicemanager's access checks are no-ops while SELinux is disabled,
        // but libbinder/servicemanager abort if the pointer is null.
        let want_secctx = !reply
            && target_node
                .map(|n| self.nodes[&n].txn_security_ctx)
                .unwrap_or(false);
        const SECCTX: &[u8] = b"u:r:su:s0\0";
        let secctx_off = sg_end;
        let buffer_total = if want_secctx {
            sg_end + align_up(SECCTX.len(), 8)
        } else {
            sg_end
        };

        let accept_fds = if reply {
            in_reply_to
                .and_then(|t| self.txns.get(&t))
                .map(|t| t.flags & TF_ACCEPT_FDS != 0)
                .unwrap_or(false)
        } else {
            target_node
                .map(|n| self.nodes[&n].accept_fds)
                .unwrap_or(false)
        };

        let mut release: Vec<ReleaseItem> = Vec::new();
        // fds captured from the sender; installed into the target at delivery.
        let mut fd_fixups: Vec<(usize, u64)> = Vec::new();
        let buffer_addr = mem
            .alloc_recv(target_proc, buffer_total, oneway)
            .ok_or(BR_FAILED_REPLY)?;

        // Walk offsets and translate each object in `data`.
        let n_off = offsets_size / 8;
        for i in 0..n_off {
            let obj_off =
                u64::from_le_bytes(offsets_raw[i * 8..i * 8 + 8].try_into().unwrap()) as usize;
            if obj_off + 4 > data.len() || !obj_off.is_multiple_of(4) {
                tlog!("xlate: bad obj_off={obj_off} dlen={}", data.len());
                self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                return Err(BR_FAILED_REPLY);
            }
            let hdr: u32 = read_pod(&data, obj_off).unwrap();
            tlog!("xlate obj[{i}] off={obj_off} hdr={hdr:#x}");
            match hdr {
                BINDER_TYPE_BINDER | BINDER_TYPE_WEAK_BINDER => {
                    let mut fbo: FlatBinderObject =
                        read_pod(&data, obj_off).ok_or(BR_FAILED_REPLY)?;
                    let strong = hdr == BINDER_TYPE_BINDER;
                    let node = self.new_node(proc, fbo.binder_or_handle, fbo.cookie, fbo.flags);
                    let desc = self.inc_ref_for_node(target_proc, node, strong);
                    // Owner is the sender: deliver the handshake with the reply.
                    self.queue_node_change_deferred(node, tid);
                    fbo.hdr_type = if strong {
                        BINDER_TYPE_HANDLE
                    } else {
                        BINDER_TYPE_WEAK_HANDLE
                    };
                    fbo.binder_or_handle = desc as u64;
                    fbo.cookie = 0;
                    data[obj_off..obj_off + size_of::<FlatBinderObject>()]
                        .copy_from_slice(pod_bytes(&fbo));
                    release.push(ReleaseItem::Handle { desc, strong });
                }
                BINDER_TYPE_HANDLE | BINDER_TYPE_WEAK_HANDLE => {
                    let mut fbo: FlatBinderObject =
                        read_pod(&data, obj_off).ok_or(BR_FAILED_REPLY)?;
                    let strong = hdr == BINDER_TYPE_HANDLE;
                    let h = fbo.binder_or_handle as u32;
                    let node = match self.ref_node_of(proc, h) {
                        Some(n) => n,
                        None => {
                            tlog!("xlate: handle {h} not found in proc={proc}");
                            self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                            return Err(BR_FAILED_REPLY);
                        }
                    };
                    let owner = self.nodes[&node].proc;
                    if owner == target_proc {
                        // Handle resolves to a node the target owns: hand back
                        // the local binder object.
                        let (ptr, cookie) = {
                            let n = &self.nodes[&node];
                            (n.ptr, n.cookie)
                        };
                        {
                            let n = self.nodes.get_mut(&node).unwrap();
                            if strong {
                                n.local_strong += 1
                            } else {
                                n.local_weak += 1
                            }
                        }
                        self.queue_node_change(node);
                        fbo.hdr_type = if strong {
                            BINDER_TYPE_BINDER
                        } else {
                            BINDER_TYPE_WEAK_BINDER
                        };
                        fbo.binder_or_handle = ptr;
                        fbo.cookie = cookie;
                        release.push(ReleaseItem::Node { node, strong });
                    } else {
                        let desc = self.inc_ref_for_node(target_proc, node, strong);
                        self.queue_node_change(node);
                        fbo.hdr_type = hdr;
                        fbo.binder_or_handle = desc as u64;
                        fbo.cookie = 0;
                        release.push(ReleaseItem::Handle { desc, strong });
                    }
                    data[obj_off..obj_off + size_of::<FlatBinderObject>()]
                        .copy_from_slice(pod_bytes(&fbo));
                }
                BINDER_TYPE_FD => {
                    if !accept_fds {
                        self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                        return Err(BR_FAILED_REPLY);
                    }
                    let mut fdo: BinderFdObject =
                        read_pod(&data, obj_off).ok_or(BR_FAILED_REPLY)?;
                    let src_fd = fdo.fd_or_pad as u32;
                    let token = match mem.capture_fd(proc, src_fd) {
                        Some(t) => t,
                        None => {
                            self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                            return Err(BR_FAILED_REPLY);
                        }
                    };
                    // fd field is the low 32 bits of the union at offset 8.
                    fdo.fd_or_pad = u32::MAX as u64; // placeholder; patched at delivery
                    data[obj_off..obj_off + size_of::<BinderFdObject>()]
                        .copy_from_slice(pod_bytes(&fdo));
                    fd_fixups.push((obj_off + 8, token));
                }
                BINDER_TYPE_PTR => {
                    let mut bbo: BinderBufferObject =
                        read_pod(&data, obj_off).ok_or(BR_FAILED_REPLY)?;
                    let len = bbo.length as usize;
                    if sg_cursor + align_up(len, 8) > sg_bytes.len() {
                        self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                        return Err(BR_FAILED_REPLY);
                    }
                    let payload = mem.read(proc, bbo.buffer, len).ok_or(BR_FAILED_REPLY)?;
                    sg_bytes[sg_cursor..sg_cursor + len].copy_from_slice(&payload);
                    // Rewrite pointer to the target user address of this sg slot.
                    let slot_user = buffer_addr + (sg_start + sg_cursor) as u64;
                    // HAS_PARENT fixup: patch the parent buffer's pointer slot.
                    if bbo.flags & BINDER_BUFFER_FLAG_HAS_PARENT != 0 {
                        let parent_idx = bbo.parent as usize;
                        if parent_idx < n_off {
                            let parent_obj_off = u64::from_le_bytes(
                                offsets_raw[parent_idx * 8..parent_idx * 8 + 8]
                                    .try_into()
                                    .unwrap(),
                            ) as usize;
                            let parent: BinderBufferObject =
                                read_pod(&data, parent_obj_off).ok_or(BR_FAILED_REPLY)?;
                            // Offset of the pointer slot within the parent's sg payload.
                            let patch_at_user = parent.buffer + bbo.parent_offset;
                            let parent_sg_off =
                                (parent.buffer - (buffer_addr + sg_start as u64)) as usize;
                            let within = (patch_at_user - parent.buffer) as usize;
                            let dst = parent_sg_off + within;
                            if dst + 8 <= sg_bytes.len() {
                                sg_bytes[dst..dst + 8].copy_from_slice(&slot_user.to_le_bytes());
                            }
                        }
                    }
                    bbo.buffer = slot_user;
                    data[obj_off..obj_off + size_of::<BinderBufferObject>()]
                        .copy_from_slice(pod_bytes(&bbo));
                    sg_cursor += align_up(len, 8);
                }
                BINDER_TYPE_FDA => {
                    if !accept_fds {
                        self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                        return Err(BR_FAILED_REPLY);
                    }
                    let fda: BinderFdArrayObject =
                        read_pod(&data, obj_off).ok_or(BR_FAILED_REPLY)?;
                    // Parent must be a PTR buffer already copied into sg_bytes.
                    let parent_idx = fda.parent as usize;
                    if parent_idx >= n_off {
                        self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                        return Err(BR_FAILED_REPLY);
                    }
                    let parent_obj_off = u64::from_le_bytes(
                        offsets_raw[parent_idx * 8..parent_idx * 8 + 8]
                            .try_into()
                            .unwrap(),
                    ) as usize;
                    let parent: BinderBufferObject =
                        read_pod(&data, parent_obj_off).ok_or(BR_FAILED_REPLY)?;
                    let parent_sg_off = (parent.buffer - (buffer_addr + sg_start as u64)) as usize;
                    let base = parent_sg_off + fda.parent_offset as usize;
                    let num = fda.num_fds as usize;
                    for k in 0..num {
                        let at = base + k * 4;
                        if at + 4 > sg_bytes.len() {
                            self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                            return Err(BR_FAILED_REPLY);
                        }
                        let src_fd = u32::from_le_bytes(sg_bytes[at..at + 4].try_into().unwrap());
                        let token = match mem.capture_fd(proc, src_fd) {
                            Some(t) => t,
                            None => {
                                self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                                return Err(BR_FAILED_REPLY);
                            }
                        };
                        sg_bytes[at..at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
                        fd_fixups.push((sg_start + at, token));
                    }
                }
                _ => {
                    self.unwind(target_proc, buffer_addr, &release, &fd_fixups, mem);
                    return Err(BR_FAILED_REPLY);
                }
            }
        }

        // Assemble the final target buffer: data | offsets | sg | [secctx].
        let mut out = vec![0u8; buffer_total];
        out[..data_size].copy_from_slice(&data[..data_size]);
        out[off_start..off_start + offsets_size].copy_from_slice(&offsets_raw);
        out[sg_start..sg_start + sg_bytes.len()].copy_from_slice(&sg_bytes);
        let security_ctx = if want_secctx {
            out[secctx_off..secctx_off + SECCTX.len()].copy_from_slice(SECCTX);
            buffer_addr + secctx_off as u64
        } else {
            0
        };
        mem.write_recv(target_proc, buffer_addr, &out);

        // Build the transaction record. In both real-uid and virtual-uid mode
        // the reported sender_euid is `Process::euid`; the difference is only
        // in how `euid` was chosen at `open()` (the real caller uid today, an
        // Android uid in a later single-host-uid mode).
        let sender_pid = self.procs[&proc].pid;
        let sender_euid = self.procs[&proc].euid;
        let txid = self.next_txn;
        self.next_txn += 1;
        let txn = Txn {
            id: txid,
            from_proc: if oneway || reply { None } else { Some(proc) },
            from_tid: if oneway || reply { None } else { Some(tid) },
            to_proc: target_proc,
            to_thread: target_thread,
            target_node,
            code: td.code,
            flags: td.flags,
            sender_pid,
            sender_euid,
            buffer_addr,
            data_size,
            offsets_size,
            buffer_total,
            is_reply: reply,
            is_async: oneway,
            from_parent: None,
            to_parent: in_reply_to,
            release,
            fd_fixups,
            security_ctx,
        };
        self.txns.insert(txid, txn);

        // async accounting
        if oneway && let Some(p) = self.procs.get_mut(&target_proc) {
            p.free_async_space -= buffer_total as i64;
        }

        if reply {
            // TRANSACTION_COMPLETE to us; deliver reply to the waiter thread.
            self.thread(proc, tid)
                .todo
                .push_back(Work::TransactionComplete);
            self.thread(proc, tid).process_todo = true;
            let twork = Work::Transaction(txid);
            // Pop the waiting caller's transaction stack (its outgoing call,
            // `in_reply_to`, is now answered), mirroring the kernel's
            // binder_pop_transaction_ilocked(target_thread, in_reply_to).
            let caller_parent = in_reply_to
                .and_then(|t| self.txns.get(&t))
                .and_then(|t| t.from_parent);
            if let Some(ftid) = target_thread {
                let tt = self
                    .procs
                    .get_mut(&target_proc)
                    .unwrap()
                    .threads
                    .entry(ftid)
                    .or_default();
                tt.transaction_stack = caller_parent;
                tt.todo.push_back(twork);
                tt.process_todo = true;
            } else if let Some(p) = self.procs.get_mut(&target_proc) {
                p.todo.push_back(twork);
            }
            if let Some(irt) = in_reply_to {
                self.txns.remove(&irt);
            }
        } else if !oneway {
            // Defer TRANSACTION_COMPLETE until reply; push onto our stack.
            {
                let cur = self.thread(proc, tid).transaction_stack;
                let t = self.txns.get_mut(&txid).unwrap();
                t.from_parent = cur;
            }
            self.thread(proc, tid).transaction_stack = Some(txid);
            // deferred tcomplete: enqueue to our own todo but don't set
            // process_todo so we don't consume it before blocking for the reply
            self.thread(proc, tid)
                .todo
                .push_back(Work::TransactionComplete);
            self.proc_transaction(txid, target_proc, target_thread);
        } else {
            // oneway: TRANSACTION_COMPLETE now; queue to node async or proc.
            // If the target's async buffer space is running low and spam
            // detection is on, flag it (kernel: debug_low_async_space_locked).
            let spam = {
                let p = &self.procs[&target_proc];
                p.oneway_spam_detection && p.free_async_space < p.async_buffer_size / 5
            };
            let tc = if spam {
                Work::OnewaySpamSuspect
            } else {
                Work::TransactionComplete
            };
            self.thread(proc, tid).todo.push_back(tc);
            self.thread(proc, tid).process_todo = true;
            self.proc_transaction(txid, target_proc, None);
        }
        Ok(())
    }

    fn proc_transaction(&mut self, txid: TxnId, target_proc: ProcId, target_thread: Option<u32>) {
        let (oneway, node) = {
            let t = &self.txns[&txid];
            (t.is_async, t.target_node)
        };
        // oneway serialisation per node
        if oneway && let Some(nid) = node {
            let busy = {
                let n = self.nodes.get_mut(&nid).unwrap();
                if n.has_async_transaction {
                    true
                } else {
                    n.has_async_transaction = true;
                    false
                }
            };
            if busy {
                self.nodes.get_mut(&nid).unwrap().async_todo.push_back(txid);
                return;
            }
        }
        let work = Work::Transaction(txid);
        if let Some(ftid) = target_thread {
            let t = self
                .procs
                .get_mut(&target_proc)
                .unwrap()
                .threads
                .entry(ftid)
                .or_default();
            t.todo.push_back(work);
            t.process_todo = true;
        } else {
            let p = self.procs.get_mut(&target_proc).unwrap();
            p.todo.push_back(work);
        }
    }

    fn unwind(
        &mut self,
        target_proc: ProcId,
        buffer_addr: u64,
        release: &[ReleaseItem],
        fd_fixups: &[(usize, u64)],
        mem: &mut dyn Mem,
    ) {
        for item in release {
            match item {
                ReleaseItem::Handle { desc, strong } => {
                    let _ = self.dec_ref_for_handle(target_proc, *desc, *strong);
                }
                ReleaseItem::Node { node, strong } => {
                    if let Some(n) = self.nodes.get_mut(node) {
                        if *strong {
                            n.local_strong -= 1
                        } else {
                            n.local_weak -= 1
                        }
                    }
                }
            }
        }
        // Fds were captured into the broker but never installed: drop them.
        for (_, token) in fd_fixups {
            mem.drop_fd(*token);
        }
        mem.free_recv(target_proc, buffer_addr);
    }

    fn free_buffer(&mut self, proc: ProcId, data_ptr: u64, mem: &mut dyn Mem) {
        // Find the transaction whose buffer starts at data_ptr in `proc`.
        let txid = self
            .txns
            .iter()
            .find(|(_, t)| t.to_proc == proc && t.buffer_addr == data_ptr)
            .map(|(&id, _)| id);
        let txid = match txid {
            Some(t) => t,
            None => {
                mem.free_recv(proc, data_ptr);
                return;
            }
        };
        let (release, is_async, is_reply, node, total) = {
            let t = &self.txns[&txid];
            (
                t.release.clone(),
                t.is_async,
                t.is_reply,
                t.target_node,
                t.buffer_total,
            )
        };
        // Release contained strong refs the *target* now drops (handles/nodes).
        for item in &release {
            match item {
                ReleaseItem::Handle { desc, strong } => {
                    let _ = self.dec_ref_for_handle(proc, *desc, *strong);
                }
                ReleaseItem::Node { node, strong } => {
                    if let Some(n) = self.nodes.get_mut(node) {
                        if *strong {
                            n.local_strong -= 1
                        } else {
                            n.local_weak -= 1
                        }
                        self.queue_node_change(*node);
                    }
                }
            }
        }
        // Installed fds are now owned by the target; the broker releases its
        // retained copies.
        let tokens: Vec<u64> = self.txns[&txid].fd_fixups.iter().map(|&(_, t)| t).collect();
        for token in tokens {
            mem.drop_fd(token);
        }
        mem.free_recv(proc, data_ptr);
        if is_async {
            if let Some(p) = self.procs.get_mut(&proc) {
                p.free_async_space += total as i64;
            }
            // Release the node's async slot; schedule the next queued oneway.
            if let Some(nid) = node {
                let next = {
                    let n = self.nodes.get_mut(&nid).unwrap();
                    n.async_todo.pop_front()
                };
                match next {
                    Some(nx) => self.deliver_async(nx, proc),
                    None => {
                        self.nodes.get_mut(&nid).unwrap().has_async_transaction = false;
                    }
                }
            }
        }
        // A synchronous *incoming* call is freed by the target before it sends
        // the reply, but the transaction record must outlive the buffer: the
        // target's BC_REPLY still needs it (it is on the target thread's
        // transaction stack). Keep it (the reply path removes it); just detach
        // the now-freed buffer so a later allocation at the same address can't
        // match this record. Replies and oneway transactions are done, so drop.
        if !is_reply && !is_async {
            if let Some(t) = self.txns.get_mut(&txid) {
                t.buffer_addr = u64::MAX;
                t.release.clear();
                t.fd_fixups.clear();
            }
        } else {
            self.txns.remove(&txid);
        }
    }

    fn deliver_async(&mut self, txid: TxnId, proc: ProcId) {
        let p = self.procs.get_mut(&proc).unwrap();
        p.todo.push_back(Work::Transaction(txid));
    }

    // ---- death notifications ------------------------------------------

    fn death_notification(
        &mut self,
        proc: ProcId,
        tid: u32,
        target: u32,
        cookie: u64,
        request: bool,
    ) {
        let node = match self.ref_node_of(proc, target) {
            Some(n) => n,
            None => return,
        };
        let owner_dead = self
            .procs
            .get(&self.nodes[&node].proc)
            .map(|p| p.dead)
            .unwrap_or(true);
        let p = self.procs.get_mut(&proc).unwrap();
        let r = p.refs_by_desc.get_mut(&target).unwrap();
        if request {
            if r.death_cookie.is_some() {
                return;
            }
            r.death_cookie = Some(cookie);
            if owner_dead {
                r.death_queued = true;
                p.todo.push_back(Work::DeadBinder(cookie));
            }
        } else {
            if r.death_cookie != Some(cookie) {
                return;
            }
            let was_queued = r.death_queued;
            r.death_cookie = None;
            r.death_queued = false;
            if was_queued {
                // Already-queued DEAD_BINDER becomes AND_CLEAR.
                if let Some(pos) = p
                    .todo
                    .iter()
                    .position(|w| matches!(w, Work::DeadBinder(c) if *c == cookie))
                {
                    p.todo[pos] = Work::DeadBinderAndClear(cookie);
                }
            } else {
                let t = p.threads.entry(tid).or_default();
                if t.looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0 {
                    t.todo.push_back(Work::ClearDeathDone(cookie));
                    t.process_todo = true;
                } else {
                    p.todo.push_back(Work::ClearDeathDone(cookie));
                }
            }
        }
    }

    /// Mark a process dead and fire death notifications to everyone holding a
    /// handle to one of its nodes. Returns wakeups.
    pub fn release_proc(&mut self, proc: ProcId) -> Vec<Wakeup> {
        if let Some(p) = self.procs.get_mut(&proc) {
            p.dead = true;
        }
        // Clear context-manager slot if this proc held it.
        for c in self.contexts.iter_mut() {
            if c.mgr_proc == Some(proc) {
                c.mgr_proc = None;
                c.mgr_node = None;
            }
        }
        // Collect this proc's nodes; for each incoming ref holding a death
        // request, queue DEAD_BINDER to that holder.
        let node_ids: Vec<NodeId> = self
            .nodes
            .values()
            .filter(|n| n.proc == proc)
            .map(|n| n.id)
            .collect();
        for nid in &node_ids {
            // mark node owner gone
            let holders: Vec<(ProcId, u32)> = self
                .procs
                .iter()
                .flat_map(|(&hp, hproc)| {
                    hproc
                        .refs_by_desc
                        .iter()
                        .filter(|(_, r)| r.node == *nid && r.death_cookie.is_some())
                        .map(move |(&d, _)| (hp, d))
                })
                .collect();
            for (hp, d) in holders {
                let p = self.procs.get_mut(&hp).unwrap();
                let r = p.refs_by_desc.get_mut(&d).unwrap();
                let cookie = r.death_cookie.unwrap();
                r.death_queued = true;
                p.todo.push_back(Work::DeadBinder(cookie));
            }
        }
        // Fail any sync transactions targeting this proc (the waiters get a
        // dead reply). Keep it simple: scan txns.
        let dead_txns: Vec<TxnId> = self
            .txns
            .values()
            .filter(|t| t.to_proc == proc && !t.is_async && t.from_proc.is_some())
            .map(|t| t.id)
            .collect();
        for txid in dead_txns {
            let (fp, ft) = {
                let t = &self.txns[&txid];
                (t.from_proc.unwrap(), t.from_tid.unwrap())
            };
            let t = self
                .procs
                .get_mut(&fp)
                .unwrap()
                .threads
                .entry(ft)
                .or_default();
            t.todo.push_back(Work::ReturnError(BR_DEAD_REPLY));
            t.process_todo = true;
            self.txns.remove(&txid);
        }
        let mut w = Vec::new();
        self.collect_wakeups(&mut w);
        w
    }

    pub fn thread_exit(&mut self, proc: ProcId, tid: u32) {
        if let Some(p) = self.procs.get_mut(&proc) {
            p.threads.remove(&tid);
            p.waiting_threads.retain(|&t| t != tid);
        }
    }

    pub fn get_extended_error(&mut self, proc: ProcId, tid: u32) -> BinderExtendedError {
        let t = self.thread(proc, tid);
        let ee = BinderExtendedError {
            id: BINDER_EE_ID,
            command: BR_OK,
            param: 0,
        };
        let prev = t.ee;
        t.ee = ee;
        if prev.command == 0 { ee } else { prev }
    }

    pub fn node_debug_info(&self, proc: ProcId, after_ptr: u64) -> BinderNodeDebugInfo {
        let mut best: Option<&Node> = None;
        if let Some(p) = self.procs.get(&proc) {
            for (&ptr, &nid) in p.nodes.iter() {
                if ptr > after_ptr {
                    let n = &self.nodes[&nid];
                    if best.map(|b| n.ptr < b.ptr).unwrap_or(true) {
                        best = Some(n);
                    }
                }
            }
        }
        match best {
            Some(n) => BinderNodeDebugInfo {
                ptr: n.ptr,
                cookie: n.cookie,
                has_strong_ref: n.has_strong_ref as u32,
                has_weak_ref: n.has_weak_ref as u32,
            },
            None => BinderNodeDebugInfo::default(),
        }
    }

    // ---- the read path -------------------------------------------------

    fn do_read(
        &mut self,
        proc: ProcId,
        tid: u32,
        read_size: usize,
        non_block: bool,
        mem: &mut dyn Mem,
        res: &mut WriteRead,
    ) {
        // Whether this thread may service process-wide work: it must not be
        // mid-call (no transaction stack) and have an empty thread queue.
        let (available, has_work) = {
            let p = self.procs.get(&proc).unwrap();
            let t = p.threads.get(&tid);
            let available = t
                .map(|t| t.transaction_stack.is_none() && t.todo.is_empty())
                .unwrap_or(true);
            let process_todo = t.map(|t| t.process_todo).unwrap_or(false);
            let has_work = process_todo || (available && !p.todo.is_empty());
            (available, has_work)
        };

        if !has_work {
            if non_block {
                res.eagain = true;
                return;
            }
            // Park this thread; a later enqueue produces a Wakeup for it.
            let p = self.procs.get_mut(&proc).unwrap();
            let t = p.threads.entry(tid).or_default();
            t.waiting = true;
            t.looper |= LOOPER_WAITING;
            if !p.waiting_threads.contains(&tid) {
                p.waiting_threads.push_back(tid);
            }
            res.blocked = true;
            return;
        }

        let mut out = Vec::with_capacity(read_size.min(256));
        // Kernel always leads with BR_NOOP on a fresh read.
        write_pod(&mut out, &BR_NOOP);

        loop {
            let work = self.next_work(proc, tid, available);
            let Some(work) = work else { break };
            // Stop if the next item won't fit (leave it queued).
            let need = self.work_size(&work);
            if out.len() + need > read_size {
                self.unget_work(proc, tid, work);
                break;
            }
            if !self.emit_work(proc, tid, work, &mut out, mem) {
                // emit asked us to stop after this item (DEAD_BINDER etc.)
                break;
            }
        }

        // BR_SPAWN_LOOPER if the pool needs growing.
        self.maybe_spawn_looper(proc, tid, &mut out, read_size);
        res.read = out;
    }

    fn next_work(&mut self, proc: ProcId, tid: u32, available: bool) -> Option<Work> {
        let p = self.procs.get_mut(&proc)?;
        if let Some(t) = p.threads.get_mut(&tid)
            && let Some(w) = t.todo.pop_front()
        {
            if t.todo.is_empty() {
                t.process_todo = false;
            }
            return Some(w);
        }
        // Process-wide work only if this thread is available for it.
        if available { p.todo.pop_front() } else { None }
    }

    fn unget_work(&mut self, proc: ProcId, tid: u32, work: Work) {
        let p = self.procs.get_mut(&proc).unwrap();
        // Put it back at the head of the thread's queue.
        let t = p.threads.entry(tid).or_default();
        t.todo.push_front(work);
        t.process_todo = true;
    }

    fn work_size(&self, work: &Work) -> usize {
        match work {
            Work::Transaction(txid) => {
                let t = &self.txns[txid];
                let sec = t
                    .target_node
                    .map(|n| self.nodes[&n].txn_security_ctx)
                    .unwrap_or(false);
                4 + if sec {
                    size_of::<BinderTransactionDataSecctx>()
                } else {
                    size_of::<BinderTransactionData>()
                }
            }
            Work::TransactionComplete | Work::OnewaySpamSuspect => 4,
            Work::NodeChange(_) => 4 * (4 + 4 * size_of::<u64>()), // up to 4 cmds
            Work::DeadBinder(_) | Work::ClearDeathDone(_) | Work::DeadBinderAndClear(_) => 4 + 8,
            Work::ReturnError(_) => 4,
        }
    }

    // returns false if the caller should stop reading after this item
    fn emit_work(
        &mut self,
        proc: ProcId,
        tid: u32,
        work: Work,
        out: &mut Vec<u8>,
        mem: &mut dyn Mem,
    ) -> bool {
        match work {
            Work::TransactionComplete => {
                write_pod(out, &BR_TRANSACTION_COMPLETE);
                true
            }
            Work::OnewaySpamSuspect => {
                write_pod(out, &BR_ONEWAY_SPAM_SUSPECT);
                true
            }
            Work::ReturnError(cmd) => {
                write_pod(out, &cmd);
                true
            }
            Work::ClearDeathDone(cookie) => {
                write_pod(out, &BR_CLEAR_DEATH_NOTIFICATION_DONE);
                write_pod(out, &cookie);
                true
            }
            Work::DeadBinder(cookie) | Work::DeadBinderAndClear(cookie) => {
                write_pod(out, &BR_DEAD_BINDER);
                write_pod(out, &cookie);
                if let Some(p) = self.procs.get_mut(&proc) {
                    p.delivered_death.push(cookie);
                }
                false // DEAD_BINDER can cause transactions; stop here
            }
            Work::NodeChange(nid) => {
                self.emit_node_change(nid, out);
                true
            }
            Work::Transaction(txid) => {
                self.emit_transaction(proc, tid, txid, out, mem);
                true
            }
        }
    }

    fn emit_node_change(&mut self, nid: NodeId, out: &mut Vec<u8>) {
        let (ptr, cookie, incs, acq, rel, dec) = {
            let n = match self.nodes.get_mut(&nid) {
                Some(n) => n,
                None => return,
            };
            n.work_queued = false;
            let want_strong = n.wants_strong();
            let want_weak = n.wants_weak();
            let mut incs = false;
            let mut acq = false;
            let mut rel = false;
            let mut dec = false;
            if want_weak && !n.has_weak_ref {
                n.has_weak_ref = true;
                n.pending_weak = true;
                n.local_weak += 1;
                incs = true;
            }
            if want_strong && !n.has_strong_ref {
                n.has_strong_ref = true;
                n.pending_strong = true;
                n.local_strong += 1;
                acq = true;
            }
            if !want_strong && n.has_strong_ref {
                n.has_strong_ref = false;
                rel = true;
            }
            if !want_weak && n.has_weak_ref {
                n.has_weak_ref = false;
                dec = true;
            }
            (n.ptr, n.cookie, incs, acq, rel, dec)
        };
        let put = |cmd: u32, out: &mut Vec<u8>| {
            write_pod(out, &cmd);
            write_pod(out, &ptr);
            write_pod(out, &cookie);
        };
        if incs {
            put(BR_INCREFS, out);
        }
        if acq {
            put(BR_ACQUIRE, out);
        }
        if rel {
            put(BR_RELEASE, out);
        }
        if dec {
            put(BR_DECREFS, out);
        }
    }

    fn emit_transaction(
        &mut self,
        proc: ProcId,
        tid: u32,
        txid: TxnId,
        out: &mut Vec<u8>,
        mem: &mut dyn Mem,
    ) {
        let (is_reply, target_node, sec) = {
            let t = &self.txns[&txid];
            let sec = t
                .target_node
                .map(|n| self.nodes[&n].txn_security_ctx)
                .unwrap_or(false);
            (t.is_reply, t.target_node, sec)
        };
        // Install captured fds into the target now that it has an outstanding
        // notification, and patch the recv buffer with the new fd numbers
        // (kernel: binder_apply_fd_fixups runs in the target's context).
        let fixups: Vec<(usize, u64)> = self.txns[&txid].fd_fixups.clone();
        if !fixups.is_empty() {
            let base = self.txns[&txid].buffer_addr;
            for (off, token) in fixups {
                if let Some(fd) = mem.install_fd(proc, token) {
                    mem.write_recv(proc, base + off as u64, &fd.to_le_bytes());
                }
                mem.drop_fd(token);
            }
            self.txns.get_mut(&txid).unwrap().fd_fixups.clear();
        }
        let t = &self.txns[&txid];
        let mut td = BinderTransactionData::default();
        if let Some(nid) = target_node {
            let n = &self.nodes[&nid];
            td.target = n.ptr;
            td.cookie = n.cookie;
        }
        td.code = t.code;
        td.flags = t.flags;
        td.sender_pid = t.sender_pid;
        td.sender_euid = t.sender_euid;
        td.data_size = t.data_size as u64;
        td.offsets_size = t.offsets_size as u64;
        td.data_buffer = t.buffer_addr;
        td.data_offsets = t.buffer_addr + align_up(t.data_size, 8) as u64;

        let cmd = if is_reply {
            BR_REPLY
        } else if sec {
            BR_TRANSACTION_SEC_CTX
        } else {
            BR_TRANSACTION
        };
        let secctx_addr = t.security_ctx;
        write_pod(out, &cmd);
        if cmd == BR_TRANSACTION_SEC_CTX {
            let sc = BinderTransactionDataSecctx {
                transaction_data: td,
                secctx: secctx_addr,
            };
            write_pod(out, &sc);
        } else {
            write_pod(out, &td);
        }

        // Sync, non-reply: push onto the receiver's stack so its reply routes
        // back to the caller.
        if !is_reply && t.flags & TF_ONE_WAY == 0 {
            let cur = self.thread(proc, tid).transaction_stack;
            {
                let tm = self.txns.get_mut(&txid).unwrap();
                tm.to_parent = cur;
                tm.to_thread = Some(tid);
            }
            self.thread(proc, tid).transaction_stack = Some(txid);
        } else {
            // reply or oneway: the transaction record is done being routed;
            // the buffer is freed later by BC_FREE_BUFFER.
        }
    }

    fn maybe_spawn_looper(&mut self, proc: ProcId, tid: u32, out: &mut Vec<u8>, read_size: usize) {
        let spawn = {
            let p = self.procs.get(&proc).unwrap();
            let t = p.threads.get(&tid);
            let registered = t
                .map(|t| t.looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0)
                .unwrap_or(false);
            p.requested_threads == 0
                && p.waiting_threads.is_empty()
                && p.requested_threads_started < p.max_threads
                && registered
        };
        if spawn && out.len() + 4 <= read_size {
            self.procs.get_mut(&proc).unwrap().requested_threads += 1;
            write_pod(out, &BR_SPAWN_LOOPER);
        }
    }

    // ---- introspection for tests --------------------------------------

    #[cfg(test)]
    pub(crate) fn ref_counts(&self, proc: ProcId, desc: u32) -> Option<(i32, i32)> {
        self.procs
            .get(&proc)?
            .refs_by_desc
            .get(&desc)
            .map(|r| (r.strong, r.weak))
    }

    #[cfg(test)]
    #[allow(dead_code)]
    pub(crate) fn node_for_desc(&self, proc: ProcId, desc: u32) -> Option<NodeId> {
        self.ref_node_of(proc, desc)
    }
}
