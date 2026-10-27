//! Userspace reimplementation of the Android binder driver's semantics.
//!
//! This is a pure state machine: it never issues a syscall. All access to a
//! process's memory, its receive buffer, and its file descriptors goes through
//! the [`Mem`] trait, so the whole driver is exercised by `cargo test` against
//! an in-memory fake (see `tests.rs` and `bin/fuzz.rs`). The syscall/seccomp
//! binding that wires this to real Android processes lives in `sys.rs`.
//!
//! The reference is the kernel's `drivers/android/binder.c`; function names in
//! comments refer to it. The structure deliberately follows it:
//!
//!  * work lists: per-process `todo`, per-thread `todo` with the
//!    `process_todo` flag (deferred work such as the TRANSACTION_COMPLETE of a
//!    synchronous call is queued without it), per-node `async_todo`, and the
//!    per-process `delivered_death` list;
//!  * thread selection: a process-wide transaction goes straight to the first
//!    thread in `waiting_threads` (`binder_select_thread_ilocked`), else to the
//!    process `todo`, waking poll threads; only looper threads with an empty
//!    stack and queue take process work (`binder_available_for_proc_work`);
//!  * transactions and buffers are separate objects: a buffer outlives its
//!    transaction until `BC_FREE_BUFFER`, and owns the references taken while
//!    translating the objects in it (released by
//!    `binder_transaction_buffer_release`);
//!  * node refcounts (`internal_strong`, `local_strong/weak`, `tmp_refs`, the
//!    set of incoming refs) and the INCREFS/ACQUIRE/RELEASE/DECREFS handshake
//!    with `BC_*_DONE` are the kernel's;
//!  * death notifications move through the same states (DEAD_BINDER,
//!    DEAD_BINDER_AND_CLEAR, CLEAR_DEATH_NOTIFICATION, delivered).
//!
//! Blocking is cooperative: a read with no work returns [`ReadOutcome::Blocked`]
//! and marks the thread parked; any later enqueue that would have woken the
//! kernel thread records a [`Wakeup`] which the syscall layer drains with
//! [`Broker::take_wakeups`], re-running that thread's read.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::sync::atomic::Ordering;

use crate::defs::*;
use crate::{DEBUG, log};

macro_rules! tlog {
    ($($a:tt)*) => {
        if DEBUG.load(Ordering::Relaxed) {
            log(format_args!("broker: {}", format_args!($($a)*)));
        }
    };
}

pub type ProcId = u32;
pub type NodeId = u64;
pub type TxnId = u64;
type DeathId = u64;

/// Access to process memory, receive buffers and fds. The real implementation
/// (`sys.rs`) uses each process's pinned `/proc/<pid>/mem`, a per-process
/// memfd, and `SECCOMP_IOCTL_NOTIF_ADDFD`/`pidfd_getfd`; tests use an
/// in-memory fake.
pub trait Mem {
    /// Read `len` bytes from `proc`'s address space at `addr`.
    fn read(&mut self, proc: ProcId, addr: u64, len: usize) -> Option<Vec<u8>>;
    /// Size of `proc`'s mapped receive region (0 if not mapped yet).
    fn recv_size(&mut self, proc: ProcId) -> usize;
    /// Reserve `size` bytes in `proc`'s receive region and return the target
    /// user address of the start, or `None` on ENOSPC.
    fn alloc_recv(&mut self, proc: ProcId, size: usize, is_async: bool) -> Option<u64>;
    /// Write translated transaction bytes into a previously-reserved region.
    fn write_recv(&mut self, proc: ProcId, addr: u64, data: &[u8]);
    /// Release a receive region previously returned by `alloc_recv`.
    fn free_recv(&mut self, proc: ProcId, addr: u64);
    /// Capture `fd` out of `from` into the broker, returning an opaque token.
    /// The real implementation uses `pidfd_getfd`, so the fd survives even if
    /// the sender closes it before the target reads the transaction.
    fn capture_fd(&mut self, from: ProcId, from_fd: u32) -> Option<u64>;
    /// Install a previously captured fd into `to` (which is inside its own
    /// `BINDER_WRITE_READ`), returning the new fd number.
    fn install_fd(&mut self, to: ProcId, token: u64) -> Option<u32>;
    /// Drop a captured fd that was never (or no longer) installed.
    fn drop_fd(&mut self, token: u64);
}

// Looper state flags (kernel BINDER_LOOPER_STATE_*).
const LOOPER_REGISTERED: u32 = 0x01;
const LOOPER_ENTERED: u32 = 0x02;
const LOOPER_EXITED: u32 = 0x04;
const LOOPER_INVALID: u32 = 0x08;
const LOOPER_WAITING: u32 = 0x10;
const LOOPER_POLL: u32 = 0x20;

/// Size of the largest single item the read loop emits
/// (`binder_transaction_data_secctx` plus its command word), which the kernel
/// requires free before dequeuing any work.
const MIN_READ_SPACE: usize = 4 + size_of::<BinderTransactionDataSecctx>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Work {
    Transaction(TxnId),
    TransactionComplete,
    OnewaySpamSuspect,
    ReturnError,
    ReplyError,
    Node(NodeId),
    Death(DeathId),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DeathKind {
    DeadBinder,
    DeadBinderAndClear,
    Clear,
}

struct Death {
    cookie: u64,
    kind: DeathKind,
    /// The work item is on some list (a todo list or `delivered_death`).
    queued: bool,
}

#[derive(Default)]
struct Thread {
    looper: u32,
    todo: VecDeque<Work>,
    process_todo: bool,
    looper_need_return: bool,
    transaction_stack: Option<TxnId>,
    return_error: u32,
    reply_error: u32,
    is_dead: bool,
    /// Parked in a blocking read (the syscall layer holds its notification).
    parked: bool,
    ee: BinderExtendedError,
}

struct Node {
    /// Owner, `None` once the owner died (`node->proc == NULL`).
    proc: Option<ProcId>,
    ptr: u64,
    cookie: u64,
    internal_strong: i32,
    local_strong: i32,
    local_weak: i32,
    tmp_refs: i32,
    has_strong_ref: bool,
    has_weak_ref: bool,
    pending_strong: bool,
    pending_weak: bool,
    accept_fds: bool,
    txn_security_ctx: bool,
    has_async_transaction: bool,
    async_todo: VecDeque<TxnId>,
    /// Processes holding a ref (a process holds at most one per node).
    refs: BTreeSet<ProcId>,
    /// The node's work item is queued on some list.
    work_queued: bool,
}

struct Ref {
    node: NodeId,
    strong: i32,
    weak: i32,
    death: Option<DeathId>,
}

#[derive(Default)]
struct Process {
    /// pid as seen inside the Android pid namespace (reported as sender_pid).
    ns_pid: i32,
    euid: u32,
    context: u8,
    dead: bool,
    max_threads: u32,
    requested_threads: u32,
    requested_threads_started: u32,
    oneway_spam_detection: bool,
    threads: BTreeMap<u32, Thread>,
    todo: VecDeque<Work>,
    waiting_threads: VecDeque<u32>,
    nodes: HashMap<u64, NodeId>,
    refs_by_desc: BTreeMap<u32, Ref>,
    node_to_desc: HashMap<NodeId, u32>,
    delivered_death: Vec<DeathId>,
    free_async_space: i64,
}

struct Context {
    mgr_node: Option<NodeId>,
    mgr_uid: Option<u32>,
}

/// A thread that was parked in a read and now has work (or must return). The
/// supervisor maps `(proc, tid)` back to its stored seccomp notification and
/// re-runs the read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Wakeup {
    pub proc: ProcId,
    pub tid: u32,
}

/// Result of a `BINDER_WRITE_READ`.
#[derive(Debug, Default)]
pub struct WriteRead {
    /// Bytes to copy into the caller's `read_buffer` at `read_consumed`.
    pub read: Vec<u8>,
    /// Blocking read with nothing ready yet: the supervisor must defer the
    /// seccomp response until a later [`Wakeup`] names this thread.
    pub blocked: bool,
    /// Non-blocking read with nothing ready: return `-EAGAIN`.
    pub eagain: bool,
    /// Bytes of the write buffer consumed.
    pub write_consumed: usize,
    /// errno for a failed write phase (0 = ok). The read phase is skipped.
    pub write_err: i32,
    /// This thread just became a polling (Looper) thread: the supervisor makes
    /// the binder fd pollable.
    pub became_poll: bool,
    /// Parked threads made runnable by this call (drained from the broker).
    pub wakeups: Vec<Wakeup>,
}

enum ReadOutcome {
    Data(Vec<u8>),
    Blocked,
    Again,
}

struct Txn {
    /// Synchronous caller (`t->from`); `None` for oneway, replies, and once
    /// the caller thread died.
    from: Option<(ProcId, u32)>,
    from_parent: Option<TxnId>,
    to_proc: Option<ProcId>,
    to_thread: Option<u32>,
    to_parent: Option<TxnId>,
    buffer: Option<(ProcId, u64)>,
    code: u32,
    flags: u32,
    sender_euid: u32,
}

#[derive(Clone, Copy)]
enum Obj {
    /// A node local to the buffer owner (BINDER_TYPE_(WEAK_)BINDER).
    Node { node: NodeId, strong: bool },
    /// A handle in the buffer owner's table (BINDER_TYPE_(WEAK_)HANDLE).
    Handle { desc: u32, strong: bool },
}

struct Buffer {
    data_size: usize,
    offsets_size: usize,
    total: usize,
    is_async: bool,
    target_node: Option<NodeId>,
    transaction: Option<TxnId>,
    allow_user_free: bool,
    objects: Vec<Obj>,
    /// Captured fds still to install: (offset of the fd in the buffer, token).
    fd_fixups: Vec<(usize, u64)>,
    /// Captured fds already installed (the broker's copies to drop on free).
    secctx: u64,
}

pub struct Broker {
    contexts: [Context; 3],
    procs: HashMap<ProcId, Process>,
    nodes: HashMap<NodeId, Node>,
    txns: HashMap<TxnId, Txn>,
    buffers: HashMap<(ProcId, u64), Buffer>,
    deaths: HashMap<DeathId, Death>,
    next_proc: ProcId,
    next_id: u64,
    wakeups: Vec<Wakeup>,
    poll_dirty: BTreeSet<ProcId>,
}

impl Default for Broker {
    fn default() -> Self {
        Self::new()
    }
}

fn ctx_new() -> Context {
    Context {
        mgr_node: None,
        mgr_uid: None,
    }
}

