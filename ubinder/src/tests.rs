//! Unit tests for the broker state machine. They drive the broker exactly as
//! libbinder would: build a BC_* command stream, call `write_read`, and
//! decode the BR_* stream, asserting the kernel's observable behaviour.
#![allow(clippy::field_reassign_with_default)] // readable struct-by-field setup

use crate::broker::*;
use crate::defs::*;
use crate::testmem::FakeMem;

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
        self.0.extend_from_slice(pod_bytes_pub(td));
        self
    }
    fn done(self) -> Vec<u8> {
        self.0
    }
}

fn pod_bytes_pub<T: Copy>(v: &T) -> &[u8] {
    let p = v as *const T as *const u8;
    unsafe { std::slice::from_raw_parts(p, std::mem::size_of::<T>()) }
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
        BR_DEAD_BINDER | BR_CLEAR_DEATH_NOTIFICATION_DONE => 8,
        BR_ERROR => 4,
        _ => 0,
    }
}

fn find(stream: &[(u32, Vec<u8>)], cmd: u32) -> Option<&Vec<u8>> {
    stream.iter().find(|(c, _)| *c == cmd).map(|(_, p)| p)
}

fn td_from(p: &[u8]) -> BinderTransactionData {
    read_pod_pub(p, 0)
}

fn read_pod_pub<T: Copy>(buf: &[u8], off: usize) -> T {
    let mut v = std::mem::MaybeUninit::<T>::uninit();
    unsafe {
        std::ptr::copy_nonoverlapping(
            buf.as_ptr().add(off),
            v.as_mut_ptr() as *mut u8,
            std::mem::size_of::<T>(),
        );
        v.assume_init()
    }
}

// A standard two-process setup: a server that is the context manager, and a
// client. recv regions far apart so target addresses are unambiguous.
struct Setup {
    b: Broker,
    m: FakeMem,
    server: ProcId,
    client: ProcId,
}

fn setup() -> Setup {
    let mut b = Broker::new();
    let mut m = FakeMem::default();
    let server = b.open(1000, 1000, 0);
    let client = b.open(1001, 10001, 0);
    m.add_proc(server, 0x8000_0000);
    m.add_proc(client, 0x9000_0000);
    // server becomes context manager
    b.set_context_mgr(server, None).unwrap();
    // both enter the looper
    // The server's thread 2 is a looper (joinThreadPool); the client's
    // thread 1 is a plain caller.
    let w = Bc::default().cmd(BC_ENTER_LOOPER).done();
    b.write_read(server, 2, &w, 0, false, &mut m);
    Setup {
        b,
        m,
        server,
        client,
    }
}

// ---- tests ------------------------------------------------------------

#[test]
fn version_and_ctx_mgr() {
    let mut b = Broker::new();
    assert_eq!(b.protocol_version(), 8);
    let p = b.open(1000, 1000, 0);
    assert!(b.set_context_mgr(p, None).is_ok());
    // second attempt is EBUSY
    let p2 = b.open(1000, 1000, 0);
    assert_eq!(b.set_context_mgr(p2, None), Err(-libc::EBUSY));
}

