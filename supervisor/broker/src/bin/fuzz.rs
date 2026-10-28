//! A standalone, dependency-free fuzzer for the broker. Each iteration runs a
//! random sequence of operations over several processes and threads:
//! `BINDER_WRITE_READ` with command streams that are either raw random bytes
//! or built from the BC_* grammar with random arguments (so transactions with
//! object arrays, refcount commands, death notifications and replies get deep
//! into the state machine), plus reads, thread exits, context-manager setup,
//! process death, signals that interrupt blocked calls (which then restart),
//! freezing and thawing processes (`BINDER_FREEZE` with timeouts, as time
//! passes) and freeze notifications. In most iterations userspace operations also fail at random:
//! copies (`EFAULT`), allocations, fd captures and installs, and calls that
//! a signal ends just before they are answered. After every step the
//! broker's internal invariants are checked; any panic is a bug.
//!
//! Run a bounded campaign:  `linuxapk-broker-fuzz [iterations] [seed]`
//! It is also wired into `cargo test` via `fuzz::smoke` below.

use linuxapk_broker::binder::driver::{BinderDriver, ProcId};
use linuxapk_broker::binder::uapi::*;
use linuxapk_broker::fake::{FakeUserspace, Rng};

const NPROC: usize = 3;
const DATA: u64 = 0x10_0000;
const OFFS: u64 = 0x20_0000;
const SG: u64 = 0x30_0000;

fn put<T: Copy>(v: &mut Vec<u8>, x: &T) {
    let p = x as *const T as *const u8;
    v.extend_from_slice(unsafe { std::slice::from_raw_parts(p, size_of::<T>()) });
}

/// A small handle or descriptor, biased towards values that exist.
fn small(r: &mut Rng) -> u64 {
    if r.chance(90) {
        r.below(6)
    } else {
        r.next_u64()
    }
}

/// Write a random object array into `proc`'s memory and return
/// (data_size, offsets_size, extra_buffers_size).
fn objects(r: &mut Rng, m: &mut FakeUserspace, proc: ProcId, base: u64) -> (u64, u64, u64) {
    let n = if r.chance(40) { 0 } else { r.below(5) as usize };
    let mut data = Vec::new();
    let mut offs = Vec::new();
    let mut extra = 0u64;
    for i in 0..n {
        if r.chance(5) {
            data.extend((0..r.below(9)).map(|_| r.byte()));
        }
        while data.len() % 4 != 0 {
            data.push(0);
        }
        let off = if r.chance(3) {
            r.below(64)
        } else {
            data.len() as u64
        };
        offs.push(off);
        match r.below(10) {
            0..=4 => {
                let fbo = FlatBinderObject {
                    hdr_type: if r.chance(80) {
                        BINDER_TYPE_BINDER
                    } else {
                        BINDER_TYPE_WEAK_BINDER
                    },
                    flags: (r.next_u64() as u32) & 0x11ff,
                    binder_or_handle: 0x1000 + r.below(4) * 0x10,
                    cookie: r.below(3),
                };
                put(&mut data, &fbo);
            }
            5 | 6 => {
                let fbo = FlatBinderObject {
                    hdr_type: if r.chance(80) {
                        BINDER_TYPE_HANDLE
                    } else {
                        BINDER_TYPE_WEAK_HANDLE
                    },
                    flags: 0,
                    binder_or_handle: small(r),
                    cookie: 0,
                };
                put(&mut data, &fbo);
            }
            7 => {
                let fdo = BinderFdObject {
                    hdr_type: BINDER_TYPE_FD,
                    pad_flags: 0,
                    fd_or_pad: r.below(8),
                    cookie: 0,
                };
                put(&mut data, &fdo);
            }
            8 => {
                let len = if r.chance(90) {
                    r.below(64)
                } else {
                    r.next_u64()
                };
                let bbo = BinderBufferObject {
                    hdr_type: BINDER_TYPE_PTR,
                    flags: if i > 0 && r.chance(50) {
                        BINDER_BUFFER_FLAG_HAS_PARENT
                    } else {
                        0
                    },
                    buffer: SG + r.below(256),
                    length: len,
                    parent: r.below(i as u64 + 1),
                    parent_offset: r.below(64),
                };
                extra += (len.min(1 << 16) + 7) & !7;
                put(&mut data, &bbo);
            }
            _ => {
                let fda = BinderFdArrayObject {
                    hdr_type: BINDER_TYPE_FDA,
                    pad: 0,
                    num_fds: r.below(4),
                    parent: r.below(i as u64 + 1),
                    parent_offset: r.below(32),
                };
                put(&mut data, &fda);
            }
        }
    }
    if r.chance(10) && !data.is_empty() {
        let i = r.below(data.len() as u64) as usize;
        data[i] = r.byte();
    }
    m.poke(proc, base, &data);
    let ob: Vec<u8> = offs.iter().flat_map(|o| o.to_le_bytes()).collect();
    m.poke(proc, base + (OFFS - DATA), &ob);
    if r.chance(10) {
        extra = r.next_u64() & 0xffff_fff8;
    }
    (data.len() as u64, ob.len() as u64, extra)
}