impl Broker {
    pub fn new() -> Self {
        Broker {
            contexts: [ctx_new(), ctx_new(), ctx_new()],
            procs: HashMap::new(),
            nodes: HashMap::new(),
            txns: HashMap::new(),
            buffers: HashMap::new(),
            deaths: HashMap::new(),
            next_proc: 1,
            next_id: 1,
            wakeups: Vec::new(),
            poll_dirty: BTreeSet::new(),
        }
    }

    fn id(&mut self) -> u64 {
        let i = self.next_id;
        self.next_id += 1;
        i
    }

    /// Open a binder device for a process with pid `ns_pid` (in Android's pid
    /// namespace) and `euid`; `context` is 0=binder, 1=hwbinder,
    /// 2=vndbinder.
    pub fn open(&mut self, ns_pid: i32, euid: u32, context: u8) -> ProcId {
        let id = self.next_proc;
        self.next_proc += 1;
        self.procs.insert(
            id,
            Process {
                ns_pid,
                euid,
                context: context.min(2),
                free_async_space: -1, // set from the mapping on first use
                ..Default::default()
            },
        );
        id
    }

    pub fn protocol_version(&self) -> i32 {
        BINDER_CURRENT_PROTOCOL_VERSION
    }

    pub fn proc_exists(&self, proc: ProcId) -> bool {
        self.procs.get(&proc).map(|p| !p.dead).unwrap_or(false)
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

    /// Drain the parked threads that became runnable.
    pub fn take_wakeups(&mut self) -> Vec<Wakeup> {
        std::mem::take(&mut self.wakeups)
    }

    /// Drain the processes whose poll readiness may have changed.
    pub fn take_poll_dirty(&mut self) -> Vec<ProcId> {
        std::mem::take(&mut self.poll_dirty).into_iter().collect()
    }

    /// `BINDER_SET_CONTEXT_MGR(_EXT)`. `flat` is the optional flat_binder_object
    /// (EXT form); pass `None` for the plain form. Returns a negative errno.
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
        match self.contexts[ctx].mgr_uid {
            Some(want) if want != euid => return Err(-libc::EPERM),
            Some(_) => {}
            None => self.contexts[ctx].mgr_uid = Some(euid),
        }
        let (ptr, cookie, flags) = match flat {
            Some(f) => (f.binder_or_handle, f.cookie, f.flags),
            None => (0, 0, 0),
        };
        let nid = self.get_or_new_node(proc, ptr, cookie, flags);
        let n = self.nodes.get_mut(&nid).unwrap();
        n.local_weak += 1;
        n.local_strong += 1;
        n.has_strong_ref = true;
        n.has_weak_ref = true;
        self.contexts[ctx].mgr_node = Some(nid);
        Ok(())
    }

    fn get_or_new_node(&mut self, proc: ProcId, ptr: u64, cookie: u64, flags: u32) -> NodeId {
        if let Some(&nid) = self.procs[&proc].nodes.get(&ptr) {
            return nid;
        }
        let id = self.id();
        self.nodes.insert(
            id,
            Node {
                proc: Some(proc),
                ptr,
                cookie,
                internal_strong: 0,
                local_strong: 0,
                local_weak: 0,
                tmp_refs: 0,
                has_strong_ref: false,
                has_weak_ref: false,
                pending_strong: false,
                pending_weak: false,
                accept_fds: flags & FLAT_BINDER_FLAG_ACCEPTS_FDS != 0,
                txn_security_ctx: flags & FLAT_BINDER_FLAG_TXN_SECURITY_CTX != 0,
                has_async_transaction: false,
                async_todo: VecDeque::new(),
                refs: BTreeSet::new(),
                work_queued: false,
            },
        );
        self.procs.get_mut(&proc).unwrap().nodes.insert(ptr, id);
        id
    }

    // ---- threads, queues and wakeups ---------------------------------------

    fn thread_mut(&mut self, proc: ProcId, tid: u32) -> &mut Thread {
        self.procs
            .get_mut(&proc)
            .unwrap()
            .threads
            .entry(tid)
            .or_insert_with(|| Thread {
                looper_need_return: true,
                return_error: BR_OK,
                reply_error: BR_OK,
                ee: BinderExtendedError {
                    id: 0,
                    command: BR_OK,
                    param: 0,
                },
                ..Default::default()
            })
    }

    fn available_for_proc_work(t: &Thread) -> bool {
        t.transaction_stack.is_none()
            && t.todo.is_empty()
            && t.looper & (LOOPER_ENTERED | LOOPER_REGISTERED) != 0
    }

    fn has_work(p: &Process, t: &Thread, do_proc_work: bool) -> bool {
        t.process_todo || t.looper_need_return || (do_proc_work && !p.todo.is_empty())
    }

    /// Wake a parked thread so the syscall layer re-runs its read.
    fn wake_thread(&mut self, proc: ProcId, tid: u32) {
        if let Some(p) = self.procs.get_mut(&proc)
            && let Some(t) = p.threads.get_mut(&tid)
            && t.parked
        {
            t.parked = false;
            p.waiting_threads.retain(|&x| x != tid);
            self.wakeups.push(Wakeup { proc, tid });
        }
        self.poll_dirty.insert(proc);
    }

    /// A thread whose read blocked won't wait after all (the syscall layer
    /// failed the ioctl): undo its parking, so no work is routed to a thread
    /// that isn't reading. Work already queued on it stays for its next read.
    pub fn cancel_wait(&mut self, proc: ProcId, tid: u32) {
        if let Some(p) = self.procs.get_mut(&proc)
            && let Some(t) = p.threads.get_mut(&tid)
        {
            t.parked = false;
            t.looper &= !LOOPER_WAITING;
            p.waiting_threads.retain(|&x| x != tid);
        }
    }

    /// `binder_select_thread_ilocked`: pop the first thread waiting for
    /// process work.
    fn select_thread(&mut self, proc: ProcId) -> Option<u32> {
        let p = self.procs.get_mut(&proc)?;
        while let Some(tid) = p.waiting_threads.pop_front() {
            if p.threads.get(&tid).map(|t| t.parked).unwrap_or(false) {
                return Some(tid);
            }
        }
        None
    }

    /// `binder_wakeup_proc_ilocked`: wake one waiting thread, else the poll
    /// threads (the supervisor re-evaluates the poll eventfd).
    fn wakeup_proc(&mut self, proc: ProcId) {
        if let Some(tid) = self.select_thread(proc) {
            self.wake_thread(proc, tid);
        }
        self.poll_dirty.insert(proc);
    }

    /// `binder_enqueue_thread_work_ilocked`.
    fn enqueue_thread_work(&mut self, proc: ProcId, tid: u32, w: Work) {
        let t = self.thread_mut(proc, tid);
        t.todo.push_back(w);
        t.process_todo = true;
        self.wake_thread(proc, tid);
    }

    /// `binder_enqueue_deferred_thread_work_ilocked`: queued, but the thread
    /// does not return to userspace for it alone.
    fn enqueue_deferred_thread_work(&mut self, proc: ProcId, tid: u32, w: Work) {
        self.thread_mut(proc, tid).todo.push_back(w);
    }

    fn enqueue_proc_work(&mut self, proc: ProcId, w: Work) {
        if let Some(p) = self.procs.get_mut(&proc) {
            p.todo.push_back(w);
        }
        self.wakeup_proc(proc);
    }

    /// Remove a work item from every list of `proc` (`binder_dequeue_work`).
    fn dequeue_work(&mut self, proc: ProcId, w: Work) {
        if let Some(p) = self.procs.get_mut(&proc) {
            p.todo.retain(|x| *x != w);
            for t in p.threads.values_mut() {
                t.todo.retain(|x| *x != w);
                if t.todo.is_empty() {
                    t.process_todo = false;
                }
            }
            if let Work::Death(d) = w {
                p.delivered_death.retain(|x| *x != d);
            }
        }
    }

    /// Would a poll thread of `proc` see work now (`binder_poll`)?
    pub fn proc_pollable(&self, proc: ProcId) -> bool {
        let Some(p) = self.procs.get(&proc) else {
            return false;
        };
        p.threads.values().any(|t| {
            t.looper & LOOPER_POLL != 0 && Self::has_work(p, t, Self::available_for_proc_work(t))
        })
    }

    // ---- node refcounts ----------------------------------------------------

    fn node_wants(n: &Node) -> (bool, bool) {
        let strong = n.internal_strong > 0 || n.local_strong > 0;
        let weak = !n.refs.is_empty() || n.local_weak > 0 || n.tmp_refs > 0 || strong;
        (strong, weak)
    }

    /// `binder_inc_node_nilocked`. `target` is the thread whose deferred todo
    /// receives the node work (the sender translating its own binder).
    fn inc_node(
        &mut self,
        nid: NodeId,
        strong: bool,
        internal: bool,
        target: Option<(ProcId, u32)>,
    ) {
        let Some(n) = self.nodes.get_mut(&nid) else {
            return;
        };
        let mut enqueue = false;
        let mut requeue = false;
        if strong {
            if internal {
                n.internal_strong += 1;
            } else {
                n.local_strong += 1;
            }
            if !n.has_strong_ref && target.is_some() {
                requeue = true;
            }
        } else {
            if !internal {
                n.local_weak += 1;
            }
            if !n.has_weak_ref && target.is_some() && !n.work_queued {
                enqueue = true;
            }
        }
        let owner = n.proc;
        if let (Some((tp, tt)), Some(owner)) = (target, owner)
            && (enqueue || requeue)
        {
            if requeue && self.nodes[&nid].work_queued {
                self.dequeue_work(owner, Work::Node(nid));
            }
            self.nodes.get_mut(&nid).unwrap().work_queued = true;
            self.enqueue_deferred_thread_work(tp, tt, Work::Node(nid));
        }
    }

    /// `binder_dec_node_nilocked` (+ freeing). Queues the owner's node work
    /// when the last strong or weak reference goes away.
    fn dec_node(&mut self, nid: NodeId, strong: bool, internal: bool) {
        let Some(n) = self.nodes.get_mut(&nid) else {
            return;
        };
        if strong {
            if internal {
                n.internal_strong -= 1;
            } else {
                n.local_strong -= 1;
            }
            if n.local_strong > 0 || n.internal_strong > 0 {
                return;
            }
        } else {
            if !internal {
                n.local_weak -= 1;
            }
            if n.local_weak > 0 || n.tmp_refs > 0 || !n.refs.is_empty() {
                return;
            }
        }
        self.node_maybe_release(nid);
    }