#[test]
fn register_service_and_transact() {
    let mut s = setup();
    // Client sends a transaction to the context manager (handle 0) that
    // contains one BINDER_TYPE_BINDER (registering a service object).
    let data_addr = 0x9001_0000u64;
    let off_addr = 0x9002_0000u64;
    let mut fbo = FlatBinderObject::default();
    fbo.hdr_type = BINDER_TYPE_BINDER;
    fbo.flags = FLAT_BINDER_FLAG_ACCEPTS_FDS;
    fbo.binder_or_handle = 0xBEEF_0000; // client's local BBinder ptr
    fbo.cookie = 0xCAFE;
    s.m.poke(s.client, data_addr, pod_bytes_pub(&fbo));
    s.m.poke(s.client, off_addr, &0u64.to_le_bytes()); // object at offset 0

    let mut td = BinderTransactionData::default();
    td.target = 0; // context manager
    td.code = 1;
    td.flags = 0;
    td.data_size = std::mem::size_of::<FlatBinderObject>() as u64;
    td.offsets_size = 8;
    td.data_buffer = data_addr;
    td.data_offsets = off_addr;

    // The server has a thread parked in a read, as servicemanager does.
    let park = s.b.write_read(s.server, 2, &[], 256, false, &mut s.m);
    assert!(park.blocked);

    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    let r = s.b.write_read(s.client, 1, &w, 0, false, &mut s.m);
    // The server should have become runnable.
    assert!(r.wakeups.iter().any(|w| w.proc == s.server));

    // Server reads: expect BR_TRANSACTION carrying a translated HANDLE.
    let rd = s.b.write_read(s.server, 2, &[], 256, false, &mut s.m);
    let stream = decode(&rd.read);
    let tp = find(&stream, BR_TRANSACTION).expect("server gets BR_TRANSACTION");
    let td2 = td_from(tp);
    assert_eq!(td2.code, 1);
    assert_eq!(td2.sender_pid, 1001);
    assert_eq!(td2.sender_euid, 10001);
    // The server's copy of the object is a HANDLE now.
    let obj = s.m.peek(
        s.server,
        td2.data_buffer,
        std::mem::size_of::<FlatBinderObject>(),
    );
    let fbo2: FlatBinderObject = read_pod_pub(&obj, 0);
    assert_eq!(fbo2.hdr_type, BINDER_TYPE_HANDLE);
    let handle = fbo2.binder_or_handle as u32;
    assert!(handle >= 1);
    // The server now holds a strong ref to the client's node.
    let (strong, _weak) = s.b.ref_counts(s.server, handle).unwrap();
    assert_eq!(strong, 1);

    // Client is now blocked awaiting the reply (sync call).
    let cblock = s.b.write_read(s.client, 1, &[], 256, false, &mut s.m);
    assert!(cblock.blocked);

    // Server replies; client wakes and, on the same read, receives its
    // deferred node handshake (INCREFS/ACQUIRE), TRANSACTION_COMPLETE and REPLY.
    let mut rtd = BinderTransactionData::default();
    rtd.code = 0;
    let wr = Bc::default().cmd(BC_REPLY).td(&rtd).done();
    s.b.write_read(s.server, 2, &wr, 256, false, &mut s.m);
    let rc = s.b.write_read(s.client, 1, &[], 512, false, &mut s.m);
    let cs = decode(&rc.read);
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
    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    s.b.write_read(s.client, 1, &w, 0, false, &mut s.m);
    let r = s.b.write_read(s.client, 1, &[], 256, false, &mut s.m);
    assert!(find(&decode(&r.read), BR_FAILED_REPLY).is_some());
}

#[test]
fn sync_call_and_reply_routing() {
    let mut s = setup();
    // Client calls the context-manager node directly (handle 0), server replies.
    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.code = 42;
    td.data_size = 0;
    td.offsets_size = 0;
    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    s.b.write_read(s.client, 1, &w, 0, false, &mut s.m);

    // Server receives the transaction.
    let rd = s.b.write_read(s.server, 2, &[], 256, false, &mut s.m);
    let stream = decode(&rd.read);
    assert!(find(&stream, BR_TRANSACTION).is_some());

    // Client, meanwhile, blocks waiting for the reply.
    let rc = s.b.write_read(s.client, 1, &[], 256, false, &mut s.m);
    assert!(rc.blocked, "client with no reply yet must block");

    // Server replies.
    let mut rtd = BinderTransactionData::default();
    rtd.code = 0;
    let wr = Bc::default().cmd(BC_REPLY).td(&rtd).done();
    let rs = s.b.write_read(s.server, 2, &wr, 256, false, &mut s.m);
    // Replying wakes the client.
    assert!(rs.wakeups.iter().any(|w| w.proc == s.client && w.tid == 1));

    // Client reads the reply.
    let rc2 = s.b.write_read(s.client, 1, &[], 256, false, &mut s.m);
    let cs = decode(&rc2.read);
    assert!(find(&cs, BR_REPLY).is_some(), "client gets BR_REPLY");
}

