//! The bpf() emulator (src/bpf.rs) through the real filter. Checks that maps,
//! pins and netd's OFD map locks work, and that the simple bounds hold. The
//! bounds cover element operations on ring buffers, LPM prefixes, pins as
//! regular files under /sys/fs/bpf, impossible sizes, and the object and
//! memory caps. Runs in a private user and mount namespace with a tmpfs on
//! /sys/fs/bpf, as the session has.

use std::ffi::{CStr, CString};
use std::path::Path;

use innkeeper_broker::Guest;
use libc::*;

pub fn check(scratch: &Path) -> bool {
    let outside = scratch.join("bpf-outside");
    std::fs::create_dir_all(&outside).unwrap();
    let mut c = super::probe(
        &["bpfemu", outside.to_str().unwrap()],
        Some("mount -t tmpfs bpf /sys/fs/bpf"),
    );
    super::report(super::status_ok(super::run(&mut c, 60)), "bpf emulation")
}

const MAP_CREATE: i64 = 0;
const MAP_LOOKUP_ELEM: i64 = 1;
const MAP_UPDATE_ELEM: i64 = 2;
const PROG_LOAD: i64 = 5;
const OBJ_PIN: i64 = 6;
const OBJ_GET: i64 = 7;
const OBJ_GET_INFO_BY_FD: i64 = 15;
const HASH: u32 = 1;
const ARRAY: u32 = 2;
const PERCPU_HASH: u32 = 5;
const LPM_TRIE: u32 = 11;
const RINGBUF: u32 = 27;
const USER_RINGBUF: u32 = 31;

/// `union bpf_attr`, as wide as the broker reads it.
struct Attr([u8; 168]);

impl Attr {
    fn new() -> Attr {
        Attr([0; 168])
    }
    fn u32(mut self, off: usize, v: u32) -> Attr {
        self.0[off..off + 4].copy_from_slice(&v.to_ne_bytes());
        self
    }
    fn u64(mut self, off: usize, v: u64) -> Attr {
        self.0[off..off + 8].copy_from_slice(&v.to_ne_bytes());
        self
    }
    /// The result, or -errno.
    fn call(&self, cmd: i64) -> i64 {
        let r = unsafe { syscall(SYS_bpf, cmd, self.0.as_ptr(), self.0.len()) };
        if r < 0 { -super::errno() as i64 } else { r }
    }
}

fn ptr<T>(v: &T) -> u64 {
    v as *const T as u64
}

fn map_create(ty: u32, key: u32, val: u32, max: u32) -> i64 {
    Attr::new()
        .u32(0, ty)
        .u32(4, key)
        .u32(8, val)
        .u32(12, max)
        .call(MAP_CREATE)
}

/// An element operation. `val` is written for a lookup.
fn elem<K>(cmd: i64, fd: i64, key: &K, val: *mut u64) -> i64 {
    Attr::new()
        .u32(0, fd as u32)
        .u64(8, ptr(key))
        .u64(16, val as u64)
        .call(cmd)
}

fn pin(fd: i64, path: &CStr) -> i64 {
    Attr::new()
        .u64(0, path.as_ptr() as u64)
        .u32(8, fd as u32)
        .call(OBJ_PIN)
}

fn get(path: &CStr) -> i64 {
    Attr::new().u64(0, path.as_ptr() as u64).call(OBJ_GET)
}

/// `bpf_map_info.id`, or 0.
fn map_id(fd: i64) -> u32 {
    let info = [0u32; 32];
    let r = Attr::new()
        .u32(0, fd as u32)
        .u32(4, size_of_val(&info) as u32)
        .u64(8, ptr(&info))
        .call(OBJ_GET_INFO_BY_FD);
    if r == 0 { info[1] } else { 0 }
}

struct Checks(u32);

impl Checks {
    fn check(&mut self, ok: bool, what: &str) {
        if !super::report(ok, what) {
            self.0 += 1;
        }
    }
}