    fn node_maybe_release(&mut self, nid: NodeId) {
        let Some(n) = self.nodes.get(&nid) else {
            return;
        };
        if let Some(owner) = n.proc
            && (n.has_strong_ref || n.has_weak_ref)
        {
            if !n.work_queued {
                self.nodes.get_mut(&nid).unwrap().work_queued = true;
                self.enqueue_proc_work(owner, Work::Node(nid));
            }
            return;
        }
        let (strong, weak) = Self::node_wants(n);
        if !strong && !weak && n.refs.is_empty() {
            self.free_node(nid);
        }
    }

    fn free_node(&mut self, nid: NodeId) {
        if let Some(n) = self.nodes.remove(&nid) {
            if let Some(owner) = n.proc {
                if n.work_queued {
                    self.dequeue_work(owner, Work::Node(nid));
                }
                if let Some(p) = self.procs.get_mut(&owner)
                    && p.nodes.get(&n.ptr) == Some(&nid)
                {
                    p.nodes.remove(&n.ptr);
                }
            }
            for c in self.contexts.iter_mut() {
                if c.mgr_node == Some(nid) {
                    c.mgr_node = None;
                }
            }
        }
    }

    // ---- refs (handles) -----------------------------------------------------

    fn ref_node(&self, proc: ProcId, desc: u32, need_strong: bool) -> Option<NodeId> {
        let r = self.procs.get(&proc)?.refs_by_desc.get(&desc)?;
        if need_strong && r.strong == 0 {
            return None;
        }
        Some(r.node)
    }

    /// `binder_inc_ref_for_node`: get or create `proc`'s ref to `node`, take a
    /// strong or weak count on it, and return its descriptor.
    fn inc_ref_for_node(
        &mut self,
        proc: ProcId,
        nid: NodeId,
        strong: bool,
        target: Option<(ProcId, u32)>,
    ) -> Result<u32, u32> {
        let p = self.procs.get(&proc).ok_or(BR_DEAD_REPLY)?;
        let desc = match p.node_to_desc.get(&nid) {
            Some(&d) => d,
            None => {
                let is_mgr = self.contexts[p.context as usize].mgr_node == Some(nid);
                let mut desc = if is_mgr { 0 } else { 1 };
                for &d in p.refs_by_desc.range(desc..).map(|(d, _)| d) {
                    if d != desc {
                        break;
                    }
                    desc += 1;
                }
                let p = self.procs.get_mut(&proc).unwrap();
                p.refs_by_desc.insert(
                    desc,
                    Ref {
                        node: nid,
                        strong: 0,
                        weak: 0,
                        death: None,
                    },
                );
                p.node_to_desc.insert(nid, desc);
                self.nodes.get_mut(&nid).unwrap().refs.insert(proc);
                desc
            }
        };
        self.inc_ref(proc, desc, strong, target)?;
        Ok(desc)
    }

    /// `binder_inc_ref_olocked`.
    fn inc_ref(
        &mut self,
        proc: ProcId,
        desc: u32,
        strong: bool,
        target: Option<(ProcId, u32)>,
    ) -> Result<(), u32> {
        let r = self
            .procs
            .get_mut(&proc)
            .and_then(|p| p.refs_by_desc.get_mut(&desc))
            .ok_or(BR_FAILED_REPLY)?;
        let nid = r.node;
        if strong {
            if r.strong == 0 {
                // binder_inc_node(node, 1, 1, target): a node with no strong
                // refs can only gain one through translation of its owner's
                // object (target != None) or if it is a live context manager.
                let n = &self.nodes[&nid];
                let is_mgr = self.contexts.iter().any(|c| c.mgr_node == Some(nid));
                if target.is_none()
                    && n.internal_strong == 0
                    && !(n.proc.is_some() && is_mgr && n.has_strong_ref)
                {
                    return Err(BR_FAILED_REPLY);
                }
                self.inc_node(nid, true, true, target);
            }
            let r = self
                .procs
                .get_mut(&proc)
                .unwrap()
                .refs_by_desc
                .get_mut(&desc)
                .unwrap();
            r.strong += 1;
        } else {
            if r.weak == 0 {
                self.inc_node(nid, false, true, target);
            }
            let r = self
                .procs
                .get_mut(&proc)
                .unwrap()
                .refs_by_desc
                .get_mut(&desc)
                .unwrap();
            r.weak += 1;
        }
        Ok(())
    }

    /// `binder_dec_ref_olocked` + `binder_cleanup_ref_olocked` when it hits 0.
    fn dec_ref(&mut self, proc: ProcId, desc: u32, strong: bool) -> Result<(), i32> {
        let r = self
            .procs
            .get_mut(&proc)
            .and_then(|p| p.refs_by_desc.get_mut(&desc))
            .ok_or(-libc::EINVAL)?;
        let nid = r.node;
        if strong {
            if r.strong == 0 {
                return Err(-libc::EINVAL);
            }
            r.strong -= 1;
            if r.strong == 0 {
                self.dec_node(nid, true, true);
            }
        } else {
            if r.weak == 0 {
                return Err(-libc::EINVAL);
            }
            r.weak -= 1;
        }
        let r = &self.procs[&proc].refs_by_desc[&desc];
        if r.strong == 0 && r.weak == 0 {
            self.delete_ref(proc, desc);
        }
        Ok(())
    }

    fn delete_ref(&mut self, proc: ProcId, desc: u32) {
        let Some(p) = self.procs.get_mut(&proc) else {
            return;
        };
        let Some(r) = p.refs_by_desc.remove(&desc) else {
            return;
        };
        p.node_to_desc.remove(&r.node);
        if r.strong > 0 {
            self.dec_node(r.node, true, true);
        }
        if let Some(n) = self.nodes.get_mut(&r.node) {
            n.refs.remove(&proc);
        }
        self.dec_node(r.node, false, true);
        if let Some(d) = r.death {
            self.dequeue_work(proc, Work::Death(d));
            self.deaths.remove(&d);
        }
    }

    // ---- top-level BINDER_WRITE_READ ----------------------------------------