#[test]
fn fd_passing() {
    let _s = setup();
    // The server's receive node must accept fds. Register a service first so
    // the client holds a handle to a server node that accepts fds.
    // Simplify: client transacts to ctx-mgr node (which accepts fds? ctx mgr
    // node flags=0). Give the client a direct node on the server instead by
    // having the server register a node via reply. To keep it focused, we test
    // fd translation through a transaction to a node created with ACCEPTS_FDS.

    // Server registers a node by sending it to the client in a reply isn't
    // set up here; instead create one: client -> ctxmgr won't accept fds.
    // So: make a server-owned node with ACCEPTS_FDS and give client a handle.
    // We do that by having the server send a transaction to the client first.
    //
    // Easiest: reuse register_service_and_transact's mechanism in reverse —
    // the context-manager node is fine for routing; patch its accept_fds by
    // registering a fresh ctx with ACCEPTS_FDS via SET_CONTEXT_MGR_EXT in a
    // separate broker.
    let mut b = Broker::new();
    let mut m = FakeMem::default();
    let server = b.open(1000, 1000, 0);
    let client = b.open(1001, 10001, 0);
    m.add_proc(server, 0x8000_0000);
    m.add_proc(client, 0x9000_0000);
    let mut ext = FlatBinderObject::default();
    ext.flags = FLAT_BINDER_FLAG_ACCEPTS_FDS;
    b.set_context_mgr(server, Some(ext)).unwrap();
    let el = Bc::default().cmd(BC_ENTER_LOOPER).done();
    b.write_read(server, 2, &el, 0, false, &mut m);

    // Client sends a transaction with a BINDER_TYPE_FD object.
    let data_addr = 0x9001_0000u64;
    let off_addr = 0x9002_0000u64;
    let mut fdo = BinderFdObject::default();
    fdo.hdr_type = BINDER_TYPE_FD;
    fdo.fd_or_pad = 7; // client's fd 7
    m.poke(client, data_addr, pod_bytes_pub(&fdo));
    m.poke(client, off_addr, &0u64.to_le_bytes());

    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.flags = TF_ACCEPT_FDS;
    td.data_size = std::mem::size_of::<BinderFdObject>() as u64;
    td.offsets_size = 8;
    td.data_buffer = data_addr;
    td.data_offsets = off_addr;
    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    b.write_read(client, 1, &w, 0, false, &mut m);

    let rd = b.write_read(server, 2, &[], 256, false, &mut m);
    let stream = decode(&rd.read);
    let tp = find(&stream, BR_TRANSACTION).expect("server gets BR_TRANSACTION with fd");
    let td2 = td_from(tp);
    let obj = m.peek(
        server,
        td2.data_buffer,
        std::mem::size_of::<BinderFdObject>(),
    );
    let fdo2: BinderFdObject = read_pod_pub(&obj, 0);
    let new_fd = fdo2.fd_or_pad as u32;
    assert_ne!(new_fd, 7);
    assert!(m.procs[&server].installed_fds.contains(&new_fd));
}

#[test]
fn oneway_does_not_block_and_completes() {
    let mut s = setup();
    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.flags = TF_ONE_WAY;
    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    let r = s.b.write_read(s.client, 1, &w, 256, false, &mut s.m);
    // Oneway returns TRANSACTION_COMPLETE immediately to the caller.
    let cs = decode(&r.read);
    assert!(find(&cs, BR_TRANSACTION_COMPLETE).is_some());
    // Server gets the transaction (as proc work).
    let rd = s.b.write_read(s.server, 2, &[], 256, false, &mut s.m);
    assert!(find(&decode(&rd.read), BR_TRANSACTION).is_some());
}

