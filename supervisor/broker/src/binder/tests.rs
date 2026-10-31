//! Unit tests for the broker state machine. They drive the broker exactly as
//! libbinder would: build a BC_* command stream, issue `BINDER_WRITE_READ`
//! through the fake userspace, and decode the BR_* stream the call is
//! answered with, asserting the kernel's observable behaviour.
#![allow(clippy::field_reassign_with_default)] // readable struct-by-field setup

use crate::binder::driver::*;
use crate::binder::uapi::*;
use crate::fake::{FakeUserspace, Fault};

// ---- command-stream helpers -------------------------------------------

#[derive(Default)]
struct Bc(Vec<u8>);
impl Bc {
    fn cmd(mut self, c: u32) -> Self {
        self.0.extend_from_slice(&c.to_le_bytes());
        self
    }
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_le_bytes());
        self
    }
    fn td(mut self, td: &BinderTransactionData) -> Self {
        self.0.extend_from_slice(pod_bytes(td));
        self
    }
    fn done(self) -> Vec<u8> {
        self.0
    }
}

/// Decode a BR_* stream into (cmd, payload-bytes) pairs.
fn decode(buf: &[u8]) -> Vec<(u32, Vec<u8>)> {
    let mut out = Vec::new();
    let mut off = 0;
    while off + 4 <= buf.len() {
        let cmd = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap());
        off += 4;
        let plen = br_payload_len(cmd);
        let p = buf[off..(off + plen).min(buf.len())].to_vec();
        off += plen;
        out.push((cmd, p));
    }
    out
}

fn br_payload_len(cmd: u32) -> usize {
    match cmd {
        BR_NOOP
        | BR_TRANSACTION_COMPLETE
        | BR_SPAWN_LOOPER
        | BR_DEAD_REPLY
        | BR_FAILED_REPLY
        | BR_OK
        | BR_ONEWAY_SPAM_SUSPECT => 0,
        BR_TRANSACTION | BR_REPLY => std::mem::size_of::<BinderTransactionData>(),
        BR_TRANSACTION_SEC_CTX => std::mem::size_of::<BinderTransactionDataSecctx>(),
        BR_INCREFS | BR_ACQUIRE | BR_RELEASE | BR_DECREFS => std::mem::size_of::<BinderPtrCookie>(),
        BR_DEAD_BINDER | BR_CLEAR_DEATH_NOTIFICATION_DONE | BR_CLEAR_FREEZE_NOTIFICATION_DONE => 8,
        BR_FROZEN_BINDER => std::mem::size_of::<BinderFrozenStateInfo>(),
        BR_ERROR => 4,
        _ => 0,
    }
}

fn find(stream: &[(u32, Vec<u8>)], cmd: u32) -> Option<&Vec<u8>> {
    stream.iter().find(|(c, _)| *c == cmd).map(|(_, p)| p)
}

fn td_from(p: &[u8]) -> BinderTransactionData {
    read_pod(p, 0).unwrap()
}

// A standard two-process setup: a server that is the context manager, and a
// client. recv regions far apart so target addresses are unambiguous.
struct Setup {
    b: BinderDriver,
    us: FakeUserspace,
    server: ProcId,
    client: ProcId,
}

impl Setup {
    /// Two processes, without a context manager.
    fn bare() -> Setup {
        Self::bare_on(crate::Guest::Android)
    }

    /// [`Setup::bare`] on `guest`'s driver.
    fn bare_on(guest: crate::Guest) -> Setup {
        let mut b = BinderDriver::with_guest(guest);
        let mut us = FakeUserspace::default();
        let server = b.open(1000, 1000, 0, false);
        let client = b.open(1001, 10001, 0, false);
        us.add_proc(server, 0x8000_0000);
        us.add_proc(client, 0x9000_0000);
        Setup {
            b,
            us,
            server,
            client,
        }
    }

    fn ioctl(&mut self, proc: ProcId, tid: u32, cmd: u32, arg: u64) -> Call {
        self.us.ioctl(&mut self.b, proc, tid, cmd, arg)
    }

    fn wr(&mut self, proc: ProcId, tid: u32, write: &[u8], read_size: usize) -> Call {
        let c = self.us.write_read(&mut self.b, proc, tid, write, read_size);
        self.b.check_invariants();
        c
    }

    /// A write-only BINDER_WRITE_READ that must succeed.
    fn write(&mut self, proc: ProcId, tid: u32, write: &[u8]) {
        let c = self.wr(proc, tid, write, 0);
        assert_eq!(self.us.outcome(c).errno, 0);
    }

    /// The decoded stream `call` was answered with.
    fn stream(&self, call: Call) -> Vec<(u32, Vec<u8>)> {
        let o = self.us.outcome(call);
        assert_eq!(o.errno, 0);
        decode(&o.read)
    }

    /// A read that must not block.
    fn read(&mut self, proc: ProcId, tid: u32, size: usize) -> Vec<(u32, Vec<u8>)> {
        let c = self.wr(proc, tid, &[], size);
        self.stream(c)
    }
}

/// The server's thread 2 is a looper (joinThreadPool); the client's thread 1
/// is a plain caller.
fn setup() -> Setup {
    setup_on(crate::Guest::Android)
}

fn setup_on(guest: crate::Guest) -> Setup {
    let mut s = Setup::bare_on(guest);
    let c = s.ioctl(s.server, 2, BINDER_SET_CONTEXT_MGR, 0);
    assert_eq!(s.us.outcome(c).errno, 0);
    s.write(s.server, 2, &Bc::default().cmd(BC_ENTER_LOOPER).done());
    s
}