    /// One `BINDER_WRITE_READ`: `write` is the unconsumed part of the write
    /// buffer, `read_size` the free space in the read buffer.
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
        if !self.procs.contains_key(&proc) {
            res.write_err = libc::EINVAL;
            return res;
        }
        self.thread_mut(proc, tid);
        if !write.is_empty() {
            let enter_only = read_size == 0;
            let (consumed, err) = self.thread_write(proc, tid, write, mem);
            res.write_consumed = consumed;
            if err != 0 {
                res.write_err = err;
                self.ioctl_done(proc, tid);
                res.wakeups = self.take_wakeups();
                return res;
            }
            // A write-only BC_ENTER_LOOPER is IPCThreadState::setupPolling():
            // this thread will epoll the binder fd (kernel: binder_poll marks
            // it BINDER_LOOPER_STATE_POLL).
            if enter_only && has_cmd(write, BC_ENTER_LOOPER) {
                let t = self.thread_mut(proc, tid);
                if t.looper & LOOPER_POLL == 0 {
                    t.looper |= LOOPER_POLL;
                    res.became_poll = true;
                }
                self.poll_dirty.insert(proc);
            }
        }
        if read_size > 0 {
            match self.thread_read(proc, tid, read_size, non_block, mem) {
                ReadOutcome::Data(d) => res.read = d,
                ReadOutcome::Blocked => res.blocked = true,
                ReadOutcome::Again => res.eagain = true,
            }
        }
        if !res.blocked {
            self.ioctl_done(proc, tid);
        }
        res.wakeups = self.take_wakeups();
        res
    }

    /// End of `binder_ioctl` for this thread.
    pub fn ioctl_done(&mut self, proc: ProcId, tid: u32) {
        if let Some(t) = self
            .procs
            .get_mut(&proc)
            .and_then(|p| p.threads.get_mut(&tid))
        {
            t.looper_need_return = false;
        }
    }

    /// `binder_thread_write`. Returns (bytes consumed, errno).
    fn thread_write(
        &mut self,
        proc: ProcId,
        tid: u32,
        buf: &[u8],
        mem: &mut dyn Mem,
    ) -> (usize, i32) {
        let mut off = 0usize;
        while off < buf.len() {
            if self.procs[&proc].threads[&tid].return_error != BR_OK {
                break;
            }
            let Some(cmd) = read_pod::<u32>(buf, off) else {
                return (off, libc::EFAULT);
            };
            let start = off;
            off += 4;
            macro_rules! arg {
                ($t:ty) => {{
                    match read_pod::<$t>(buf, off) {
                        Some(v) => {
                            off += size_of::<$t>();
                            v
                        }
                        None => return (start, libc::EFAULT),
                    }
                }};
            }
            match cmd {
                BC_INCREFS | BC_ACQUIRE | BC_RELEASE | BC_DECREFS => {
                    let target: u32 = arg!(u32);
                    let strong = cmd == BC_ACQUIRE || cmd == BC_RELEASE;
                    let inc = cmd == BC_INCREFS || cmd == BC_ACQUIRE;
                    let r = if inc && target == 0 {
                        let ctx = self.procs[&proc].context as usize;
                        match self.contexts[ctx].mgr_node {
                            Some(n) if self.nodes[&n].proc == Some(proc) => Err(()),
                            Some(n) => self
                                .inc_ref_for_node(proc, n, strong, None)
                                .map(|_| ())
                                .map_err(|_| ()),
                            None => self.inc_ref(proc, 0, strong, None).map_err(|_| ()),
                        }
                    } else if inc {
                        if self.ref_node(proc, target, strong).is_some() {
                            self.inc_ref(proc, target, strong, None).map_err(|_| ())
                        } else {
                            Err(())
                        }
                    } else if self.ref_node(proc, target, strong).is_some() {
                        self.dec_ref(proc, target, strong).map_err(|_| ())
                    } else {
                        Err(())
                    };
                    if r.is_err() {
                        tlog!("proc={proc} refcount change {cmd:#x} on invalid ref {target}");
                    }
                }
                BC_INCREFS_DONE | BC_ACQUIRE_DONE => {
                    let ptr: u64 = arg!(u64);
                    let cookie: u64 = arg!(u64);
                    let Some(&nid) = self.procs[&proc].nodes.get(&ptr) else {
                        continue;
                    };
                    let n = self.nodes.get_mut(&nid).unwrap();
                    if n.cookie != cookie {
                        continue;
                    }
                    let strong = cmd == BC_ACQUIRE_DONE;
                    if strong {
                        if !n.pending_strong {
                            continue;
                        }
                        n.pending_strong = false;
                    } else {
                        if !n.pending_weak {
                            continue;
                        }
                        n.pending_weak = false;
                    }
                    self.dec_node(nid, strong, false);
                }
                BC_ATTEMPT_ACQUIRE | BC_ACQUIRE_RESULT => return (start, libc::EINVAL),
                BC_FREE_BUFFER => {
                    let ptr: u64 = arg!(u64);
                    self.free_buffer_cmd(proc, ptr, mem);
                }
                BC_TRANSACTION_SG | BC_REPLY_SG => {
                    let td: BinderTransactionData = arg!(BinderTransactionData);
                    let extra: u64 = arg!(u64);
                    self.transaction(proc, tid, &td, cmd == BC_REPLY_SG, extra, mem);
                }
                BC_TRANSACTION | BC_REPLY => {
                    let td: BinderTransactionData = arg!(BinderTransactionData);
                    self.transaction(proc, tid, &td, cmd == BC_REPLY, 0, mem);
                }
                BC_REGISTER_LOOPER => {
                    let p = self.procs.get_mut(&proc).unwrap();
                    let t = p.threads.get_mut(&tid).unwrap();
                    if t.looper & LOOPER_ENTERED != 0 || p.requested_threads == 0 {
                        t.looper |= LOOPER_INVALID;
                    } else {
                        p.requested_threads -= 1;
                        p.requested_threads_started += 1;
                    }
                    t.looper |= LOOPER_REGISTERED;
                }
                BC_ENTER_LOOPER => {
                    let t = self.thread_mut(proc, tid);
                    if t.looper & LOOPER_REGISTERED != 0 {
                        t.looper |= LOOPER_INVALID;
                    }
                    t.looper |= LOOPER_ENTERED;
                }
                BC_EXIT_LOOPER => {
                    self.thread_mut(proc, tid).looper |= LOOPER_EXITED;
                }
                BC_REQUEST_DEATH_NOTIFICATION | BC_CLEAR_DEATH_NOTIFICATION => {
                    let target: u32 = arg!(u32);
                    let cookie: u64 = arg!(u64);
                    self.death_cmd(
                        proc,
                        tid,
                        target,
                        cookie,
                        cmd == BC_REQUEST_DEATH_NOTIFICATION,
                    );
                }
                BC_DEAD_BINDER_DONE => {
                    let cookie: u64 = arg!(u64);
                    self.dead_binder_done(proc, tid, cookie);
                }
                BC_REQUEST_FREEZE_NOTIFICATION | BC_CLEAR_FREEZE_NOTIFICATION => {
                    // Freezing is not supported (BINDER_FREEZE fails), so no
                    // freeze state ever changes; accept and ignore.
                    let _target: u32 = arg!(u32);
                    let _cookie: u64 = arg!(u64);
                }
                BC_FREEZE_NOTIFICATION_DONE => {
                    let _cookie: u64 = arg!(u64);
                }
                _ => return (start, libc::EINVAL),
            }
        }
        (off, 0)
    }

    // ---- transactions -------------------------------------------------------

    /// Record a failed transaction on the sending thread (the error part of
    /// `binder_transaction`).
    fn txn_error(
        &mut self,
        proc: ProcId,
        tid: u32,
        err: u32,
        errno: i32,
        in_reply_to: Option<TxnId>,
    ) {
        tlog!("txn fail proc={proc} tid={tid} err={err:#x} errno={errno} reply_to={in_reply_to:?}");
        let t = self.thread_mut(proc, tid);
        t.ee = BinderExtendedError {
            id: 0,
            command: err,
            param: errno,
        };
        if let Some(irt) = in_reply_to {
            t.return_error = BR_TRANSACTION_COMPLETE;
            self.enqueue_thread_work(proc, tid, Work::ReturnError);
            self.send_failed_reply(irt, err);
        } else {
            t.return_error = err;
            self.enqueue_thread_work(proc, tid, Work::ReturnError);
        }
    }

    fn transaction(
        &mut self,
        proc: ProcId,
        tid: u32,
        td: &BinderTransactionData,
        reply: bool,
        extra: u64,
        mem: &mut dyn Mem,
    ) {
        let oneway = td.flags & TF_ONE_WAY != 0;
        tlog!(
            "txn proc={proc} tid={tid} reply={reply} oneway={oneway} handle={} code={:#x} dsz={} osz={}",
            td.target as u32,
            td.code,
            td.data_size,
            td.offsets_size
        );
        let target_proc;
        let mut target_thread: Option<u32> = None;
        let mut target_node: Option<NodeId> = None;
        let mut in_reply_to: Option<TxnId> = None;

        if reply {
            let Some(irt) = self.procs[&proc].threads[&tid].transaction_stack else {
                return self.txn_error(proc, tid, BR_FAILED_REPLY, -libc::EPROTO, None);
            };
            if self.txns[&irt].to_thread != Some(tid) || self.txns[&irt].to_proc != Some(proc) {
                return self.txn_error(proc, tid, BR_FAILED_REPLY, -libc::EPROTO, None);
            }
            let to_parent = self.txns[&irt].to_parent;
            self.thread_mut(proc, tid).transaction_stack = to_parent;
            in_reply_to = Some(irt);
            let Some((fp, ft)) = self.txns[&irt].from else {
                return self.txn_error(proc, tid, BR_DEAD_REPLY, 0, in_reply_to);
            };
            let ok = self
                .procs
                .get(&fp)
                .and_then(|p| p.threads.get(&ft))
                .map(|t| t.transaction_stack == Some(irt))
                .unwrap_or(false);
            if !ok {
                return self.txn_error(proc, tid, BR_FAILED_REPLY, -libc::EPROTO, None);
            }
            target_proc = fp;
            target_thread = Some(ft);
        } else {
            let handle = td.target as u32;
            let node = if handle != 0 {
                self.ref_node(proc, handle, true)
            } else {
                self.contexts[self.procs[&proc].context as usize].mgr_node
            };
            let Some(node) = node else {
                let e = if handle == 0 {
                    BR_DEAD_REPLY
                } else {
                    BR_FAILED_REPLY
                };
                return self.txn_error(proc, tid, e, -libc::EINVAL, None);
            };
            let Some(tp) = self.nodes[&node].proc else {
                return self.txn_error(proc, tid, BR_DEAD_REPLY, 0, None);
            };
            if tp == proc {
                return self.txn_error(proc, tid, BR_FAILED_REPLY, -libc::EINVAL, None);
            }
            target_proc = tp;
            target_node = Some(node);
            if !oneway && let Some(top) = self.procs[&proc].threads[&tid].transaction_stack {
                if self.txns[&top].to_thread != Some(tid) || self.txns[&top].to_proc != Some(proc) {
                    return self.txn_error(proc, tid, BR_FAILED_REPLY, -libc::EPROTO, None);
                }
                let mut cur = Some(top);
                while let Some(c) = cur {
                    let t = &self.txns[&c];
                    if let Some((fp, ft)) = t.from
                        && fp == target_proc
                    {
                        target_thread = Some(ft);
                        break;
                    }
                    cur = t.from_parent;
                }
            }
        }
        if self.procs.get(&target_proc).map(|p| p.dead).unwrap_or(true) {
            return self.txn_error(proc, tid, BR_DEAD_REPLY, 0, in_reply_to);
        }

        // Keep the target node alive and owned for the buffer's lifetime
        // (binder_get_node_refs_for_txn takes a local strong ref).
        if let Some(n) = target_node {
            self.inc_node(n, true, false, None);
        }
        let is_async = !reply && oneway;
        let built = self.build_buffer(
            proc,
            tid,
            td,
            extra,
            target_proc,
            target_node,
            in_reply_to,
            is_async,
            mem,
        );
        let (addr, spam) = match built {
            Ok(v) => v,
            Err((e, errno)) => {
                if let Some(n) = target_node {
                    self.dec_node(n, true, false);
                }
                return self.txn_error(proc, tid, e, errno, in_reply_to);
            }
        };
        let txid = self.id();
        self.txns.insert(
            txid,
            Txn {
                from: if !reply && !oneway {
                    Some((proc, tid))
                } else {
                    None
                },
                from_parent: None,
                to_proc: Some(target_proc),
                to_thread: target_thread,
                to_parent: None,
                buffer: Some((target_proc, addr)),
                code: td.code,
                flags: td.flags,
                sender_euid: self.procs[&proc].euid,
            },
        );
        self.buffers
            .get_mut(&(target_proc, addr))
            .unwrap()
            .transaction = Some(txid);
        let tc = if spam {
            Work::OnewaySpamSuspect
        } else {
            Work::TransactionComplete
        };

        if reply {
            self.enqueue_thread_work(proc, tid, tc);
            let ft = target_thread.unwrap();
            let irt = in_reply_to.unwrap();
            if self.procs[&target_proc]
                .threads
                .get(&ft)
                .map(|t| t.is_dead)
                .unwrap_or(true)
            {
                self.dequeue_tc(proc, tid);
                self.abort_txn(txid, mem);
                return self.txn_error(proc, tid, BR_DEAD_REPLY, 0, in_reply_to);
            }
            self.pop_transaction(target_proc, ft, irt);
            self.enqueue_thread_work(target_proc, ft, Work::Transaction(txid));
            self.free_txn(irt);
        } else if !oneway {
            self.enqueue_deferred_thread_work(proc, tid, tc);
            let cur = self.procs[&proc].threads[&tid].transaction_stack;
            self.txns.get_mut(&txid).unwrap().from_parent = cur;
            self.thread_mut(proc, tid).transaction_stack = Some(txid);
            if let Err(e) = self.proc_transaction(txid, target_proc, target_thread) {
                // binder_pop_transaction_ilocked(thread, t)
                self.thread_mut(proc, tid).transaction_stack = cur;
                self.dequeue_tc(proc, tid);
                self.abort_txn(txid, mem);
                self.txn_error(proc, tid, e, 0, None);
            }
        } else {
            match self.proc_transaction(txid, target_proc, None) {
                Ok(()) => self.enqueue_thread_work(proc, tid, tc),
                Err(e) => {
                    self.abort_txn(txid, mem);
                    self.txn_error(proc, tid, e, 0, None);
                }
            }
        }
    }

    /// Remove the deferred TRANSACTION_COMPLETE queued for a failed call.
    fn dequeue_tc(&mut self, proc: ProcId, tid: u32) {
        let t = self.thread_mut(proc, tid);
        if let Some(pos) = t
            .todo
            .iter()
            .rposition(|w| matches!(w, Work::TransactionComplete | Work::OnewaySpamSuspect))
        {
            t.todo.remove(pos);
        }
    }

    /// Undo a transaction that was built but never queued.
    fn abort_txn(&mut self, txid: TxnId, mem: &mut dyn Mem) {
        if let Some(t) = self.txns.remove(&txid)
            && let Some(key) = t.buffer
        {
            self.release_buffer(key, mem);
        }
    }

    /// `binder_proc_transaction`.
    fn proc_transaction(
        &mut self,
        txid: TxnId,
        target_proc: ProcId,
        target_thread: Option<u32>,
    ) -> Result<(), u32> {
        let (oneway, node) = {
            let t = &self.txns[&txid];
            let node = t
                .buffer
                .and_then(|k| self.buffers.get(&k))
                .and_then(|b| b.target_node);
            (t.flags & TF_ONE_WAY != 0, node)
        };
        let mut pending_async = false;
        if oneway && let Some(n) = node {
            let n = self.nodes.get_mut(&n).unwrap();
            if n.has_async_transaction {
                pending_async = true;
            } else {
                n.has_async_transaction = true;
            }
        }
        let dead = self.procs.get(&target_proc).map(|p| p.dead).unwrap_or(true)
            || target_thread
                .map(|t| {
                    self.procs[&target_proc]
                        .threads
                        .get(&t)
                        .map(|t| t.is_dead)
                        .unwrap_or(true)
                })
                .unwrap_or(false);
        if dead {
            if oneway
                && !pending_async
                && let Some(n) = node
            {
                self.nodes.get_mut(&n).unwrap().has_async_transaction = false;
            }
            return Err(BR_DEAD_REPLY);
        }
        let thread = if target_thread.is_none() && !pending_async {
            self.select_thread(target_proc)
        } else {
            target_thread
        };
        if let Some(th) = thread {
            self.enqueue_thread_work(target_proc, th, Work::Transaction(txid));
        } else if !pending_async {
            self.procs
                .get_mut(&target_proc)
                .unwrap()
                .todo
                .push_back(Work::Transaction(txid));
            self.poll_dirty.insert(target_proc);
        } else {
            let n = node.unwrap();
            self.nodes.get_mut(&n).unwrap().async_todo.push_back(txid);
        }
        Ok(())
    }

    /// `binder_pop_transaction_ilocked`.
    fn pop_transaction(&mut self, proc: ProcId, tid: u32, txid: TxnId) {
        let parent = self.txns.get(&txid).and_then(|t| t.from_parent);
        if let Some(t) = self
            .procs
            .get_mut(&proc)
            .and_then(|p| p.threads.get_mut(&tid))
            && t.transaction_stack == Some(txid)
        {
            t.transaction_stack = parent;
        }
        if let Some(t) = self.txns.get_mut(&txid) {
            t.from = None;
        }
    }

    /// `binder_free_transaction`: drop the record; its buffer lives on until
    /// BC_FREE_BUFFER.
    fn free_txn(&mut self, txid: TxnId) {
        if let Some(t) = self.txns.remove(&txid)
            && let Some(key) = t.buffer
            && let Some(b) = self.buffers.get_mut(&key)
        {
            b.transaction = None;
        }
    }

    /// `binder_send_failed_reply`: fail the synchronous caller of `txid` (or
    /// the first live caller up its chain).
    fn send_failed_reply(&mut self, txid: TxnId, err: u32) {
        let mut cur = Some(txid);
        while let Some(t) = cur {
            let Some(tx) = self.txns.get(&t) else {
                return;
            };
            if tx.flags & TF_ONE_WAY != 0 {
                self.free_txn(t);
                return;
            }
            if let Some((fp, ft)) = tx.from
                && self
                    .procs
                    .get(&fp)
                    .and_then(|p| p.threads.get(&ft))
                    .is_some()
            {
                self.pop_transaction(fp, ft, t);
                let th = self.thread_mut(fp, ft);
                if th.reply_error == BR_OK {
                    th.reply_error = err;
                    self.enqueue_thread_work(fp, ft, Work::ReplyError);
                }
                self.free_txn(t);
                return;
            }
            let next = tx.from_parent;
            self.free_txn(t);
            cur = next;
        }
    }

    /// Copy the sender's data into a new buffer in `target_proc`, translating
    /// every object (`binder_transaction` from `binder_alloc_new_buf` to the
    /// end of the object loop). Returns the buffer address.
    #[allow(clippy::too_many_arguments)]
    fn build_buffer(
        &mut self,
        proc: ProcId,
        tid: u32,
        td: &BinderTransactionData,
        extra: u64,
        target_proc: ProcId,
        target_node: Option<NodeId>,
        in_reply_to: Option<TxnId>,
        is_async: bool,
        mem: &mut dyn Mem,
    ) -> Result<(u64, bool), (u32, i32)> {
        let fail = (BR_FAILED_REPLY, -libc::EINVAL);
        let data_size = usize::try_from(td.data_size).map_err(|_| fail)?;
        let offsets_size = usize::try_from(td.offsets_size).map_err(|_| fail)?;
        let extra = usize::try_from(extra).map_err(|_| fail)?;
        if !offsets_size.is_multiple_of(8) || !extra.is_multiple_of(8) {
            return Err(fail);
        }
        let want_secctx = target_node
            .map(|n| self.nodes[&n].txn_security_ctx)
            .unwrap_or(false);
        const SECCTX: &[u8] = b"u:r:su:s0\0";
        // Checked: a huge data_size must fail here, not wrap to a small total.
        let off_start = data_size.checked_next_multiple_of(8).ok_or(fail)?;
        let sg_start = off_start.checked_add(offsets_size).ok_or(fail)?;
        let sg_end = sg_start.checked_add(extra).ok_or(fail)?;
        let total = if want_secctx {
            sg_end.checked_add(align_up(SECCTX.len(), 8)).ok_or(fail)?
        } else {
            sg_end
        }
        .max(8);
        // Bound everything by the target's mapped region before reading.
        let space = mem.recv_size(target_proc);
        if total > space {
            return Err((BR_FAILED_REPLY, -libc::ENOSPC));
        }
        {
            let p = self.procs.get_mut(&target_proc).unwrap();
            if p.free_async_space < 0 {
                p.free_async_space = (space / 2) as i64;
            }
            if is_async && p.free_async_space < total as i64 {
                return Err((BR_FAILED_REPLY, -libc::ENOSPC));
            }
        }
        let mut data = mem
            .read(proc, td.data_buffer, data_size)
            .ok_or((BR_FAILED_REPLY, -libc::EFAULT))?;
        let offsets = mem
            .read(proc, td.data_offsets, offsets_size)
            .ok_or((BR_FAILED_REPLY, -libc::EFAULT))?;
        let addr = mem
            .alloc_recv(target_proc, total, is_async)
            .ok_or((BR_FAILED_REPLY, -libc::ENOSPC))?;
        let spam = {
            let p = self.procs.get_mut(&target_proc).unwrap();
            if is_async {
                p.free_async_space -= total as i64;
            }
            is_async && p.oneway_spam_detection && p.free_async_space < (space / 10) as i64
        };
        self.buffers.insert(
            (target_proc, addr),
            Buffer {
                data_size,
                offsets_size,
                total,
                is_async,
                target_node,
                transaction: None,
                allow_user_free: false,
                objects: Vec::new(),
                fd_fixups: Vec::new(),
                secctx: 0,
            },
        );
        let key = (target_proc, addr);
        let r = self.translate(
            proc,
            tid,
            &mut data,
            &offsets,
            sg_start,
            extra,
            key,
            in_reply_to,
            mem,
        );
        let mut out = match r {
            Ok(sg) => {
                let mut out = vec![0u8; total];
                out[..data_size].copy_from_slice(&data);
                out[off_start..off_start + offsets_size].copy_from_slice(&offsets);
                out[sg_start..sg_end].copy_from_slice(&sg);
                out
            }
            Err(e) => {
                // The caller drops the target-node ref it took.
                self.buffers.get_mut(&key).unwrap().target_node = None;
                self.release_buffer(key, mem);
                return Err(e);
            }
        };
        if want_secctx {
            out[sg_end..sg_end + SECCTX.len()].copy_from_slice(SECCTX);
            self.buffers.get_mut(&key).unwrap().secctx = addr + sg_end as u64;
        }
        mem.write_recv(target_proc, addr, &out);
        Ok((addr, spam))
    }

    /// Translate the objects in `data` for `key.0`; returns the gathered
    /// scatter-gather area. Objects translated so far are recorded on the
    /// buffer, so the caller releases them on failure.
    #[allow(clippy::too_many_arguments)]
    fn translate(
        &mut self,
        proc: ProcId,
        tid: u32,
        data: &mut [u8],
        offsets: &[u8],
        sg_start: usize,
        extra: usize,
        key: (ProcId, u64),
        in_reply_to: Option<TxnId>,
        mem: &mut dyn Mem,
    ) -> Result<Vec<u8>, (u32, i32)> {
        let fail = (BR_FAILED_REPLY, -libc::EINVAL);
        let (target_proc, addr) = key;
        let allow_fds = match in_reply_to {
            Some(t) => self
                .txns
                .get(&t)
                .map(|t| t.flags & TF_ACCEPT_FDS != 0)
                .unwrap_or(false),
            None => self.buffers[&key]
                .target_node
                .map(|n| self.nodes[&n].accept_fds)
                .unwrap_or(false),
        };
        let mut sg = vec![0u8; extra];
        let mut sg_used = 0usize;
        let mut off_min = 0usize;
        // (object offset in data, sg offset, length) of validated PTR objects
        let mut ptrs: Vec<Option<(usize, usize)>> = Vec::new();
        for i in 0..offsets.len() / 8 {
            let obj_off = read_pod::<u64>(offsets, i * 8).unwrap();
            let obj_off = usize::try_from(obj_off).map_err(|_| fail)?;
            if obj_off < off_min
                || !obj_off.is_multiple_of(4)
                || obj_off.checked_add(8).ok_or(fail)? > data.len()
            {
                tlog!(
                    "xlate: bad object offset {obj_off} (min {off_min}, size {})",
                    data.len()
                );
                return Err(fail);
            }
            let hdr = read_pod::<u32>(data, obj_off).unwrap();
            let mut this_ptr = None;
            match hdr {
                BINDER_TYPE_BINDER | BINDER_TYPE_WEAK_BINDER => {
                    let mut fbo: FlatBinderObject = read_pod(data, obj_off).ok_or(fail)?;
                    let strong = hdr == BINDER_TYPE_BINDER;
                    let nid =
                        self.get_or_new_node(proc, fbo.binder_or_handle, fbo.cookie, fbo.flags);
                    if self.nodes[&nid].cookie != fbo.cookie {
                        return Err(fail);
                    }
                    let desc = self
                        .inc_ref_for_node(target_proc, nid, strong, Some((proc, tid)))
                        .map_err(|e| (e, -libc::EINVAL))?;
                    self.buffers
                        .get_mut(&key)
                        .unwrap()
                        .objects
                        .push(Obj::Handle { desc, strong });
                    fbo.hdr_type = if strong {
                        BINDER_TYPE_HANDLE
                    } else {
                        BINDER_TYPE_WEAK_HANDLE
                    };
                    fbo.binder_or_handle = desc as u64;
                    fbo.cookie = 0;
                    data[obj_off..obj_off + size_of::<FlatBinderObject>()]
                        .copy_from_slice(pod_bytes(&fbo));
                    off_min = obj_off + size_of::<FlatBinderObject>();
                }
                BINDER_TYPE_HANDLE | BINDER_TYPE_WEAK_HANDLE => {
                    let mut fbo: FlatBinderObject = read_pod(data, obj_off).ok_or(fail)?;
                    let strong = hdr == BINDER_TYPE_HANDLE;
                    let h = fbo.binder_or_handle as u32;
                    let Some(nid) = self.ref_node(proc, h, strong) else {
                        tlog!("xlate: proc={proc} handle {h} not found");
                        return Err(fail);
                    };
                    if self.nodes[&nid].proc == Some(target_proc) {
                        let n = &self.nodes[&nid];
                        fbo.hdr_type = if strong {
                            BINDER_TYPE_BINDER
                        } else {
                            BINDER_TYPE_WEAK_BINDER
                        };
                        fbo.binder_or_handle = n.ptr;
                        fbo.cookie = n.cookie;
                        self.inc_node(nid, strong, false, None);
                        self.buffers
                            .get_mut(&key)
                            .unwrap()
                            .objects
                            .push(Obj::Node { node: nid, strong });
                    } else {
                        let desc = self
                            .inc_ref_for_node(target_proc, nid, strong, None)
                            .map_err(|e| (e, -libc::EINVAL))?;
                        self.buffers
                            .get_mut(&key)
                            .unwrap()
                            .objects
                            .push(Obj::Handle { desc, strong });
                        fbo.binder_or_handle = desc as u64;
                        fbo.cookie = 0;
                    }
                    data[obj_off..obj_off + size_of::<FlatBinderObject>()]
                        .copy_from_slice(pod_bytes(&fbo));
                    off_min = obj_off + size_of::<FlatBinderObject>();
                }
                BINDER_TYPE_FD => {
                    let mut fdo: BinderFdObject = read_pod(data, obj_off).ok_or(fail)?;
                    if !allow_fds {
                        return Err((BR_FAILED_REPLY, -libc::EPERM));
                    }
                    let tok = mem
                        .capture_fd(proc, fdo.fd_or_pad as u32)
                        .ok_or((BR_FAILED_REPLY, -libc::EBADF))?;
                    self.buffers
                        .get_mut(&key)
                        .unwrap()
                        .fd_fixups
                        .push((obj_off + 8, tok));
                    fdo.fd_or_pad = 0;
                    data[obj_off..obj_off + size_of::<BinderFdObject>()]
                        .copy_from_slice(pod_bytes(&fdo));
                    off_min = obj_off + size_of::<BinderFdObject>();
                }
                BINDER_TYPE_PTR => {
                    let mut bbo: BinderBufferObject = read_pod(data, obj_off).ok_or(fail)?;
                    let len = usize::try_from(bbo.length).map_err(|_| fail)?;
                    if len > extra - sg_used {
                        return Err(fail);
                    }
                    let payload = mem
                        .read(proc, bbo.buffer, len)
                        .ok_or((BR_FAILED_REPLY, -libc::EFAULT))?;
                    let at = sg_used;
                    sg[at..at + len].copy_from_slice(&payload);
                    sg_used += align_up(len, 8).min(extra - sg_used);
                    bbo.buffer = addr + (sg_start + at) as u64;
                    if bbo.flags & BINDER_BUFFER_FLAG_HAS_PARENT != 0 {
                        let (psg, plen) = parent_ptr(&ptrs, bbo.parent).ok_or(fail)?;
                        let po = usize::try_from(bbo.parent_offset).map_err(|_| fail)?;
                        if plen < 8 || po > plen - 8 {
                            return Err(fail);
                        }
                        sg[psg + po..psg + po + 8].copy_from_slice(&bbo.buffer.to_le_bytes());
                    }
                    data[obj_off..obj_off + size_of::<BinderBufferObject>()]
                        .copy_from_slice(pod_bytes(&bbo));
                    this_ptr = Some((at, len));
                    off_min = obj_off + size_of::<BinderBufferObject>();
                }
                BINDER_TYPE_FDA => {
                    let fda: BinderFdArrayObject = read_pod(data, obj_off).ok_or(fail)?;
                    if !allow_fds {
                        return Err((BR_FAILED_REPLY, -libc::EPERM));
                    }
                    let (psg, plen) = parent_ptr(&ptrs, fda.parent).ok_or(fail)?;
                    let po = usize::try_from(fda.parent_offset).map_err(|_| fail)?;
                    let n = usize::try_from(fda.num_fds).map_err(|_| fail)?;
                    let bytes = n.checked_mul(4).ok_or(fail)?;
                    if po.checked_add(bytes).ok_or(fail)? > plen
                        || !(sg_start + psg + po).is_multiple_of(4)
                    {
                        return Err(fail);
                    }
                    for k in 0..n {
                        let at = psg + po + k * 4;
                        let fd = u32::from_le_bytes(sg[at..at + 4].try_into().unwrap());
                        let tok = mem
                            .capture_fd(proc, fd)
                            .ok_or((BR_FAILED_REPLY, -libc::EBADF))?;
                        self.buffers
                            .get_mut(&key)
                            .unwrap()
                            .fd_fixups
                            .push((sg_start + at, tok));
                        sg[at..at + 4].copy_from_slice(&0u32.to_le_bytes());
                    }
                    off_min = obj_off + size_of::<BinderFdArrayObject>();
                }
                _ => {
                    tlog!("xlate: bad object type {hdr:#x}");
                    return Err(fail);
                }
            }
            ptrs.push(this_ptr);
        }
        Ok(sg)
    }

    // ---- buffers ------------------------------------------------------------

    /// `BC_FREE_BUFFER`.
    fn free_buffer_cmd(&mut self, proc: ProcId, ptr: u64, mem: &mut dyn Mem) {
        let key = (proc, ptr);
        let Some(b) = self.buffers.get(&key) else {
            tlog!("proc={proc} BC_FREE_BUFFER {ptr:#x} no match");
            return;
        };
        if !b.allow_user_free {
            tlog!("proc={proc} BC_FREE_BUFFER {ptr:#x} not freeable");
            return;
        }
        if let Some(t) = b.transaction
            && let Some(tx) = self.txns.get_mut(&t)
        {
            tx.buffer = None;
        }
        if b.is_async
            && let Some(n) = b.target_node
            && let Some(node) = self.nodes.get_mut(&n)
        {
            match node.async_todo.pop_front() {
                None => node.has_async_transaction = false,
                Some(next) => self.enqueue_proc_work(proc, Work::Transaction(next)),
            }
        }
        self.release_buffer(key, mem);
    }

    /// `binder_transaction_buffer_release` + `binder_alloc_free_buf`.
    fn release_buffer(&mut self, key: (ProcId, u64), mem: &mut dyn Mem) {
        let Some(b) = self.buffers.remove(&key) else {
            return;
        };
        let (proc, addr) = key;
        if let Some(t) = b.transaction
            && let Some(tx) = self.txns.get_mut(&t)
        {
            tx.buffer = None;
        }
        if let Some(n) = b.target_node {
            self.dec_node(n, true, false);
        }
        for o in b.objects {
            match o {
                Obj::Node { node, strong } => self.dec_node(node, strong, false),
                Obj::Handle { desc, strong } => {
                    let _ = self.dec_ref(proc, desc, strong);
                }
            }
        }
        for (_, tok) in b.fd_fixups {
            mem.drop_fd(tok);
        }
        if b.is_async
            && let Some(p) = self.procs.get_mut(&proc)
        {
            p.free_async_space += b.total as i64;
        }
        mem.free_recv(proc, addr);
    }

    // ---- death notifications ------------------------------------------------

    fn death_cmd(&mut self, proc: ProcId, tid: u32, desc: u32, cookie: u64, request: bool) {
        let Some(r) = self.procs[&proc].refs_by_desc.get(&desc) else {
            tlog!("proc={proc} death notification on invalid ref {desc}");
            return;
        };
        let nid = r.node;
        if request {
            if r.death.is_some() {
                return;
            }
            let d = self.id();
            self.deaths.insert(
                d,
                Death {
                    cookie,
                    kind: DeathKind::DeadBinder,
                    queued: false,
                },
            );
            self.procs
                .get_mut(&proc)
                .unwrap()
                .refs_by_desc
                .get_mut(&desc)
                .unwrap()
                .death = Some(d);
            if self
                .nodes
                .get(&nid)
                .map(|n| n.proc.is_none())
                .unwrap_or(true)
            {
                self.deaths.get_mut(&d).unwrap().queued = true;
                self.enqueue_proc_work(proc, Work::Death(d));
            }
        } else {
            let Some(d) = r.death else {
                return;
            };
            if self.deaths[&d].cookie != cookie {
                return;
            }
            self.procs
                .get_mut(&proc)
                .unwrap()
                .refs_by_desc
                .get_mut(&desc)
                .unwrap()
                .death = None;
            let death = self.deaths.get_mut(&d).unwrap();
            if !death.queued {
                death.kind = DeathKind::Clear;
                death.queued = true;
                self.queue_death_to_thread_or_proc(proc, tid, d);
            } else {
                death.kind = DeathKind::DeadBinderAndClear;
            }
        }
    }

    fn queue_death_to_thread_or_proc(&mut self, proc: ProcId, tid: u32, d: DeathId) {
        if self.thread_mut(proc, tid).looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0 {
            self.enqueue_thread_work(proc, tid, Work::Death(d));
        } else {
            self.enqueue_proc_work(proc, Work::Death(d));
        }
    }

    fn dead_binder_done(&mut self, proc: ProcId, tid: u32, cookie: u64) {
        let found = self.procs[&proc].delivered_death.iter().copied().find(|d| {
            self.deaths
                .get(d)
                .map(|x| x.cookie == cookie)
                .unwrap_or(false)
        });
        let Some(d) = found else {
            return;
        };
        self.procs
            .get_mut(&proc)
            .unwrap()
            .delivered_death
            .retain(|x| *x != d);
        let death = self.deaths.get_mut(&d).unwrap();
        death.queued = false;
        if death.kind == DeathKind::DeadBinderAndClear {
            death.kind = DeathKind::Clear;
            death.queued = true;
            self.queue_death_to_thread_or_proc(proc, tid, d);
        }
    }

    // ---- thread / process release -------------------------------------------

    /// `binder_thread_release` (BINDER_THREAD_EXIT, or the thread vanished).
    pub fn thread_exit(&mut self, proc: ProcId, tid: u32) {
        let Some(p) = self.procs.get_mut(&proc) else {
            return;
        };
        let Some(mut t) = p.threads.remove(&tid) else {
            return;
        };
        p.waiting_threads.retain(|&x| x != tid);
        t.is_dead = true;
        let mut send_reply = None;
        let mut cur = t.transaction_stack;
        if let Some(c) = cur
            && self
                .txns
                .get(&c)
                .map(|x| x.to_thread == Some(tid) && x.to_proc == Some(proc))
                .unwrap_or(false)
        {
            send_reply = Some(c);
        }
        while let Some(c) = cur {
            let Some(tx) = self.txns.get_mut(&c) else {
                break;
            };
            if tx.to_thread == Some(tid) && tx.to_proc == Some(proc) {
                tx.to_proc = None;
                tx.to_thread = None;
                cur = tx.to_parent;
                if let Some(key) = tx.buffer.take()
                    && let Some(b) = self.buffers.get_mut(&key)
                {
                    b.transaction = None;
                }
            } else if tx.from == Some((proc, tid)) {
                tx.from = None;
                cur = tx.from_parent;
            } else {
                break;
            }
        }
        if let Some(c) = send_reply {
            self.send_failed_reply(c, BR_DEAD_REPLY);
        }
        self.release_work(proc, t.todo);
        self.poll_dirty.insert(proc);
    }

    /// `binder_release_work`: fail synchronous transactions nobody will read.
    fn release_work(&mut self, _proc: ProcId, list: VecDeque<Work>) {
        for w in list {
            match w {
                Work::Transaction(t) => {
                    let Some(tx) = self.txns.get(&t) else {
                        continue;
                    };
                    let has_node = tx
                        .buffer
                        .and_then(|k| self.buffers.get(&k))
                        .and_then(|b| b.target_node)
                        .is_some();
                    if has_node && tx.flags & TF_ONE_WAY == 0 {
                        self.send_failed_reply(t, BR_DEAD_REPLY);
                    } else {
                        self.free_txn(t);
                    }
                }
                Work::Death(d) => {
                    if self
                        .deaths
                        .get(&d)
                        .map(|x| x.kind != DeathKind::DeadBinder)
                        .unwrap_or(false)
                    {
                        self.deaths.remove(&d);
                    } else if let Some(x) = self.deaths.get_mut(&d) {
                        x.queued = false;
                    }
                }
                Work::Node(n) => {
                    if let Some(n) = self.nodes.get_mut(&n) {
                        n.work_queued = false;
                    }
                }
                _ => {}
            }
        }
    }

    /// The process closed its binder fd or died (`binder_deferred_release`).
    /// Returns the parked threads made runnable.
    pub fn release_proc(&mut self, proc: ProcId, mem: &mut dyn Mem) -> Vec<Wakeup> {
        let Some(p) = self.procs.get_mut(&proc) else {
            return self.take_wakeups();
        };
        if p.dead {
            return self.take_wakeups();
        }
        p.dead = true;
        let ctx = p.context as usize;
        if let Some(n) = self.contexts[ctx].mgr_node
            && self
                .nodes
                .get(&n)
                .map(|n| n.proc == Some(proc))
                .unwrap_or(true)
        {
            self.contexts[ctx].mgr_node = None;
        }
        let tids: Vec<u32> = self.procs[&proc].threads.keys().copied().collect();
        for tid in tids {
            self.thread_exit(proc, tid);
        }
        // Nodes: notify every holder that asked for a death notification.
        let nodes: Vec<NodeId> = self.procs[&proc].nodes.values().copied().collect();
        self.procs.get_mut(&proc).unwrap().nodes.clear();
        for nid in nodes {
            let Some(n) = self.nodes.get_mut(&nid) else {
                continue;
            };
            let async_todo: VecDeque<Work> =
                n.async_todo.drain(..).map(Work::Transaction).collect();
            let queued = n.work_queued;
            n.work_queued = false;
            n.proc = None;
            n.local_strong = 0;
            n.local_weak = 0;
            let holders: Vec<ProcId> = n.refs.iter().copied().collect();
            self.release_work(proc, async_todo);
            if queued {
                self.dequeue_work(proc, Work::Node(nid));
            }
            if holders.is_empty() {
                self.nodes.remove(&nid);
                continue;
            }
            for h in holders {
                let Some(&desc) = self.procs.get(&h).and_then(|p| p.node_to_desc.get(&nid)) else {
                    continue;
                };
                let Some(d) = self.procs[&h].refs_by_desc[&desc].death else {
                    continue;
                };
                if let Some(death) = self.deaths.get_mut(&d)
                    && !death.queued
                {
                    death.kind = DeathKind::DeadBinder;
                    death.queued = true;
                    self.enqueue_proc_work(h, Work::Death(d));
                }
            }
        }
        // Outgoing refs.
        let descs: Vec<u32> = self.procs[&proc].refs_by_desc.keys().copied().collect();
        for d in descs {
            self.delete_ref(proc, d);
        }
        let todo = std::mem::take(&mut self.procs.get_mut(&proc).unwrap().todo);
        self.release_work(proc, todo);
        let delivered: VecDeque<Work> =
            std::mem::take(&mut self.procs.get_mut(&proc).unwrap().delivered_death)
                .into_iter()
                .map(Work::Death)
                .collect();
        self.release_work(proc, delivered);
        // Buffers still held by the process.
        let keys: Vec<(ProcId, u64)> = self
            .buffers
            .keys()
            .filter(|k| k.0 == proc)
            .copied()
            .collect();
        for k in keys {
            if let Some(t) = self.buffers[&k].transaction {
                if self.txns.get(&t).map(|t| t.from.is_some()).unwrap_or(false) {
                    self.send_failed_reply(t, BR_DEAD_REPLY);
                } else {
                    self.free_txn(t);
                }
            }
            // binder_alloc_deferred_release frees the memory only: the refs
            // these buffers held died with the process's nodes and handles.
            if let Some(b) = self.buffers.remove(&k) {
                for (_, tok) in b.fd_fixups {
                    mem.drop_fd(tok);
                }
                mem.free_recv(k.0, k.1);
            }
        }
        // Transactions still pointing at this proc as the target.
        let stale: Vec<TxnId> = self
            .txns
            .iter()
            .filter(|(_, t)| t.to_proc == Some(proc))
            .map(|(&id, _)| id)
            .collect();
        for t in stale {
            if self.txns.get(&t).map(|t| t.from.is_some()).unwrap_or(false) {
                self.send_failed_reply(t, BR_DEAD_REPLY);
            } else {
                self.txns.remove(&t);
            }
        }
        self.procs.remove(&proc);
        self.poll_dirty.remove(&proc);
        let mut w = self.take_wakeups();
        w.retain(|x| x.proc != proc);
        w
    }

    // ---- small ioctls ---------------------------------------------------------

    pub fn get_extended_error(&mut self, proc: ProcId, tid: u32) -> BinderExtendedError {
        let t = self.thread_mut(proc, tid);
        let ee = t.ee;
        t.ee = BinderExtendedError {
            id: 0,
            command: BR_OK,
            param: 0,
        };
        ee
    }

    pub fn node_debug_info(&self, proc: ProcId, after_ptr: u64) -> BinderNodeDebugInfo {
        let mut best: Option<&Node> = None;
        if let Some(p) = self.procs.get(&proc) {
            for (&ptr, nid) in p.nodes.iter() {
                if ptr > after_ptr {
                    let n = &self.nodes[nid];
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

    /// `BINDER_GET_NODE_INFO_FOR_REF` (context manager only).
    pub fn node_info_for_ref(
        &self,
        proc: ProcId,
        info: BinderNodeInfoForRef,
    ) -> Result<BinderNodeInfoForRef, i32> {
        if info.strong_count != 0
            || info.weak_count != 0
            || info.reserved1 != 0
            || info.reserved2 != 0
            || info.reserved3 != 0
        {
            return Err(-libc::EINVAL);
        }
        let p = self.procs.get(&proc).ok_or(-libc::EINVAL)?;
        let mgr = self.contexts[p.context as usize]
            .mgr_node
            .ok_or(-libc::EPERM)?;
        if self.nodes.get(&mgr).and_then(|n| n.proc) != Some(proc) {
            return Err(-libc::EPERM);
        }
        let nid = self
            .ref_node(proc, info.handle, false)
            .ok_or(-libc::EINVAL)?;
        let n = &self.nodes[&nid];
        Ok(BinderNodeInfoForRef {
            handle: info.handle,
            strong_count: (n.local_strong + n.internal_strong) as u32,
            weak_count: n.local_weak as u32,
            ..Default::default()
        })
    }

    // ---- the read path ---------------------------------------------------------

    /// `binder_thread_read`.
    fn thread_read(
        &mut self,
        proc: ProcId,
        tid: u32,
        size: usize,
        non_block: bool,
        mem: &mut dyn Mem,
    ) -> ReadOutcome {
        if size < 4 {
            return ReadOutcome::Data(Vec::new());
        }
        let mut out: Vec<u8> = Vec::with_capacity(size.min(512));
        write_pod(&mut out, &BR_NOOP);
        'retry: loop {
            let (wait_for_proc_work, has_work) = {
                let p = &self.procs[&proc];
                let t = &p.threads[&tid];
                let w = Self::available_for_proc_work(t);
                (w, Self::has_work(p, t, w))
            };
            if !has_work {
                if non_block {
                    return ReadOutcome::Again;
                }
                let p = self.procs.get_mut(&proc).unwrap();
                let t = p.threads.get_mut(&tid).unwrap();
                t.parked = true;
                t.looper |= LOOPER_WAITING;
                if wait_for_proc_work && !p.waiting_threads.contains(&tid) {
                    p.waiting_threads.push_back(tid);
                }
                return ReadOutcome::Blocked;
            }
            {
                let p = self.procs.get_mut(&proc).unwrap();
                p.waiting_threads.retain(|&x| x != tid);
                let t = p.threads.get_mut(&tid).unwrap();
                t.parked = false;
                t.looper &= !LOOPER_WAITING;
            }
            loop {
                let from_thread = !self.procs[&proc].threads[&tid].todo.is_empty();
                if !from_thread && (self.procs[&proc].todo.is_empty() || !wait_for_proc_work) {
                    if out.len() == 4 && !self.procs[&proc].threads[&tid].looper_need_return {
                        continue 'retry;
                    }
                    break 'retry;
                }
                if size - out.len() < MIN_READ_SPACE {
                    break 'retry;
                }
                let w = {
                    let p = self.procs.get_mut(&proc).unwrap();
                    let w = if from_thread {
                        p.threads.get_mut(&tid).unwrap().todo.pop_front()
                    } else {
                        p.todo.pop_front()
                    };
                    let t = p.threads.get_mut(&tid).unwrap();
                    if t.todo.is_empty() {
                        t.process_todo = false;
                    }
                    w.unwrap()
                };
                self.poll_dirty.insert(proc);
                match w {
                    Work::ReturnError => {
                        let t = self.thread_mut(proc, tid);
                        let cmd = std::mem::replace(&mut t.return_error, BR_OK);
                        write_pod(&mut out, &cmd);
                    }
                    Work::ReplyError => {
                        let t = self.thread_mut(proc, tid);
                        let cmd = std::mem::replace(&mut t.reply_error, BR_OK);
                        write_pod(&mut out, &cmd);
                    }
                    Work::TransactionComplete => write_pod(&mut out, &BR_TRANSACTION_COMPLETE),
                    Work::OnewaySpamSuspect => write_pod(&mut out, &BR_ONEWAY_SPAM_SUSPECT),
                    Work::Node(nid) => self.emit_node(nid, &mut out),
                    Work::Death(d) => {
                        let Some(death) = self.deaths.get(&d) else {
                            continue;
                        };
                        let cookie = death.cookie;
                        if death.kind == DeathKind::Clear {
                            self.deaths.remove(&d);
                            write_pod(&mut out, &BR_CLEAR_DEATH_NOTIFICATION_DONE);
                            write_pod(&mut out, &cookie);
                        } else {
                            self.procs.get_mut(&proc).unwrap().delivered_death.push(d);
                            write_pod(&mut out, &BR_DEAD_BINDER);
                            write_pod(&mut out, &cookie);
                            // DEAD_BINDER notifications can cause transactions.
                            break 'retry;
                        }
                    }
                    Work::Transaction(txid) => {
                        if self.emit_transaction(proc, tid, txid, &mut out, mem) {
                            break 'retry;
                        }
                    }
                }
            }
        }
        // Ask for another looper thread if none is idle.
        let p = &self.procs[&proc];
        let t = &p.threads[&tid];
        if p.requested_threads == 0
            && p.waiting_threads.is_empty()
            && p.requested_threads_started < p.max_threads
            && t.looper & (LOOPER_REGISTERED | LOOPER_ENTERED) != 0
        {
            self.procs.get_mut(&proc).unwrap().requested_threads += 1;
            out[..4].copy_from_slice(&BR_SPAWN_LOOPER.to_le_bytes());
        }
        ReadOutcome::Data(out)
    }

    fn emit_node(&mut self, nid: NodeId, out: &mut Vec<u8>) {
        let Some(n) = self.nodes.get_mut(&nid) else {
            return;
        };
        n.work_queued = false;
        let (strong, weak) = Self::node_wants(n);
        let had_weak = n.has_weak_ref;
        let had_strong = n.has_strong_ref;
        if weak && !had_weak {
            n.has_weak_ref = true;
            n.pending_weak = true;
            n.local_weak += 1;
        }
        if strong && !had_strong {
            n.has_strong_ref = true;
            n.pending_strong = true;
            n.local_strong += 1;
        }
        if !strong && had_strong {
            n.has_strong_ref = false;
        }
        if !weak && had_weak {
            n.has_weak_ref = false;
        }
        let (ptr, cookie) = (n.ptr, n.cookie);
        if !weak && !strong {
            self.free_node(nid);
        }
        let mut put = |cmd: u32| {
            write_pod(out, &cmd);
            write_pod(out, &ptr);
            write_pod(out, &cookie);
        };
        if weak && !had_weak {
            put(BR_INCREFS);
        }
        if strong && !had_strong {
            put(BR_ACQUIRE);
        }
        if !strong && had_strong {
            put(BR_RELEASE);
        }
        if !weak && had_weak {
            put(BR_DECREFS);
        }
    }

    /// Emit BR_TRANSACTION(_SEC_CTX) / BR_REPLY. Returns false if the
    /// transaction was dropped (fd install failed) and reading continues.
    fn emit_transaction(
        &mut self,
        proc: ProcId,
        tid: u32,
        txid: TxnId,
        out: &mut Vec<u8>,
        mem: &mut dyn Mem,
    ) -> bool {
        let Some(t) = self.txns.get(&txid) else {
            return false;
        };
        let Some(key) = t.buffer else {
            self.free_txn(txid);
            return false;
        };
        let (code, flags, euid, from) = (t.code, t.flags, t.sender_euid, t.from);
        // binder_apply_fd_fixups: install fds into the reader now.
        let fixups = std::mem::take(&mut self.buffers.get_mut(&key).unwrap().fd_fixups);
        let mut ok = true;
        for &(off, tok) in &fixups {
            if ok {
                match mem.install_fd(proc, tok) {
                    Some(fd) => mem.write_recv(proc, key.1 + off as u64, &fd.to_le_bytes()),
                    None => ok = false,
                }
            }
            mem.drop_fd(tok);
        }
        if !ok {
            tlog!("proc={proc} fd fixups failed for txn {txid}");
            if flags & TF_ONE_WAY == 0 && self.buffers[&key].target_node.is_some() {
                self.send_failed_reply(txid, BR_FAILED_REPLY);
            } else {
                self.free_txn(txid);
            }
            self.release_buffer(key, mem);
            return false;
        }
        let b = self.buffers.get_mut(&key).unwrap();
        b.allow_user_free = true;
        let mut td = BinderTransactionData::default();
        let cmd = match b.target_node {
            Some(n) => {
                let n = &self.nodes[&n];
                td.target = n.ptr;
                td.cookie = n.cookie;
                if b.secctx != 0 {
                    BR_TRANSACTION_SEC_CTX
                } else {
                    BR_TRANSACTION
                }
            }
            None => BR_REPLY,
        };
        td.code = code;
        td.flags = flags;
        td.sender_euid = euid;
        td.sender_pid = from
            .and_then(|(fp, _)| self.procs.get(&fp))
            .map(|p| p.ns_pid)
            .unwrap_or(0);
        td.data_size = b.data_size as u64;
        td.offsets_size = b.offsets_size as u64;
        td.data_buffer = key.1;
        td.data_offsets = key.1 + align_up(b.data_size, 8) as u64;
        write_pod(out, &cmd);
        if cmd == BR_TRANSACTION_SEC_CTX {
            write_pod(
                out,
                &BinderTransactionDataSecctx {
                    transaction_data: td,
                    secctx: b.secctx,
                },
            );
        } else {
            write_pod(out, &td);
        }
        tlog!("emit proc={proc} tid={tid} txn={txid} cmd={cmd:#x} code={code:#x}");
        if cmd != BR_REPLY && flags & TF_ONE_WAY == 0 {
            let cur = self.procs[&proc].threads[&tid].transaction_stack;
            let tx = self.txns.get_mut(&txid).unwrap();
            tx.to_parent = cur;
            tx.to_thread = Some(tid);
            tx.to_proc = Some(proc);
            self.thread_mut(proc, tid).transaction_stack = Some(txid);
        } else {
            self.free_txn(txid);
        }
        true
    }

    // ---- introspection for tests ----------------------------------------------

    #[cfg(test)]
    pub(crate) fn ref_counts(&self, proc: ProcId, desc: u32) -> Option<(i32, i32)> {
        self.procs
            .get(&proc)?
            .refs_by_desc
            .get(&desc)
            .map(|r| (r.strong, r.weak))
    }

    /// Internal consistency checks (used by tests and the fuzzer).
    pub fn check_invariants(&self) {
        for (&pid, p) in &self.procs {
            for (&d, r) in &p.refs_by_desc {
                assert!(r.strong >= 0 && r.weak >= 0);
                assert_eq!(p.node_to_desc.get(&r.node), Some(&d));
                let n = self.nodes.get(&r.node).expect("ref to freed node");
                assert!(n.refs.contains(&pid));
            }
            for &tid in &p.waiting_threads {
                assert!(p.threads.get(&tid).map(|t| t.parked).unwrap_or(false));
            }
        }
        for (id, n) in &self.nodes {
            assert!(
                n.internal_strong >= 0 && n.local_strong >= 0 && n.local_weak >= 0,
                "node {id} owner {:?} ptr {:#x}: internal_strong {} local_strong {} local_weak {}",
                n.proc,
                n.ptr,
                n.internal_strong,
                n.local_strong,
                n.local_weak
            );
        }
    }

    /// Number of live transactions, buffers and nodes (leak checks).
    pub fn counts(&self) -> (usize, usize, usize) {
        (self.txns.len(), self.buffers.len(), self.nodes.len())
    }
}

/// The (sg offset, length) of a validated PTR object `parent` (an index into
/// the objects already translated).
fn parent_ptr(ptrs: &[Option<(usize, usize)>], parent: u64) -> Option<(usize, usize)> {
    let i = usize::try_from(parent).ok()?;
    *ptrs.get(i)?
}

/// Does the BC_* stream contain `want`? (Walks command sizes; stops at the
/// first unknown command.)
fn has_cmd(buf: &[u8], want: u32) -> bool {
    let mut off = 0;
    while let Some(cmd) = read_pod::<u32>(buf, off) {
        if cmd == want {
            return true;
        }
        off += 4 + ((cmd >> 16) & 0x3fff) as usize;
    }
    false
}
