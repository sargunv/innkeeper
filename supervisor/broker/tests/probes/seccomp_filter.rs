//! Checks which syscalls the session's filter turns into user notifications.
//! They must be the ones the broker serves and no other hot syscalls. open(2)
//! and the access-token and ashmem ioctls are trapped only for an OpenHarmony
//! guest.
//!
//! A child installs the filter and serves its listener itself. A grandchild
//! makes each probe syscall, and the child records which ones arrived and
//! lets them continue.

use innkeeper_broker::Guest;
use libc::*;

const COMMON: &str = "getuid,getgroups,setresuid,setgroups,fchownat,getsockopt SO_PEERCRED,lstat,access W_OK,fstat,readlinkat,getcwd,setsockopt SO_MARK,capget,capset,mkdirat,fchmodat,prctl PR_CAPBSET_DROP";

pub fn check() -> bool {
    let mut ok = true;
    for (guest, first) in [
        ("android", "bpf,openat,ioctl binder"),
        (
            "ohos",
            "bpf,openat,ioctl binder,open,ioctl access token,ioctl ashmem",
        ),
    ] {
        let want = format!("{first},{COMMON}");
        let out = super::probe(&["seccomp-filter", guest], None)
            .output()
            .unwrap();
        let got = String::from_utf8_lossy(&out.stdout);
        let got = got.trim();
        ok &= super::report(
            out.status.success() && got == want,
            &if got == want {
                format!("{guest} filter traps exactly the calls the broker serves")
            } else {
                format!("{guest} filter traps [{got}], want [{want}]")
            },
        );
    }
    ok
}

struct Probe {
    name: &'static str,
    nr: i64,
    a: [i64; 6],
}

#[repr(C)]
struct Notif {
    id: u64,
    pid: u32,
    flags: u32,
    nr: i32,
    arch: u32,
    ip: u64,
    args: [u64; 6],
}

#[repr(C)]
struct Resp {
    id: u64,
    val: i64,
    error: i32,
    flags: u32,
}

const fn iowr(nr: u32, size: usize) -> u64 {
    ((3 << 30) | ((size as u32) << 16) | ((b'!' as u32) << 8) | nr) as u64
}