/// A transaction to the context manager whose data is `obj` (one object at
/// offset 0), from the client's memory.
fn txn_with_object(s: &mut Setup, obj: &[u8], flags: u32) -> BinderTransactionData {
    let data_addr = 0x9001_0000u64;
    let off_addr = 0x9002_0000u64;
    s.us.poke(s.client, data_addr, obj);
    s.us.poke(s.client, off_addr, &0u64.to_le_bytes());
    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.code = 1;
    td.flags = flags;
    td.data_size = obj.len() as u64;
    td.offsets_size = 8;
    td.data_buffer = data_addr;
    td.data_offsets = off_addr;
    td
}

// ---- tests ------------------------------------------------------------

#[test]
fn version_and_ctx_mgr() {
    let mut s = Setup::bare();
    let c = s.ioctl(s.server, 1, BINDER_VERSION, 0x8100_0000);
    assert_eq!(s.us.outcome(c).errno, 0);
    let v: BinderVersion = read_pod(&s.us.peek(s.server, 0x8100_0000, 4), 0).unwrap();
    assert_eq!(v.protocol_version, 8);
    let c = s.ioctl(s.server, 1, BINDER_SET_CONTEXT_MGR, 0);
    assert_eq!(s.us.outcome(c).errno, 0);
    // A second context manager is EBUSY.
    let c = s.ioctl(s.client, 1, BINDER_SET_CONTEXT_MGR, 0);
    assert_eq!(s.us.outcome(c).errno, libc::EBUSY);
}

#[test]
fn register_service_and_transact() {
    let mut s = setup();
    // Client sends a transaction to the context manager (handle 0) that
    // contains one BINDER_TYPE_BINDER (registering a service object).
    let mut fbo = FlatBinderObject::default();
    fbo.hdr_type = BINDER_TYPE_BINDER;
    fbo.flags = FLAT_BINDER_FLAG_ACCEPTS_FDS;
    fbo.binder_or_handle = 0xBEEF_0000; // client's local BBinder ptr
    fbo.cookie = 0xCAFE;
    let td = txn_with_object(&mut s, pod_bytes(&fbo), 0);

    // The server has a thread blocked in a read, as servicemanager does.
    let park = s.wr(s.server, 2, &[], 256);
    assert!(s.us.is_pending(park));

    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );
    // The server's read was answered with BR_TRANSACTION carrying a
    // translated HANDLE.
    let stream = s.stream(park);
    let tp = find(&stream, BR_TRANSACTION).expect("server gets BR_TRANSACTION");
    let td2 = td_from(tp);
    assert_eq!(td2.code, 1);
    assert_eq!(td2.sender_pid, 1001);
    assert_eq!(td2.sender_euid, 10001);
    // The server's copy of the object is a HANDLE now.
    let obj = s.us.peek(
        s.server,
        td2.data_buffer,
        std::mem::size_of::<FlatBinderObject>(),
    );
    let fbo2: FlatBinderObject = read_pod(&obj, 0).unwrap();
    assert_eq!(fbo2.hdr_type, BINDER_TYPE_HANDLE);
    let handle = fbo2.binder_or_handle as u32;
    assert!(handle >= 1);
    // The server now holds a strong ref to the client's node.
    let (strong, _weak) = s.b.ref_counts(s.server, handle).unwrap();
    assert_eq!(strong, 1);

    // Client is now blocked awaiting the reply (sync call).
    let cblock = s.wr(s.client, 1, &[], 256);
    assert!(s.us.is_pending(cblock));

    // Server replies; client wakes and, on the same read, receives its
    // deferred node handshake (INCREFS/ACQUIRE), TRANSACTION_COMPLETE and REPLY.
    let mut rtd = BinderTransactionData::default();
    rtd.code = 0;
    let wr = Bc::default().cmd(BC_REPLY).td(&rtd).done();
    s.wr(s.server, 2, &wr, 256);
    let cs = s.stream(cblock);
    assert!(find(&cs, BR_TRANSACTION_COMPLETE).is_some());
    assert!(find(&cs, BR_REPLY).is_some());
    assert!(
        find(&cs, BR_INCREFS).is_some(),
        "owner gets deferred BR_INCREFS"
    );
    assert!(
        find(&cs, BR_ACQUIRE).is_some(),
        "owner gets deferred BR_ACQUIRE"
    );
}

/// A data_size near u64::MAX must fail the transaction, not overflow the
/// alignment (a panic in debug builds) or wrap to a tiny buffer.
#[test]
fn huge_data_size_fails() {
    let mut s = setup();
    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.data_size = u64::MAX;
    td.data_buffer = 0x9001_0000;
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );
    let r = s.read(s.client, 1, 256);
    assert!(find(&r, BR_FAILED_REPLY).is_some());
}

#[test]
fn sync_call_and_reply_routing() {
    let mut s = setup();
    // Client calls the context-manager node directly (handle 0), server replies.
    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.code = 42;
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );

    // Server receives the transaction.
    let stream = s.read(s.server, 2, 256);
    assert!(find(&stream, BR_TRANSACTION).is_some());

    // Client, meanwhile, blocks waiting for the reply.
    let rc = s.wr(s.client, 1, &[], 256);
    assert!(s.us.is_pending(rc), "client with no reply yet must block");

    // Server replies, which answers the client's read with the reply.
    let mut rtd = BinderTransactionData::default();
    rtd.code = 0;
    let wr = Bc::default().cmd(BC_REPLY).td(&rtd).done();
    s.wr(s.server, 2, &wr, 256);
    assert!(
        find(&s.stream(rc), BR_REPLY).is_some(),
        "client gets BR_REPLY"
    );
}

#[test]
fn ohos_ioctls_are_only_openharmonys() {
    let mut s = Setup::bare();
    for cmd in [
        BINDER_FEATURE_SET,
        BINDER_GET_ACCESS_TOKEN,
        BINDER_GET_SENDER_INFO,
    ] {
        let c = s.ioctl(s.server, 1, cmd, 0x8100_0000);
        assert_eq!(s.us.outcome(c).errno, libc::EINVAL, "{cmd:#x} on Android");
    }
    let mut s = Setup::bare_on(crate::Guest::Ohos);
    let c = s.ioctl(s.server, 1, BINDER_FEATURE_SET, 0x8100_0000);
    assert_eq!(s.us.outcome(c).errno, 0);
    let features: u64 = read_pod(&s.us.peek(s.server, 0x8100_0000, 8), 0).unwrap();
    assert_eq!(features, OHOS_FEATURES);
}