#[test]
fn death_notification_fires_on_release() {
    let mut s = setup();
    // Client registers a service object, so the server gets a handle to it.
    let data_addr = 0x9001_0000u64;
    let off_addr = 0x9002_0000u64;
    let mut fbo = FlatBinderObject::default();
    fbo.hdr_type = BINDER_TYPE_BINDER;
    fbo.binder_or_handle = 0xBEEF;
    s.m.poke(s.client, data_addr, pod_bytes_pub(&fbo));
    s.m.poke(s.client, off_addr, &0u64.to_le_bytes());
    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.data_size = std::mem::size_of::<FlatBinderObject>() as u64;
    td.offsets_size = 8;
    td.data_buffer = data_addr;
    td.data_offsets = off_addr;
    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    s.b.write_read(s.client, 1, &w, 0, false, &mut s.m);
    let rd = s.b.write_read(s.server, 2, &[], 256, false, &mut s.m);
    let td2 = td_from(find(&decode(&rd.read), BR_TRANSACTION).unwrap());
    let obj = s.m.peek(
        s.server,
        td2.data_buffer,
        std::mem::size_of::<FlatBinderObject>(),
    );
    let handle = (read_pod_pub::<FlatBinderObject>(&obj, 0)).binder_or_handle as u32;
    // Server replies to the registration so its thread returns to proc work.
    let mut rtd = BinderTransactionData::default();
    rtd.code = 0;
    let wr = Bc::default().cmd(BC_REPLY).td(&rtd).done();
    // read_size>0 so the server consumes its own TRANSACTION_COMPLETE here.
    s.b.write_read(s.server, 2, &wr, 256, false, &mut s.m);
    // Drain the client so the outstanding call completes.
    s.b.write_read(s.client, 1, &[], 512, false, &mut s.m);

    // Server requests a death notification on that handle, then parks.
    let cookie = 0x1234_5678u64;
    let w = Bc::default()
        .cmd(BC_REQUEST_DEATH_NOTIFICATION)
        .u32(handle)
        .u64(cookie)
        .done();
    s.b.write_read(s.server, 2, &w, 0, false, &mut s.m);
    let park = s.b.write_read(s.server, 2, &[], 256, false, &mut s.m);
    assert!(park.blocked);

    // Client dies.
    let wk = s.b.release_proc(s.client, &mut s.m);
    assert!(wk.iter().any(|w| w.proc == s.server));

    // Server reads: expect BR_DEAD_BINDER with the cookie.
    let rd = s.b.write_read(s.server, 2, &[], 256, false, &mut s.m);
    let stream = decode(&rd.read);
    let p = find(&stream, BR_DEAD_BINDER).expect("server gets BR_DEAD_BINDER");
    assert_eq!(u64::from_le_bytes(p[..8].try_into().unwrap()), cookie);
}

#[test]
fn node_ref_handshake() {
    // When a node first gets an incoming ref, its owner must receive
    // BR_INCREFS then BR_ACQUIRE; when the last ref drops, BR_RELEASE/DECREFS.
    let mut s = setup();
    let data_addr = 0x9001_0000u64;
    let off_addr = 0x9002_0000u64;
    let mut fbo = FlatBinderObject::default();
    fbo.hdr_type = BINDER_TYPE_BINDER;
    fbo.binder_or_handle = 0xABCD;
    s.m.poke(s.client, data_addr, pod_bytes_pub(&fbo));
    s.m.poke(s.client, off_addr, &0u64.to_le_bytes());
    let mut td = BinderTransactionData::default();
    td.target = 0;
    td.data_size = std::mem::size_of::<FlatBinderObject>() as u64;
    td.offsets_size = 8;
    td.data_buffer = data_addr;
    td.data_offsets = off_addr;
    let w = Bc::default().cmd(BC_TRANSACTION).td(&td).done();
    s.b.write_read(s.client, 1, &w, 0, false, &mut s.m);
    // drive the server to translate (creates the incoming ref), then reply
    s.b.write_read(s.server, 2, &[], 256, false, &mut s.m);
    let rtd = BinderTransactionData::default();
    let wr = Bc::default().cmd(BC_REPLY).td(&rtd).done();
    s.b.write_read(s.server, 2, &wr, 256, false, &mut s.m);

    // The client (owner) receives the deferred node handshake with the reply.
    let rc = s.b.write_read(s.client, 1, &[], 512, false, &mut s.m);
    let cs = decode(&rc.read);
    assert!(find(&cs, BR_INCREFS).is_some(), "owner gets BR_INCREFS");
    assert!(find(&cs, BR_ACQUIRE).is_some(), "owner gets BR_ACQUIRE");
}

#[test]
fn bad_command_is_einval() {
    let mut b = Broker::new();
    let mut m = FakeMem::default();
    let p = b.open(1, 1, 0);
    m.add_proc(p, 0x1000);
    let w = Bc::default().cmd(0xdead_beef).done();
    let r = b.write_read(p, 1, &w, 0, false, &mut m);
    assert_eq!(r.write_err, libc::EINVAL);
}