/// A random but well-formed BC_* stream.
fn commands(
    r: &mut Rng,
    m: &mut FakeUserspace,
    proc: ProcId,
    bufs: &[u64],
    must_reply: bool,
) -> Vec<u8> {
    let mut w = Vec::new();
    for i in 0..1 + r.below(4) {
        let pick = if must_reply && i == 0 { 0 } else { r.below(12) };
        match pick {
            0..=2 => {
                let reply = (must_reply && i == 0) || r.chance(15);
                let (dsz, osz, extra) = objects(r, m, proc, DATA);
                let td = BinderTransactionData {
                    target: if r.chance(50) { 0 } else { small(r) },
                    cookie: 0,
                    code: r.next_u64() as u32,
                    flags: if r.chance(30) { TF_ONE_WAY } else { 0 }
                        | if r.chance(80) { TF_ACCEPT_FDS } else { 0 },
                    sender_pid: 0,
                    sender_euid: 0,
                    data_size: dsz,
                    offsets_size: osz,
                    data_buffer: DATA,
                    data_offsets: OFFS,
                };
                if extra > 0 || r.chance(20) {
                    put(
                        &mut w,
                        &if reply {
                            BC_REPLY_SG
                        } else {
                            BC_TRANSACTION_SG
                        },
                    );
                    put(&mut w, &td);
                    put(&mut w, &extra);
                } else {
                    put(&mut w, &if reply { BC_REPLY } else { BC_TRANSACTION });
                    put(&mut w, &td);
                }
            }
            3 => {
                let c = [BC_INCREFS, BC_ACQUIRE, BC_RELEASE, BC_DECREFS][r.below(4) as usize];
                put(&mut w, &c);
                put(&mut w, &(small(r) as u32));
            }
            4 => {
                let c = if r.chance(50) {
                    BC_INCREFS_DONE
                } else {
                    BC_ACQUIRE_DONE
                };
                put(&mut w, &c);
                put(&mut w, &(0x1000 + r.below(4) * 0x10));
                put(&mut w, &r.below(3));
            }
            5 => {
                put(&mut w, &BC_FREE_BUFFER);
                // Mostly buffers this process was actually handed.
                let a = if !bufs.is_empty() && r.chance(85) {
                    bufs[r.below(bufs.len() as u64) as usize]
                } else {
                    m.procs.get(&proc).map(|p| p.recv_base).unwrap_or(0) + r.below(64) * 8
                };
                put(&mut w, &a);
            }
            6 => {
                let c = [BC_ENTER_LOOPER, BC_REGISTER_LOOPER, BC_EXIT_LOOPER][r.below(3) as usize];
                put(&mut w, &c);
            }
            7 | 8 => {
                let c = if r.chance(60) {
                    BC_REQUEST_DEATH_NOTIFICATION
                } else {
                    BC_CLEAR_DEATH_NOTIFICATION
                };
                put(&mut w, &c);
                put(&mut w, &(small(r) as u32));
                put(&mut w, &r.below(3));
            }
            9 => {
                put(&mut w, &BC_DEAD_BINDER_DONE);
                put(&mut w, &r.below(3));
            }
            10 => {
                if r.chance(70) {
                    let c = if r.chance(60) {
                        BC_REQUEST_FREEZE_NOTIFICATION
                    } else {
                        BC_CLEAR_FREEZE_NOTIFICATION
                    };
                    put(&mut w, &c);
                    put(&mut w, &(small(r) as u32));
                } else {
                    put(&mut w, &BC_FREEZE_NOTIFICATION_DONE);
                }
                put(&mut w, &r.below(3));
            }
            _ => w.extend((0..r.below(16)).map(|_| r.byte())),
        }
    }
    w
}