#[test]
fn ohos_readers_get_the_senders_tokens() {
    let mut s = setup_on(crate::Guest::Ohos);
    let (server, client) = (s.server, s.client);
    s.us.tokens.insert(s.client, (0x2800_0001, 0x2800_0002));
    s.us.tokens.insert(s.server, (0x2000_0003, 0));
    let sender_info = |s: &mut Setup, proc: ProcId, tid: u32| -> BinderSenderInfo {
        let c = s.ioctl(proc, tid, BINDER_GET_SENDER_INFO, 0x8100_0000);
        assert_eq!(s.us.outcome(c).errno, 0);
        let mem = s.us.peek(proc, 0x8100_0000, size_of::<BinderSenderInfo>());
        let info: BinderSenderInfo = read_pod(&mem, 0).unwrap();
        let c = s.ioctl(proc, tid, BINDER_GET_ACCESS_TOKEN, 0x8100_0000);
        assert_eq!(s.us.outcome(c).errno, 0);
        let mem = s.us.peek(proc, 0x8100_0000, size_of::<AccessToken>());
        assert_eq!(read_pod::<AccessToken>(&mem, 0).unwrap(), info.tokens);
        info
    };
    let token = |sender_tokenid, first_tokenid| AccessToken {
        sender_tokenid,
        first_tokenid,
        reserved: [0; 2],
    };
    let mut td = BinderTransactionData::default();
    td.code = 42;
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );
    assert!(find(&s.read(s.server, 2, 256), BR_TRANSACTION).is_some());
    let info = sender_info(&mut s, server, 2);
    assert_eq!(info.tokens, token(0x2800_0001, 0x2800_0002));
    assert_eq!(info.sender_pid_nr, 1001, "the client's pid");
    // The caller reads the reply's: the server's tokens, and no pid.
    let rc = s.wr(s.client, 1, &[], 256);
    let rtd = BinderTransactionData::default();
    s.wr(
        s.server,
        2,
        &Bc::default().cmd(BC_REPLY).td(&rtd).done(),
        256,
    );
    assert!(find(&s.stream(rc), BR_REPLY).is_some());
    let info = sender_info(&mut s, client, 1);
    assert_eq!(
        (info.tokens, info.sender_pid_nr),
        (token(0x2000_0003, 0), 0)
    );
    // A oneway call carries tokens, not a pid.
    td.flags = TF_ONE_WAY;
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );
    assert!(find(&s.read(s.server, 2, 256), BR_TRANSACTION).is_some());
    let info = sender_info(&mut s, server, 2);
    assert_eq!(
        (info.tokens, info.sender_pid_nr),
        (token(0x2800_0001, 0x2800_0002), 0)
    );
}

#[test]
fn fd_passing() {
    // The context manager's node accepts fds (SET_CONTEXT_MGR_EXT).
    let mut s = Setup::bare();
    let mut ext = FlatBinderObject::default();
    ext.flags = FLAT_BINDER_FLAG_ACCEPTS_FDS;
    s.us.poke(s.server, 0x8100_0000, pod_bytes(&ext));
    let c = s.ioctl(s.server, 2, BINDER_SET_CONTEXT_MGR_EXT, 0x8100_0000);
    assert_eq!(s.us.outcome(c).errno, 0);
    s.write(s.server, 2, &Bc::default().cmd(BC_ENTER_LOOPER).done());

    // Client sends a transaction with a BINDER_TYPE_FD object.
    let mut fdo = BinderFdObject::default();
    fdo.hdr_type = BINDER_TYPE_FD;
    fdo.fd_or_pad = 7; // client's fd 7
    let td = txn_with_object(&mut s, pod_bytes(&fdo), TF_ACCEPT_FDS);
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );

    let stream = s.read(s.server, 2, 256);
    let tp = find(&stream, BR_TRANSACTION).expect("server gets BR_TRANSACTION with fd");
    let td2 = td_from(tp);
    let obj = s.us.peek(
        s.server,
        td2.data_buffer,
        std::mem::size_of::<BinderFdObject>(),
    );
    let fdo2: BinderFdObject = read_pod(&obj, 0).unwrap();
    let new_fd = fdo2.fd_or_pad as u32;
    assert_ne!(new_fd, 7);
    assert!(s.us.procs[&s.server].installed_fds.contains(&new_fd));
    // The broker no longer holds a copy of the installed fd.
    assert!(s.us.captured.is_empty());
}

#[test]
fn oneway_does_not_block_and_completes() {
    let mut s = setup();
    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.flags = TF_ONE_WAY;
    // Oneway returns TRANSACTION_COMPLETE immediately to the caller.
    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    let c = s.wr(s.client, 1, &w, 256);
    assert!(find(&s.stream(c), BR_TRANSACTION_COMPLETE).is_some());
    // Server gets the transaction (as proc work).
    assert!(find(&s.read(s.server, 2, 256), BR_TRANSACTION).is_some());
}

