//! Binder through the real filter and broker: concurrent opens and fd reuse,
//! one-way ordering, replies, and reads that signals interrupt and the
//! client restarts without replaying commands.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use innkeeper_broker::Guest;
use innkeeper_broker::binder::uapi::*;
use libc::*;

const CLIENTS: usize = 8;
const CALLS: u32 = 32;
const MAP_SIZE: usize = 1 << 20;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
struct Message {
    client: u32,
    sequence: u32,
    oneway: u32,
}

/// Interrupted reads in this process.
static INTERRUPTED: AtomicU32 = AtomicU32::new(0);
static STOP_SIGNALS: AtomicBool = AtomicBool::new(false);

pub fn check() -> bool {
    super::report(
        super::status_ok(super::run(
            &mut super::probe(&["binder-restarts"], None),
            20,
        )),
        "concurrent binder with interrupted reads",
    )
}

pub fn run() -> i32 {
    super::under_broker(super::session(Guest::Android, None), None, clients)
}

fn fail(what: &str) -> ! {
    eprintln!("FAIL {what}: {}", std::io::Error::last_os_error());
    unsafe { _exit(1) }
}

extern "C" fn on_signal(_: c_int) {}

fn bytes<T>(v: &T) -> &[u8] {
    unsafe { std::slice::from_raw_parts((v as *const T).cast(), size_of::<T>()) }
}

fn ioc_size(cmd: u32) -> usize {
    ((cmd >> 16) & 0x3fff) as usize
}

fn append(buf: &mut Vec<u8>, cmd: u32, data: &[u8]) {
    assert_eq!(data.len(), ioc_size(cmd));
    buf.extend_from_slice(&cmd.to_ne_bytes());
    buf.extend_from_slice(data);
}

fn read<T>(buf: &[u8], off: usize) -> T {
    assert!(off + size_of::<T>() <= buf.len());
    unsafe { buf.as_ptr().add(off).cast::<T>().read_unaligned() }
}

fn exchange(fd: c_int, bwr: &mut BinderWriteRead) {
    while unsafe { ioctl(fd, BINDER_WRITE_READ as _, bwr as *mut BinderWriteRead) } < 0 {
        if super::errno() != EINTR {
            fail("BINDER_WRITE_READ");
        }
        // The driver keeps write_consumed across a restart: never replay.
        INTERRUPTED.fetch_add(1, Ordering::Relaxed);
    }
}

fn binder_open() -> (c_int, *mut c_void) {
    let fd = loop {
        let fd = unsafe { open(c"/dev/binder".as_ptr(), O_RDWR | O_CLOEXEC) };
        if fd >= 0 || super::errno() != EINTR {
            break fd;
        }
    };
    if fd < 0 {
        fail("binder open");
    }
    let map = unsafe {
        mmap(
            std::ptr::null_mut(),
            MAP_SIZE,
            PROT_READ,
            MAP_PRIVATE,
            fd,
            0,
        )
    };
    if map == MAP_FAILED {
        fail("binder mmap");
    }
    (fd, map)
}

/// One BINDER_WRITE_READ of `commands`, reading into `output`.
fn write_read(fd: c_int, commands: &[u8], output: &mut [u8]) -> usize {
    let mut bwr = BinderWriteRead {
        write_size: commands.len() as _,
        write_buffer: commands.as_ptr() as _,
        read_size: output.len() as _,
        read_buffer: output.as_mut_ptr() as _,
        ..Default::default()
    };
    exchange(fd, &mut bwr);
    bwr.read_consumed as usize
}

fn finish(done: c_int) -> ! {
    let n = INTERRUPTED.load(Ordering::Relaxed);
    if unsafe { write(done, (&n as *const u32).cast(), 4) } != 4 {
        fail("done");
    }
    unsafe { _exit(0) }
}

fn server(ready: c_int, done: c_int) -> ! {
    let (fd, _map) = binder_open();
    let zero = 0i32;
    if unsafe { ioctl(fd, BINDER_SET_CONTEXT_MGR as _, &zero) } != 0 {
        fail("context manager");
    }
    if unsafe { write(ready, c"r".as_ptr().cast(), 1) } != 1 {
        fail("ready");
    }
    let mut commands = Vec::with_capacity(4096);
    let mut output = [0u8; 1024];
    let mut replies = [Message::default(); 16];
    let mut seen = [0u32; CLIENTS];
    append(&mut commands, BC_ENTER_LOOPER, &[]);
    let mut received = 0;
    while received < CLIENTS as u32 * CALLS * 2 {
        let n = write_read(fd, &commands, &mut output);
        commands.clear();
        let mut reply_count = 0;
        let mut off = 0;
        while off < n {
            let cmd: u32 = read(&output, off);
            off += 4;
            if cmd == BR_TRANSACTION {
                let td: BinderTransactionData = read(&output, off);
                if td.data_size as usize != size_of::<Message>() || td.offsets_size != 0 {
                    fail("transaction size");
                }
                let m = unsafe { (td.data_buffer as *const Message).read_unaligned() };
                if m.client as usize >= CLIENTS || m.sequence >= CALLS {
                    fail("transaction data");
                }
                if m.oneway != 0 {
                    let s = &mut seen[m.client as usize];
                    if td.flags & TF_ONE_WAY == 0 || m.sequence != *s {
                        fail("oneway order");
                    }
                    *s += 1;
                } else {
                    if td.flags & TF_ONE_WAY != 0 || reply_count >= replies.len() {
                        fail("sync flags");
                    }
                    replies[reply_count] = m;
                    let reply = BinderTransactionData {
                        data_size: size_of::<Message>() as _,
                        data_buffer: &replies[reply_count] as *const Message as _,
                        ..Default::default()
                    };
                    reply_count += 1;
                    append(&mut commands, BC_REPLY, bytes(&reply));
                }
                append(&mut commands, BC_FREE_BUFFER, bytes(&td.data_buffer));
                received += 1;
            } else if ![BR_NOOP, BR_TRANSACTION_COMPLETE, BR_SPAWN_LOOPER].contains(&cmd) {
                eprintln!("FAIL unexpected server command {cmd:#x}");
                unsafe { _exit(1) };
            }
            off += ioc_size(cmd);
        }
    }
    write_read(fd, &commands, &mut []);
    if seen.iter().any(|&s| s != CALLS) {
        fail("oneway count");
    }
    finish(done)
}