/// Remember the buffers handed out in a BR_* stream.
fn harvest(read: &[u8], bufs: &mut Vec<u64>) -> bool {
    let mut got_txn = false;
    let mut off = 0;
    while off + 4 <= read.len() {
        let cmd = u32::from_le_bytes(read[off..off + 4].try_into().unwrap());
        off += 4;
        if matches!(cmd, BR_TRANSACTION | BR_REPLY | BR_TRANSACTION_SEC_CTX)
            && off + 64 <= read.len()
        {
            // binder_transaction_data.data.ptr.buffer
            bufs.push(u64::from_le_bytes(
                read[off + 48..off + 56].try_into().unwrap(),
            ));
            let flags = u32::from_le_bytes(read[off + 20..off + 24].try_into().unwrap());
            got_txn |= cmd != BR_REPLY && flags & TF_ONE_WAY == 0;
            if bufs.len() > 16 {
                bufs.remove(0);
            }
        }
        off += ((cmd >> 16) & 0x3fff) as usize;
    }
    got_txn
}

pub fn run(iterations: u64, seed: u64) {
    let trace = std::env::var_os("LINUXAPK_BROKER_FUZZ_TRACE").is_some();
    if trace {
        linuxapk_broker::DEBUG.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    let mut r = Rng(seed ^ 0xA5A5_1234);
    for it in 0..iterations {
        if trace {
            eprintln!("=== iteration {it}");
        }
        let mut b = BinderDriver::new();
        let mut m = FakeUserspace::default();
        // Userspace failures, each operation failing with probability
        // fault_pct/1000.
        let fault_pct = [0, 0, 5, 20, 80][r.below(5) as usize];
        let mut fr = Rng(r.next_u64());
        m.fault = Some(Box::new(move |_| fr.below(1000) < fault_pct));
        let mut procs: Vec<ProcId> = Vec::new();
        for i in 0..NPROC {
            // Sometimes two binder instances of one pid.
            let pid = if i > 0 && r.chance(15) {
                1000
            } else {
                1000 + i as i32
            };
            let p = b.open(pid, 1000 + i as u32, (r.below(8) == 0) as u8, r.chance(10));
            m.add_proc(p, 0x1_0000_0000 * (i as u64 + 1));
            procs.push(p);
        }
        if r.chance(90) {
            m.ioctl(&mut b, procs[0], 1, BINDER_SET_CONTEXT_MGR, 0);
        }
        // Most threads are loopers, as in a real thread pool.
        for &p in &procs {
            for tid in 1..=2 {
                if r.chance(70) {
                    let mut w = Vec::new();
                    put(&mut w, &BC_ENTER_LOOPER);
                    m.write_read(&mut b, p, tid, &w, 0);
                }
            }
        }
        let mut bufs: Vec<Vec<u64>> = vec![Vec::new(); NPROC + 1];
        // Threads that received a synchronous call and owe a reply.
        let mut owe: Vec<(ProcId, u32)> = Vec::new();
        let steps = 1 + r.below(40);
        for _ in 0..steps {
            let mut p = procs[r.below(procs.len() as u64) as usize];
            let mut tid = 1 + r.below(3) as u32;
            let mut op = r.below(25);
            let mut must_reply = false;
            if !owe.is_empty() && r.chance(50) {
                (p, tid) = owe.remove(r.below(owe.len() as u64) as usize);
                op = 21;
                must_reply = true;
            }
            if trace {
                eprintln!("-- proc={p} tid={tid} op={op}");
            }
            let pending = m.pending_calls();
            let interrupted = m.interrupted_calls();
            match op {
                0 => {
                    let n = r.below(256) as usize;
                    let buf: Vec<u8> = (0..n).map(|_| r.byte()).collect();
                    if trace {
                        eprintln!("   raw {:02x?}", &buf[..buf.len().min(32)]);
                    }
                    m.write_read(&mut b, p, tid, &buf, 256);
                }
                1 => {
                    m.ioctl(&mut b, p, tid, BINDER_THREAD_EXIT, 0);
                }
                2 if r.chance(20) => b.release_proc(p, &mut m),
                3 => {
                    m.ioctl(&mut b, p, tid, BINDER_SET_CONTEXT_MGR, 0);
                }
                4..=8 => {
                    let size = [0usize, 8, 64, 256, 4096][r.below(5) as usize];
                    m.write_read(&mut b, p, tid, &[], size);
                }
                9 | 10 if !pending.is_empty() => {
                    // A signal ends a blocked read's wait.
                    m.interrupt(pending[r.below(pending.len() as u64) as usize]);
                }
                11 | 12 if !interrupted.is_empty() => {
                    // The kernel restarts an interrupted ioctl.
                    m.restart(
                        &mut b,
                        interrupted[r.below(interrupted.len() as u64) as usize],
                    );
                }
                22 => {
                    let info = BinderFreezeInfo {
                        pid: 1000 + r.below(4) as u32,
                        enable: r.chance(70) as u32,
                        timeout_ms: [0, 0, 1, 10, 1000][r.below(5) as usize],
                    };
                    let mut v = Vec::new();
                    put(&mut v, &info);
                    m.poke(p, DATA, &v);
                    m.ioctl(&mut b, p, tid, BINDER_FREEZE, DATA);
                }
                23 => {
                    let info = BinderFrozenStatusInfo {
                        pid: 1000 + r.below(4) as u32,
                        ..Default::default()
                    };
                    let mut v = Vec::new();
                    put(&mut v, &info);
                    m.poke(p, DATA, &v);
                    m.ioctl(&mut b, p, tid, BINDER_GET_FROZEN_INFO, DATA);
                }
                24 => {
                    // Time passes (freeze deadlines).
                    let ms = if r.chance(50) {
                        r.below(3)
                    } else {
                        r.below(1200)
                    };
                    m.advance(&mut b, ms);
                }
                _ => {
                    let w = commands(&mut r, &mut m, p, &bufs[p as usize], must_reply);
                    let size = if r.chance(70) {
                        256
                    } else {
                        r.below(300) as usize
                    };
                    m.write_read(&mut b, p, tid, &w, size);
                }
            }
            b.check_invariants();
            // Every answered read hands its buffers and calls to its thread.
            for c in m.take_answered() {
                let o = m.outcome(c);
                let (cp, ct) = m.caller(c);
                if o.errno == 0 && harvest(&o.read, &mut bufs[cp as usize]) {
                    owe.push((cp, ct));
                }
            }
            // Every read still waiting for an answer is one the broker holds.
            let held = b.blocked_threads();
            for c in m.pending_calls() {
                assert!(held.contains(&m.caller(c)), "call {} forgotten", c.id);
            }
        }
        // Every freeze still waiting ends by its deadline.
        m.advance(&mut b, 1001);
        b.check_invariants();
        for p in procs {
            b.release_proc(p, &mut m);
            b.check_invariants();
        }
        // With every process gone, nothing may be left behind.
        assert_eq!(b.counts(), (0, 0, 0), "leaked transactions/buffers/nodes");
        assert_eq!(b.freezer_state(), (0, false), "leaked freezer state");
        assert!(m.captured.is_empty(), "leaked captured fds");
        for (p, fp) in &m.procs {
            assert!(fp.live_recv.is_empty(), "proc {p} leaked receive buffers");
        }
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let iters: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(200_000);
    let seed: u64 = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    // Printed first, so a panic's output says how to reproduce it.
    println!("linuxapk-broker-fuzz: {iters} iterations from seed {seed}");
    run(iters, seed);
    println!("linuxapk-broker-fuzz: no panics");
}

#[cfg(test)]
mod fuzz {
    #[test]
    fn smoke() {
        // A quick bounded campaign across several seeds in `cargo test`.
        for seed in 0..8 {
            super::run(2_000, seed);
        }
    }
}