#[test]
fn death_notification_fires_on_release() {
    let mut s = setup();
    // Client registers a service object, so the server gets a handle to it.
    let mut fbo = FlatBinderObject::default();
    fbo.hdr_type = BINDER_TYPE_BINDER;
    fbo.binder_or_handle = 0xBEEF;
    let td = txn_with_object(&mut s, pod_bytes(&fbo), 0);
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );
    let td2 = td_from(find(&s.read(s.server, 2, 256), BR_TRANSACTION).unwrap());
    let obj = s.us.peek(
        s.server,
        td2.data_buffer,
        std::mem::size_of::<FlatBinderObject>(),
    );
    let handle = read_pod::<FlatBinderObject>(&obj, 0)
        .unwrap()
        .binder_or_handle as u32;
    // Server replies to the registration so its thread returns to proc work;
    // it reads its own TRANSACTION_COMPLETE on the same call.
    let rtd = BinderTransactionData::default();
    s.wr(
        s.server,
        2,
        &Bc::default().cmd(BC_REPLY).td(&rtd).done(),
        256,
    );
    // Drain the client so the outstanding call completes.
    s.read(s.client, 1, 512);

    // Server requests a death notification on that handle, then blocks.
    let cookie = 0x1234_5678u64;
    let w = Bc::default()
        .cmd(BC_REQUEST_DEATH_NOTIFICATION)
        .u32(handle)
        .u64(cookie)
        .done();
    s.write(s.server, 2, &w);
    let park = s.wr(s.server, 2, &[], 256);
    assert!(s.us.is_pending(park));

    // Client dies: the server's read is answered with BR_DEAD_BINDER.
    s.b.release_proc(s.client, &mut s.us);
    let stream = s.stream(park);
    let p = find(&stream, BR_DEAD_BINDER).expect("server gets BR_DEAD_BINDER");
    assert_eq!(u64::from_le_bytes(p[..8].try_into().unwrap()), cookie);
}

#[test]
fn node_ref_handshake() {
    // When a node first gets an incoming ref, its owner must receive
    // BR_INCREFS then BR_ACQUIRE.
    let mut s = setup();
    let mut fbo = FlatBinderObject::default();
    fbo.hdr_type = BINDER_TYPE_BINDER;
    fbo.binder_or_handle = 0xABCD;
    let td = txn_with_object(&mut s, pod_bytes(&fbo), 0);
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );
    // drive the server to translate (creates the incoming ref), then reply
    s.read(s.server, 2, 256);
    let rtd = BinderTransactionData::default();
    s.wr(
        s.server,
        2,
        &Bc::default().cmd(BC_REPLY).td(&rtd).done(),
        256,
    );

    // The client (owner) receives the deferred node handshake with the reply.
    let cs = s.read(s.client, 1, 512);
    assert!(find(&cs, BR_INCREFS).is_some(), "owner gets BR_INCREFS");
    assert!(find(&cs, BR_ACQUIRE).is_some(), "owner gets BR_ACQUIRE");
}

#[test]
fn bad_command_is_einval() {
    let mut s = Setup::bare();
    let c = s.wr(s.client, 1, &Bc::default().cmd(0xdead_beef).done(), 0);
    assert_eq!(s.us.outcome(c).errno, libc::EINVAL);
}

// ---- the read lifecycle ------------------------------------------------

impl Setup {
    /// Make the bwr and buffers of the next `wr` from `proc` read-only (as a
    /// read-only shared page is, even through /proc/<pid>/mem).
    fn readonly_next_call(&mut self, proc: ProcId) {
        let span = self.us.next_call_memory();
        self.us.procs.get_mut(&proc).unwrap().readonly = vec![span];
    }
}

/// A bwr that can be read but not written back fails the ioctl with
/// EFAULT, as binder_ioctl_write_read does, rather than succeeding with the
/// caller's consumed counters left stale.
#[test]
fn bwr_writeback_failure_is_efault() {
    for read_size in [0, 256] {
        let mut s = setup();
        s.readonly_next_call(s.client);
        let c = s.wr(
            s.client,
            1,
            &Bc::default().cmd(BC_ENTER_LOOPER).done(),
            read_size,
        );
        assert_eq!(s.us.outcome(c).errno, libc::EFAULT, "read_size {read_size}");
        assert_eq!(s.us.outcome(c).write_consumed, 0);
    }
}

/// A looper whose blocking read fails (its bwr can't be written back) never
/// waits: the next incoming transaction goes to a looper that is really
/// reading, not to the failed one, where a synchronous caller would hang.
#[test]
fn failed_block_leaves_no_waiting_looper() {
    let mut s = setup();
    s.write(s.server, 3, &Bc::default().cmd(BC_ENTER_LOOPER).done());
    s.readonly_next_call(s.server);
    let failed = s.wr(s.server, 2, &[], 256);
    assert_eq!(s.us.outcome(failed).errno, libc::EFAULT);
    let good = s.wr(s.server, 3, &[], 256);
    assert_eq!(s.b.blocked_threads(), vec![(s.server, 3)]);
    s.write(
        s.client,
        1,
        &Bc::default()
            .cmd(BC_TRANSACTION)
            .td(&BinderTransactionData::default())
            .done(),
    );
    assert!(find(&s.stream(good), BR_TRANSACTION).is_some());
}

/// A signal ends a blocked read's wait just as work arrives: the delivery
/// is rolled back, and the restarted ioctl reads the same transaction.
#[test]
fn interrupted_delivery_is_read_again() {
    let mut s = setup();
    let park = s.wr(s.server, 2, &[], 256);
    s.us.interrupt(park);
    s.write(
        s.client,
        1,
        &Bc::default()
            .cmd(BC_TRANSACTION)
            .td(&BinderTransactionData::default())
            .done(),
    );
    assert!(matches!(
        s.us.status(park),
        crate::fake::Status::Interrupted
    ));
    let again = s.us.restart(&mut s.b, park);
    assert!(find(&s.stream(again), BR_TRANSACTION).is_some());
    // It is on the server's stack once: the reply reaches the client.
    let rtd = BinderTransactionData::default();
    s.wr(
        s.server,
        2,
        &Bc::default().cmd(BC_REPLY).td(&rtd).done(),
        256,
    );
    assert!(find(&s.read(s.client, 1, 256), BR_REPLY).is_some());
}