pub fn run(outside: &Path) -> i32 {
    // Make room for more objects than the cap, since each holds a memfd.
    unsafe {
        let mut rl: rlimit = std::mem::zeroed();
        getrlimit(RLIMIT_NOFILE, &mut rl);
        rl.rlim_cur = rl.rlim_max;
        setrlimit(RLIMIT_NOFILE, &rl);
    }
    // As the broker does before the guest runs.
    let pins = super::open_dir(c"/sys/fs/bpf");
    let outside = outside.to_str().unwrap().to_owned();
    super::under_broker(super::session(Guest::Android, None), pins, move || {
        let mut c = Checks(0);
        tests(&mut c, &outside);
        (c.0 > 0) as i32
    })
}

fn tests(c: &mut Checks, outside: &str) {
    let fstat_of = |fd: i64| unsafe {
        let mut st: stat = std::mem::zeroed();
        (fstat(fd as i32, &mut st) == 0).then_some((st.st_dev, st.st_ino))
    };
    let ofd_lock = |fd: i64, ty: i32| unsafe {
        let mut l: flock = std::mem::zeroed();
        l.l_type = ty as i16;
        l.l_whence = SEEK_SET as i16;
        fcntl(fd as i32, F_OFD_SETLK, &l) == 0
    };

    // A working map, pinned and fetched back.
    let m = map_create(HASH, 4, 8, 16);
    let k = 7u32;
    let mut v = 42u64;
    let v = &mut v as *mut u64;
    let mut got = 0u64;
    let got = &mut got as *mut u64;
    c.check(
        m >= 0 && elem(MAP_UPDATE_ELEM, m, &k, v) == 0,
        "hash map create and update",
    );
    c.check(pin(m, c"/sys/fs/bpf/m") == 0, "pin under /sys/fs/bpf");
    let g = get(c"/sys/fs/bpf/m");
    c.check(
        g >= 0 && elem(MAP_LOOKUP_ELEM, g, &k, got) == 0 && unsafe { got.read_volatile() } == 42,
        "get pin and lookup",
    );

    // netd's mapLockTest needs one inode but independent open descriptions.
    let g2 = get(c"/sys/fs/bpf/m");
    c.check(
        g2 >= 0 && fstat_of(m).is_some() && fstat_of(m) == fstat_of(g2),
        "map opens share one inode",
    );
    c.check(ofd_lock(g, F_RDLCK), "map read OFD lock");
    c.check(
        !ofd_lock(g2, F_WRLCK) && super::errno() == EAGAIN,
        "independent map open conflicts with OFD lock",
    );
    unsafe { close(g as i32) };
    c.check(ofd_lock(g2, F_WRLCK), "closing map open releases OFD lock");
    unsafe { close(g2 as i32) };

    // Element operations on ring buffers, which have no tables.
    let ur = map_create(USER_RINGBUF, 0, 0, 4096);
    let rb = map_create(RINGBUF, 0, 0, 4096);
    c.check(ur >= 0 && rb >= 0, "ring buffer create");
    c.check(
        elem(MAP_UPDATE_ELEM, ur, &k, v) == -EINVAL as i64,
        "user ring buffer update: EINVAL",
    );
    c.check(
        elem(MAP_LOOKUP_ELEM, rb, &k, got) == -EINVAL as i64,
        "ring buffer lookup: EINVAL",
    );

    // LPM prefixes must fit the key.
    c.check(
        map_create(LPM_TRIE, 4, 8, 16) == -EINVAL as i64,
        "LPM key without data: EINVAL",
    );
    let lpm = map_create(LPM_TRIE, 8, 8, 16);
    let mut key = [33u32.to_ne_bytes(), [10, 0, 0, 1]];
    c.check(
        lpm >= 0 && elem(MAP_UPDATE_ELEM, lpm, &key, v) == -EINVAL as i64,
        "LPM prefix 33 in 4 bytes: EINVAL",
    );
    key[0] = 32u32.to_ne_bytes();
    c.check(
        elem(MAP_UPDATE_ELEM, lpm, &key, v) == 0 && elem(MAP_LOOKUP_ELEM, lpm, &key, got) == 0,
        "LPM prefix 32",
    );

    // Pins are regular files under /sys/fs/bpf, opened without blocking or
    // following links out of it.
    let cs = |s: String| CString::new(s).unwrap();
    unsafe { mkfifo(c"/sys/fs/bpf/fifo".as_ptr(), 0o600) };
    c.check(
        get(c"/sys/fs/bpf/fifo") == -EINVAL as i64,
        "get of a FIFO: EINVAL, no hang",
    );
    let esc = cs(format!("{outside}/escaped"));
    c.check(
        unsafe {
            symlink(
                cs(outside.to_owned()).as_ptr(),
                c"/sys/fs/bpf/link".as_ptr(),
            )
        } == 0,
        "symlink into the pin dir",
    );
    c.check(
        pin(m, c"/sys/fs/bpf/link/escaped") == -ELOOP as i64
            && unsafe { access(esc.as_ptr(), F_OK) } != 0,
        "pin through a symlink: ELOOP, nothing created outside",
    );
    c.check(
        unsafe { symlink(esc.as_ptr(), c"/sys/fs/bpf/flink".as_ptr()) } == 0,
        "symlink to a file outside",
    );
    c.check(
        get(c"/sys/fs/bpf/flink") == -ELOOP as i64,
        "get of a symlink: ELOOP",
    );
    c.check(
        get(c"/sys/fs/bpf/../../../etc/passwd") == -EXDEV as i64,
        "get with ..: EXDEV",
    );
    c.check(
        pin(m, &cs(format!("{outside}/m"))) == -EINVAL as i64,
        "pin outside /sys/fs/bpf: EINVAL",
    );
    let magic = unsafe {
        let mut s: statfs = std::mem::zeroed();
        (libc::statfs(c"/sys/fs/bpf/m".as_ptr(), &mut s) == 0).then_some(s.f_type)
    };
    c.check(magic == Some(0xcafe4a11), "statfs of a pin: bpffs magic");
    // The broker's pin directory was fixed at start, so a mount over
    // /sys/fs/bpf, such as FUSE in a session, doesn't change what it opens.
    if unsafe {
        mount(
            c"over".as_ptr(),
            c"/sys/fs/bpf".as_ptr(),
            c"tmpfs".as_ptr(),
            0,
            std::ptr::null(),
        )
    } == 0
    {
        c.check(
            pin(m, c"/sys/fs/bpf/over") == 0
                && unsafe { access(c"/sys/fs/bpf/over".as_ptr(), F_OK) } != 0,
            "pin after a mount over /sys/fs/bpf: in the session's directory",
        );
        let o = get(c"/sys/fs/bpf/over");
        c.check(
            o >= 0 && elem(MAP_LOOKUP_ELEM, o, &k, got) == 0,
            "get after the over-mount",
        );
        unsafe { umount2(c"/sys/fs/bpf".as_ptr(), MNT_DETACH) };
    } else {
        c.check(false, "mount over /sys/fs/bpf");
    }

    // Impossible sizes fail up front, and failures leave nothing behind.
    let before = map_id(lpm); // the newest map
    c.check(
        map_create(HASH, 4, 4096, 1 << 24) == -ENOMEM as i64,
        "64 GiB map: ENOMEM",
    );
    c.check(
        map_create(PERCPU_HASH, 4, 1 << 30, 1) == -E2BIG as i64,
        "huge per-CPU value: E2BIG",
    );
    let next = map_create(ARRAY, 4, 4, 1);
    let last = map_id(next);
    c.check(
        before != 0 && last == before + 1,
        &format!("failed creations unregistered (id {last} after {before})"),
    );

    // Aggregate map memory is capped. Each map here takes 64 MiB.
    let n = (0..64)
        .take_while(|_| {
            let f = map_create(ARRAY, 4, 16384, 4096);
            unsafe { close(f as i32) };
            f >= 0
        })
        .count();
    c.check(n < 64, &format!("map memory capped after {n} x 64 MiB"));

    // The object count is capped.
    let n = (0..5000)
        .take_while(|_| {
            let f = Attr::new().call(PROG_LOAD);
            unsafe { close(f as i32) };
            f >= 0
        })
        .count();
    c.check(n < 5000, &format!("objects capped after {n} programs"));
}