fn client(id: u32, done: c_int) -> ! {
    for cycle in 0..CALLS / 8 {
        let (fd, map) = binder_open();
        for n in cycle * 8..cycle * 8 + 8 {
            let messages = [
                Message {
                    client: id,
                    sequence: n,
                    oneway: 1,
                },
                Message {
                    client: id,
                    sequence: n,
                    oneway: 0,
                },
            ];
            let mut commands = Vec::with_capacity(256);
            let mut output = [0u8; 1024];
            for (i, m) in messages.iter().enumerate() {
                let td = BinderTransactionData {
                    flags: if i == 0 { TF_ONE_WAY } else { 0 },
                    data_size: size_of::<Message>() as _,
                    data_buffer: m as *const Message as _,
                    ..Default::default()
                };
                append(&mut commands, BC_TRANSACTION, bytes(&td));
            }
            let mut replied = false;
            while !replied {
                let len = write_read(fd, &commands, &mut output);
                commands.clear();
                let mut off = 0;
                while off < len {
                    let cmd: u32 = read(&output, off);
                    off += 4;
                    if cmd == BR_REPLY {
                        let td: BinderTransactionData = read(&output, off);
                        let m = unsafe { (td.data_buffer as *const Message).read_unaligned() };
                        if td.data_size as usize != size_of::<Message>() || m != messages[1] {
                            fail("reply data");
                        }
                        if replied {
                            fail("duplicate reply");
                        }
                        replied = true;
                        append(&mut commands, BC_FREE_BUFFER, bytes(&td.data_buffer));
                    } else if cmd != BR_NOOP && cmd != BR_TRANSACTION_COMPLETE {
                        eprintln!("FAIL unexpected client command {cmd:#x}");
                        unsafe { _exit(1) };
                    }
                    off += ioc_size(cmd);
                }
            }
            write_read(fd, &commands, &mut []);
        }
        unsafe {
            munmap(map, MAP_SIZE);
            // The next cycle reuses this fd number while the monitor checks
            // for closed binder fds.
            close(fd);
        }
    }
    finish(done)
}

/// In the filtered child: a context manager and CLIENTS clients, all
/// signalled repeatedly while they talk.
fn clients() -> i32 {
    let (mut ready, mut done) = ([0; 2], [0; 2]);
    unsafe {
        if pipe(ready.as_mut_ptr()) != 0 || pipe(done.as_mut_ptr()) != 0 {
            fail("pipes");
        }
        let mut sa: sigaction = std::mem::zeroed();
        sa.sa_sigaction = on_signal as extern "C" fn(c_int) as sighandler_t;
        sigemptyset(&mut sa.sa_mask);
        // No SA_RESTART: reads end with EINTR and the client restarts them.
        if sigaction(SIGUSR1, &sa, std::ptr::null_mut()) != 0 {
            fail("sigaction");
        }
    }
    let mut children = [0; CLIENTS + 1];
    children[0] = unsafe { fork() };
    if children[0] == 0 {
        server(ready[1], done[1]);
    }
    let mut byte = 0u8;
    if unsafe { libc::read(ready[0], (&mut byte as *mut u8).cast(), 1) } != 1 {
        fail("server ready");
    }
    for i in 0..CLIENTS {
        children[i + 1] = unsafe { fork() };
        if children[i + 1] == 0 {
            client(i as u32, done[1]);
        }
    }
    let signaler = std::thread::spawn(move || {
        for _ in 0..100 {
            if STOP_SIGNALS.load(Ordering::Relaxed) {
                break;
            }
            for &c in &children {
                unsafe { kill(c, SIGUSR1) };
            }
            std::thread::sleep(std::time::Duration::from_micros(500));
        }
    });
    let mut interrupted = 0;
    for _ in 0..=CLIENTS {
        let mut n = 0u32;
        if unsafe { libc::read(done[0], (&mut n as *mut u32).cast(), 4) } != 4 {
            fail("completion");
        }
        interrupted += n;
    }
    STOP_SIGNALS.store(true, Ordering::Relaxed);
    // Reap only after the signaler stops using these pids: no reuse race.
    signaler.join().unwrap();
    for c in children {
        let mut status = 0;
        if unsafe { waitpid(c, &mut status, 0) } < 0
            || !WIFEXITED(status)
            || WEXITSTATUS(status) != 0
        {
            fail("child status");
        }
    }
    if interrupted == 0 {
        fail("no interrupted reads exercised");
    }
    println!(
        "ok   concurrent binder: {} requests, ordered oneways, fd reuse, {interrupted} interrupted ioctls",
        CLIENTS as u32 * CALLS * 2
    );
    0
}