/// NOTIF_SEND can succeed after the tracee's interruptible wait chose EINTR
/// but before the tracee reacquires the kernel's notification lock. libbinder
/// retries with the published read_consumed, even though delivery committed.
#[test]
fn published_read_survives_notification_restart() {
    let mut s = setup();
    let parked = s.wr(s.server, 2, &[], 256);
    s.write(
        s.client,
        1,
        &Bc::default()
            .cmd(BC_TRANSACTION)
            .td(&BinderTransactionData {
                flags: TF_ONE_WAY,
                ..Default::default()
            })
            .done(),
    );
    let delivered = s.stream(parked);
    assert!(find(&delivered, BR_TRANSACTION).is_some());
    // The model's answer succeeded, but the caller saw EINTR: same argument,
    // including the already published read counters, rather than a new read.
    let retry = s.ioctl(s.server, 2, BINDER_WRITE_READ, parked.arg);
    assert!(
        !s.us.is_pending(retry),
        "return published bytes before waiting for new work"
    );
    assert_eq!(s.stream(retry), delivered);
    let td = td_from(find(&delivered, BR_TRANSACTION).unwrap());
    s.write(
        s.server,
        2,
        &Bc::default().cmd(BC_FREE_BUFFER).u64(td.data_buffer).done(),
    );
    s.b.check_invariants();
}

/// Process work whose reader can't take it (its read buffer faults) goes
/// back to the process, for the next looper that reads.
#[test]
fn undeliverable_process_work_goes_back_to_the_process() {
    let mut s = setup();
    s.write(s.server, 3, &Bc::default().cmd(BC_ENTER_LOOPER).done());
    // Queued as process work: no server thread is waiting.
    s.write(
        s.client,
        1,
        &Bc::default()
            .cmd(BC_TRANSACTION)
            .td(&BinderTransactionData::default())
            .done(),
    );
    s.readonly_next_call(s.server);
    let failed = s.wr(s.server, 2, &[], 256);
    assert_eq!(s.us.outcome(failed).errno, libc::EFAULT);
    assert!(find(&s.read(s.server, 3, 256), BR_TRANSACTION).is_some());
}

/// `setup()` with a context manager whose node accepts fds.
fn setup_accepting_fds() -> Setup {
    let mut s = Setup::bare();
    let mut ext = FlatBinderObject::default();
    ext.flags = FLAT_BINDER_FLAG_ACCEPTS_FDS;
    s.us.poke(s.server, 0x8100_0000, pod_bytes(&ext));
    let c = s.ioctl(s.server, 2, BINDER_SET_CONTEXT_MGR_EXT, 0x8100_0000);
    assert_eq!(s.us.outcome(c).errno, 0);
    s.write(s.server, 2, &Bc::default().cmd(BC_ENTER_LOOPER).done());
    s
}

fn fd_object(fd: u32) -> BinderFdObject {
    let mut fdo = BinderFdObject::default();
    fdo.hdr_type = BINDER_TYPE_FD;
    fdo.fd_or_pad = fd as u64;
    fdo
}

/// Fail every operation of kind `f` from now on.
fn always_fail(s: &mut Setup, f: Fault) {
    s.us.fault = Some(Box::new(move |g| g == f));
}

/// Fail the next operation of kind `f`.
fn fail_once(s: &mut Setup, f: Fault) {
    let mut armed = true;
    s.us.fault = Some(Box::new(move |g| g == f && std::mem::take(&mut armed)));
}

/// The server asks for up to `n` looper threads.
fn set_max_threads(s: &mut Setup, n: u32) {
    let at = 0x8100_0000;
    s.us.poke(s.server, at, &n.to_le_bytes());
    let c = s.ioctl(s.server, 1, BINDER_SET_MAX_THREADS, at);
    assert_eq!(s.us.outcome(c).errno, 0);
}

/// What `call`'s read buffer holds (the driver must not write it once the
/// call is gone).
fn read_buffer(s: &Setup, proc: ProcId, call: Call, len: usize) -> Vec<u8> {
    let bwr: BinderWriteRead =
        read_pod(&s.us.peek(proc, call.arg, size_of::<BinderWriteRead>()), 0).unwrap();
    s.us.peek(proc, bwr.read_buffer, len)
}

/// A oneway dropped because its fd can't be installed frees its buffer like
/// BC_FREE_BUFFER does, so the next oneway to the node is delivered.
#[test]
fn dropped_oneway_unblocks_the_async_queue() {
    let mut s = setup_accepting_fds();
    let td = txn_with_object(&mut s, pod_bytes(&fd_object(7)), TF_ONE_WAY | TF_ACCEPT_FDS);
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );
    let mut td2 = BinderTransactionData::default();
    td2.flags = TF_ONE_WAY;
    td2.code = 2;
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td2).done(),
    );
    fail_once(&mut s, Fault::InstallFd);
    let c = s.wr(s.server, 2, &[], 256);
    assert!(
        !s.us.is_pending(c),
        "the second oneway is stuck in the async queue"
    );
    let td = td_from(find(&s.stream(c), BR_TRANSACTION).unwrap());
    assert_eq!(td.code, 2);
}