/// Prints the notified probes' names, comma-separated, in probe order.
#[allow(non_upper_case_globals)] // libc's syscall numbers keep their UAPI names.
pub fn run(guest: &str) -> i32 {
    let guest = if guest == "ohos" {
        Guest::Ohos
    } else {
        Guest::Android
    };
    let missing = c"/nonexistent".as_ptr() as i64;
    let root = c"/".as_ptr() as i64;
    let mut st: stat = unsafe { std::mem::zeroed() };
    let stp = &mut st as *mut stat as i64;
    let p = |name, nr, a: &[i64]| {
        let mut args = [0; 6];
        args[..a.len()].copy_from_slice(a);
        Probe { name, nr, a: args }
    };
    let probes = [
        p("bpf", SYS_bpf, &[-1]),
        p(
            "openat",
            SYS_openat,
            &[AT_FDCWD as i64, missing, O_RDONLY as i64],
        ),
        p("ioctl binder", SYS_ioctl, &[-1, 0xc0306201]),
        p("ioctl other", SYS_ioctl, &[-1, 0x5401]),
        p("open", SYS_open, &[missing, O_RDONLY as i64]),
        p("ioctl access token", SYS_ioctl, &[-1, 0x80084101]),
        p("ioctl ashmem", SYS_ioctl, &[-1, 0x7704]),
        p("getuid", SYS_getuid, &[]),
        p("getgroups", SYS_getgroups, &[0, 0]),
        p("setresuid", SYS_setresuid, &[-1, -1, -1]),
        p("setgroups", SYS_setgroups, &[0, 0]),
        p(
            "fchownat",
            SYS_fchownat,
            &[AT_FDCWD as i64, missing, -1, -1, 0],
        ),
        p(
            "getsockopt SO_PEERCRED",
            SYS_getsockopt,
            &[-1, SOL_SOCKET as i64, SO_PEERCRED as i64],
        ),
        p(
            "getsockopt SO_TYPE",
            SYS_getsockopt,
            &[-1, SOL_SOCKET as i64, SO_TYPE as i64],
        ),
        p(
            "lstat",
            SYS_newfstatat,
            &[AT_FDCWD as i64, root, stp, AT_SYMLINK_NOFOLLOW as i64],
        ),
        p(
            "access W_OK",
            SYS_faccessat,
            &[AT_FDCWD as i64, root, W_OK as i64],
        ),
        p(
            "access R_OK",
            SYS_faccessat,
            &[AT_FDCWD as i64, root, R_OK as i64],
        ),
        p("stat", SYS_newfstatat, &[AT_FDCWD as i64, root, stp, 0]),
        p("fstat", SYS_fstat, &[0, stp]),
        p("getpid", SYS_getpid, &[]),
        p(
            "readlinkat",
            SYS_readlinkat,
            &[AT_FDCWD as i64, missing, 0, 0],
        ),
        p("getcwd", SYS_getcwd, &[0, 0]),
        p(
            "setsockopt SO_MARK",
            SYS_setsockopt,
            &[-1, SOL_SOCKET as i64, SO_MARK as i64],
        ),
        p(
            "setsockopt SO_RCVBUF",
            SYS_setsockopt,
            &[-1, SOL_SOCKET as i64, SO_RCVBUF as i64],
        ),
        p("capget", SYS_capget, &[0, 0]),
        p("capset", SYS_capset, &[0, 0]),
        p(
            "mkdirat",
            SYS_mkdirat,
            &[AT_FDCWD as i64, c"/nonexistent/x".as_ptr() as i64, 0],
        ),
        p("fchmodat", SYS_fchmodat, &[AT_FDCWD as i64, missing, 0]),
        p(
            "prctl PR_CAPBSET_DROP",
            SYS_prctl,
            &[PR_CAPBSET_DROP as i64, 63],
        ),
        p("prctl PR_SET_NAME", SYS_prctl, &[PR_SET_NAME as i64, 0]),
    ];
    unsafe {
        if prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
            return 1;
        }
    }
    let Ok(lfd) = innkeeper_broker::filter::install(guest) else {
        return 1;
    };
    // From here on, this process's own trapped syscalls would wait for
    // itself, so it makes only raw writes, polls and ioctls.
    let g = unsafe { fork() };
    if g == 0 {
        for p in &probes {
            let a = p.a;
            unsafe { syscall(p.nr, a[0], a[1], a[2], a[3], a[4], a[5]) };
        }
        unsafe { _exit(0) };
    }
    let mut seen = [false; 30];
    loop {
        let mut status = 0;
        if unsafe { waitpid(g, &mut status, WNOHANG) } == g {
            break;
        }
        let mut pfd = pollfd {
            fd: lfd,
            events: POLLIN,
            revents: 0,
        };
        if unsafe { poll(&mut pfd, 1, 100) } <= 0 {
            continue;
        }
        let mut req: Notif = unsafe { std::mem::zeroed() };
        if unsafe { ioctl(lfd, iowr(0, size_of::<Notif>()) as _, &mut req) } != 0 {
            continue;
        }
        for (i, p) in probes.iter().enumerate() {
            let arg = |n: usize| req.args[n] as u32 as i64 == p.a[n] as u32 as i64;
            seen[i] |= req.nr as i64 == p.nr
                && match p.nr {
                    SYS_ioctl => arg(1),
                    SYS_getsockopt | SYS_setsockopt => arg(2),
                    SYS_prctl => arg(0),
                    SYS_newfstatat => arg(3),
                    SYS_faccessat => arg(2),
                    _ => true,
                };
        }
        let resp = Resp {
            id: req.id,
            val: 0,
            error: 0,
            flags: 1, // SECCOMP_USER_NOTIF_FLAG_CONTINUE
        };
        unsafe { ioctl(lfd, iowr(1, size_of::<Resp>()) as _, &resp) };
    }
    let out = probes
        .iter()
        .zip(seen)
        .filter(|(_, s)| *s)
        .map(|(p, _)| p.name)
        .collect::<Vec<_>>()
        .join(",")
        + "\n";
    unsafe { write(1, out.as_ptr().cast(), out.len()) };
    0
}