/// A reply whose fd can't be installed fails the caller with
/// BR_FAILED_REPLY, as binder_thread_read does, rather than leaving it
/// waiting.
#[test]
fn reply_whose_fd_cannot_be_installed_fails_the_caller() {
    let mut s = setup_accepting_fds();
    let mut td = BinderTransactionData::default();
    td.flags = TF_ACCEPT_FDS;
    s.write(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&td).done(),
    );
    let got = s.read(s.server, 2, 256);
    assert!(find(&got, BR_TRANSACTION).is_some());
    let waiting = s.wr(s.client, 1, &[], 256);
    assert!(s.us.is_pending(waiting));
    // The reply carries one fd.
    let (data, offsets) = (0x8001_0000u64, 0x8002_0000u64);
    s.us.poke(s.server, data, pod_bytes(&fd_object(5)));
    s.us.poke(s.server, offsets, &0u64.to_le_bytes());
    let mut rtd = BinderTransactionData::default();
    rtd.data_size = size_of::<BinderFdObject>() as u64;
    rtd.offsets_size = 8;
    rtd.data_buffer = data;
    rtd.data_offsets = offsets;
    always_fail(&mut s, Fault::InstallFd);
    s.wr(
        s.server,
        2,
        &Bc::default().cmd(BC_REPLY).td(&rtd).done(),
        256,
    );
    // The transaction-complete item comes first; the reply only fails as the
    // first item of a read.
    assert!(find(&s.stream(waiting), BR_TRANSACTION_COMPLETE).is_some());
    // The caller's next read also queues work behind the reply (a call to a
    // handle it doesn't have fails). The failed reply ends the read, as a
    // reply does.
    let mut bad = BinderTransactionData::default();
    bad.target = 99;
    let c = s.wr(
        s.client,
        1,
        &Bc::default().cmd(BC_TRANSACTION).td(&bad).done(),
        256,
    );
    assert!(
        !s.us.is_pending(c),
        "the caller waits for a reply that was dropped"
    );
    let got: Vec<u32> = s.stream(c).iter().map(|(cmd, _)| *cmd).collect();
    assert_eq!(got, [BR_NOOP, BR_FAILED_REPLY]);
    let got: Vec<u32> = s
        .read(s.client, 1, 256)
        .iter()
        .map(|(cmd, _)| *cmd)
        .collect();
    assert_eq!(got, [BR_NOOP, BR_FAILED_REPLY]);
    s.us.fault = None;
    assert_eq!(s.b.counts().0, 0, "transactions left");
}

/// The longest-waiting looper's wait was ended by a signal that stopped it
/// (SIGSTOP, ptrace) without the driver knowing. Process work goes to the
/// other looper, and nothing is written into the stopped one's memory.
#[test]
fn process_work_skips_a_stopped_looper() {
    let mut s = setup();
    s.write(s.server, 3, &Bc::default().cmd(BC_ENTER_LOOPER).done());
    let stopped = s.wr(s.server, 2, &[], 256);
    let live = s.wr(s.server, 3, &[], 256);
    s.us.interrupt(stopped);
    s.write(
        s.client,
        1,
        &Bc::default()
            .cmd(BC_TRANSACTION)
            .td(&BinderTransactionData::default())
            .done(),
    );
    assert!(
        !s.us.is_pending(live),
        "the transaction waits for the stopped thread"
    );
    assert!(find(&s.stream(live), BR_TRANSACTION).is_some());
    assert_eq!(read_buffer(&s, s.server, stopped, 8), vec![0; 8]);
    // Restarted, the stopped thread just waits again.
    let again = s.us.restart(&mut s.b, stopped);
    assert!(s.us.is_pending(again));
}

/// A BR_SPAWN_LOOPER whose read is rolled back is asked for again by the
/// restarted read, and only one is outstanding once a read delivers it.
#[test]
fn spawn_request_survives_rollback_once() {
    let mut s = setup();
    set_max_threads(&mut s, 1);
    s.write(
        s.client,
        1,
        &Bc::default()
            .cmd(BC_TRANSACTION)
            .td(&BinderTransactionData::default())
            .done(),
    );
    let lost = {
        s.readonly_next_call(s.server);
        s.wr(s.server, 2, &[], 256)
    };
    assert_eq!(s.us.outcome(lost).errno, libc::EFAULT);
    s.us.procs.get_mut(&s.server).unwrap().readonly.clear();
    let got = s.read(s.server, 2, 256);
    assert_eq!(got[0].0, BR_SPAWN_LOOPER);
    assert!(find(&got, BR_TRANSACTION).is_some());
    // Reply; the next read finds the request outstanding and asks no more.
    let rtd = BinderTransactionData::default();
    let c = s.wr(
        s.server,
        2,
        &Bc::default().cmd(BC_REPLY).td(&rtd).done(),
        256,
    );
    assert_eq!(s.stream(c)[0].0, BR_NOOP);
}

/// A poll setup (a write-only BC_ENTER_LOOPER) whose call a signal ends
/// before the fd is made pollable is redone by the restarted ioctl.
#[test]
fn poll_setup_is_retried_by_the_restart() {
    let mut s = Setup::bare();
    let c = s.ioctl(s.server, 1, BINDER_SET_CONTEXT_MGR, 0);
    assert_eq!(s.us.outcome(c).errno, 0);
    fail_once(&mut s, Fault::Signal);
    let enter = Bc::default().cmd(BC_ENTER_LOOPER).done();
    let c = s.wr(s.server, 3, &enter, 0);
    assert!(matches!(s.us.status(c), crate::fake::Status::Interrupted));
    assert_eq!(s.us.procs[&s.server].poll_swaps, 0);
    let c = s.us.restart(&mut s.b, c);
    assert_eq!(s.us.outcome(c).errno, 0);
    assert_eq!(s.us.procs[&s.server].poll_swaps, 1);
    // The thread polls: work for the process makes its fd readable.
    s.write(
        s.client,
        1,
        &Bc::default()
            .cmd(BC_TRANSACTION)
            .td(&BinderTransactionData::default())
            .done(),
    );
    assert!(s.us.procs[&s.server].poll_ready);
}

// ---- the freezer --------------------------------------------------------

/// `BINDER_FREEZE` of `pid` from the client's thread 3; returns the call.
fn freeze(s: &mut Setup, pid: u32, enable: bool, timeout_ms: u32) -> Call {
    let at = 0x9100_0000;
    let info = BinderFreezeInfo {
        pid,
        enable: enable as u32,
        timeout_ms,
    };
    s.us.poke(s.client, at, pod_bytes(&info));
    let c = s.ioctl(s.client, 3, BINDER_FREEZE, at);
    s.b.check_invariants();
    c
}

fn freeze_errno(s: &mut Setup, pid: u32, enable: bool, timeout_ms: u32) -> i32 {
    let c = freeze(s, pid, enable, timeout_ms);
    s.us.outcome(c).errno
}

fn frozen_info(s: &mut Setup, pid: u32) -> Result<(u32, u32), i32> {
    let at = 0x9100_1000;
    let info = BinderFrozenStatusInfo {
        pid,
        ..Default::default()
    };
    s.us.poke(s.client, at, pod_bytes(&info));
    let c = s.ioctl(s.client, 3, BINDER_GET_FROZEN_INFO, at);
    match s.us.outcome(c).errno {
        0 => {
            let out: BinderFrozenStatusInfo = read_pod(
                &s.us.peek(s.client, at, size_of::<BinderFrozenStatusInfo>()),
                0,
            )
            .unwrap();
            Ok((out.sync_recv, out.async_recv))
        }
        e => Err(e),
    }
}

fn call_server(s: &mut Setup, flags: u32) -> Vec<(u32, Vec<u8>)> {
    let mut td = BinderTransactionData::default();
    td.flags = flags;
    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    let c = s.wr(s.client, 1, &w, 256);
    s.stream(c)
}

/// The kernel's BinderLibTest.Freeze and FreezeTxn: a frozen process fails
/// synchronous calls with BR_FROZEN_REPLY, takes oneways with
/// BR_TRANSACTION_PENDING_FROZEN, and reports both; thawed, it works again.
#[test]
fn frozen_process_fails_calls_and_queues_oneways() {
    let mut s = setup();
    assert_eq!(frozen_info(&mut s, 4242), Err(libc::EINVAL));
    assert_eq!(freeze_errno(&mut s, 4242, true, 0), libc::EINVAL);
    assert_eq!(freeze_errno(&mut s, 1000, true, 0), 0);
    assert_eq!(frozen_info(&mut s, 1000), Ok((0, 0)));

    let r = call_server(&mut s, 0);
    assert!(find(&r, BR_FROZEN_REPLY).is_some());
    assert!(find(&r, BR_TRANSACTION_COMPLETE).is_none());
    let r = call_server(&mut s, TF_ONE_WAY);
    assert!(find(&r, BR_TRANSACTION_PENDING_FROZEN).is_some());
    // (Bit 1: the oneway is still outstanding.)
    assert_eq!(frozen_info(&mut s, 1000), Ok((3, 1)));
    // The oneway is queued for the server, which reads it once it runs.
    assert!(find(&s.read(s.server, 2, 256), BR_TRANSACTION).is_some());

    assert_eq!(freeze_errno(&mut s, 1000, false, 0), 0);
    assert_eq!(frozen_info(&mut s, 1000), Ok((0, 0)));
    let w = Bc::default()
        .cmd(BC_TRANSACTION)
        .td(&BinderTransactionData::default())
        .done();
    s.write(s.client, 1, &w);
    assert!(find(&s.read(s.server, 2, 256), BR_TRANSACTION).is_some());
}

/// A freeze with a timeout waits for the target's queued transactions to be
/// read, and fails with EAGAIN (thawing it) if they aren't in time. One
/// without a timeout fails at once; so does one while the target serves a
/// call, which GET_FROZEN_INFO reports as bit 1 of sync_recv.
#[test]
fn freeze_waits_for_outstanding_transactions() {
    let mut s = setup();
    // A queued oneway.
    call_server(&mut s, TF_ONE_WAY);
    assert_eq!(freeze_errno(&mut s, 1000, true, 0), libc::EAGAIN);
    let f = freeze(&mut s, 1000, true, 1000);
    assert!(s.us.is_pending(f));
    // Reading it drains the server: the freeze succeeds.
    let r = s.read(s.server, 2, 256);
    let td = td_from(find(&r, BR_TRANSACTION).unwrap());
    assert_eq!(s.us.outcome(f).errno, 0);
    assert_eq!(s.us.timer, Some(1000));
    s.write(
        s.server,
        2,
        &Bc::default().cmd(BC_FREE_BUFFER).u64(td.data_buffer).done(),
    );
    assert_eq!(freeze_errno(&mut s, 1000, false, 0), 0);

    // One that doesn't drain in time.
    call_server(&mut s, TF_ONE_WAY);
    let f = freeze(&mut s, 1000, true, 1000);
    s.us.advance(&mut s.b, 999);
    assert!(s.us.is_pending(f));
    s.us.advance(&mut s.b, 1);
    s.b.check_invariants();
    assert_eq!(s.us.outcome(f).errno, libc::EAGAIN);
    // A failed freeze thaws: a call goes through (the client waits for its
    // reply).
    let w = Bc::default()
        .cmd(BC_TRANSACTION)
        .td(&BinderTransactionData::default())
        .done();
    let c = s.wr(s.client, 1, &w, 256);
    assert!(s.us.is_pending(c));

    // The server reads the oneway, then the call. Serving the call, it
    // can't be frozen.
    let r = s.read(s.server, 2, 256);
    let td = td_from(find(&r, BR_TRANSACTION).unwrap());
    assert_ne!(td.flags & TF_ONE_WAY, 0);
    let free = Bc::default().cmd(BC_FREE_BUFFER).u64(td.data_buffer).done();
    let c = s.wr(s.server, 2, &free, 256);
    let td = td_from(find(&s.stream(c), BR_TRANSACTION).unwrap());
    assert_eq!(td.flags & TF_ONE_WAY, 0);
    assert_eq!(freeze_errno(&mut s, 1000, true, 0), libc::EAGAIN);
    assert_eq!(frozen_info(&mut s, 1000), Ok((2, 0)));
}

/// A signal that ends a freeze's wait thaws its target; the restarted
/// ioctl starts over.
#[test]
fn interrupted_freeze_thaws_its_target() {
    let mut s = setup();
    call_server(&mut s, TF_ONE_WAY);
    let f = freeze(&mut s, 1000, true, 1000);
    s.us.interrupt(f);
    // Still marked frozen until the driver hears of it.
    let f = s.us.restart(&mut s.b, f);
    s.b.check_invariants();
    assert!(s.us.is_pending(f));
    s.us.interrupt(f);
    // The driver finds the call gone at its deadline.
    s.us.advance(&mut s.b, 1000);
    s.b.check_invariants();
    assert_eq!(s.b.freezer_state(), (0, false));
}

/// Two binder instances of one pid (/dev/binder and /dev/hwbinder) freeze
/// together, the second once the first has drained.
#[test]
fn freeze_covers_every_instance_of_the_pid() {
    let mut s = setup();
    let hw = s.b.open(1000, 1000, 1, false);
    s.us.add_proc(hw, 0xa000_0000);
    call_server(&mut s, TF_ONE_WAY);
    let f = freeze(&mut s, 1000, true, 100);
    assert!(s.us.is_pending(f));
    s.read(s.server, 2, 256);
    assert_eq!(s.us.outcome(f).errno, 0);
    assert_eq!(frozen_info(&mut s, 1000), Ok((0, 0)));
    let r = call_server(&mut s, 0);
    assert!(find(&r, BR_FROZEN_REPLY).is_some());
    s.b.release_proc(hw, &mut s.us);
    s.b.check_invariants();
}

fn frozen_state(r: &[(u32, Vec<u8>)]) -> Option<(u64, u32)> {
    find(r, BR_FROZEN_BINDER).map(|p| {
        let i: BinderFrozenStateInfo = read_pod(p, 0).unwrap();
        (i.cookie, i.is_frozen)
    })
}

fn handle_cookie(cmd: u32, handle: u32, cookie: u64) -> Vec<u8> {
    let hc = BinderHandleCookie { handle, cookie };
    let mut v = cmd.to_le_bytes().to_vec();
    v.extend_from_slice(pod_bytes(&hc));
    v
}

/// The kernel's freeze notifications (binderDriverInterfaceTest's
/// RequestFrozenNotification, OverwritePendingFrozenNotification and
/// ResendFrozenNotification): the state at once, then every change, with a
/// change while one is unacknowledged sent again after the ack, and changes
/// before a delivery coalesced.
#[test]
fn freeze_notifications() {
    let mut s = setup();
    // The client holds the context manager (handle 0) and watches it from
    // a looper thread, which takes the notifications.
    s.write(s.client, 2, &Bc::default().cmd(BC_ENTER_LOOPER).done());
    s.write(s.client, 2, &Bc::default().cmd(BC_INCREFS).u32(0).done());
    s.write(
        s.client,
        2,
        &handle_cookie(BC_REQUEST_FREEZE_NOTIFICATION, 0, 77),
    );
    let w = handle_cookie(BC_REQUEST_FREEZE_NOTIFICATION, 0, 78);
    let c = s.wr(s.client, 2, &w, 0);
    assert_eq!(s.us.outcome(c).errno, libc::EINVAL, "one per ref");
    assert_eq!(frozen_state(&s.read(s.client, 2, 256)), Some((77, 0)));
    let ack = Bc::default()
        .cmd(BC_FREEZE_NOTIFICATION_DONE)
        .u64(77)
        .done();
    s.write(s.client, 2, &ack);
    let c = s.wr(s.client, 2, &ack, 0);
    assert_eq!(s.us.outcome(c).errno, libc::EINVAL, "nothing to ack");

    freeze(&mut s, 1000, true, 0);
    assert_eq!(frozen_state(&s.read(s.client, 2, 256)), Some((77, 1)));
    // Changes while that is unacknowledged: sent again after the ack.
    freeze(&mut s, 1000, false, 0);
    assert!({
        let c = s.client;
        nothing_to_read(&mut s, c, 2)
    });
    s.write(s.client, 2, &ack);
    assert_eq!(frozen_state(&s.read(s.client, 2, 256)), Some((77, 0)));
    s.write(s.client, 2, &ack);
    // Changes before a delivery coalesce into the latest state.
    freeze(&mut s, 1000, true, 0);
    freeze(&mut s, 1000, false, 0);
    freeze(&mut s, 1000, true, 0);
    let r = s.read(s.client, 2, 256);
    assert_eq!(frozen_state(&r), Some((77, 1)));
    assert_eq!(r.iter().filter(|(c, _)| *c == BR_FROZEN_BINDER).count(), 1);
    // Cleared while unacknowledged: CLEAR_DONE after the ack.
    s.write(
        s.client,
        2,
        &handle_cookie(BC_CLEAR_FREEZE_NOTIFICATION, 0, 77),
    );
    assert!({
        let c = s.client;
        nothing_to_read(&mut s, c, 2)
    });
    s.write(s.client, 2, &ack);
    let r = s.read(s.client, 2, 256);
    assert_eq!(
        find(&r, BR_CLEAR_FREEZE_NOTIFICATION_DONE),
        Some(&77u64.to_le_bytes().to_vec())
    );
    assert_eq!(s.b.freezer_state().0, 0);
    let w = handle_cookie(BC_CLEAR_FREEZE_NOTIFICATION, 0, 77);
    let c = s.wr(s.client, 2, &w, 0);
    assert_eq!(s.us.outcome(c).errno, libc::EINVAL, "not watched");
    freeze(&mut s, 1000, false, 0);
    // A request whose ref goes away goes with it.
    s.write(
        s.client,
        2,
        &handle_cookie(BC_REQUEST_FREEZE_NOTIFICATION, 0, 79),
    );
    s.write(s.client, 2, &Bc::default().cmd(BC_DECREFS).u32(0).done());
    assert_eq!(s.b.freezer_state().0, 0);
    assert!({
        let c = s.client;
        nothing_to_read(&mut s, c, 2)
    });
}

/// A read from the thread blocks. (Its next ioctl abandons it.)
fn nothing_to_read(s: &mut Setup, proc: ProcId, tid: u32) -> bool {
    let c = s.wr(proc, tid, &[], 256);
    s.us.is_pending(c)
}
