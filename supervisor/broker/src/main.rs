//! innkeeper-brk `[--preload <lib>] <init> [<arg>...]` starts the guest's
//! init under the session's seccomp filter and serves that filter as init's
//! parent (supervisor/README.md). It exits with init's status.
//!
//! The broker stays every guest process's ancestor, so process-memory access
//! works under Yama's default `ptrace_scope=1`. As the subreaper, it also
//! reaps orphans. Only init gets `LD_PRELOAD=<lib>` (the shim) and
//! `INNKEEPER_SHIM_ROLE=init`, because init isn't pid 1 here. The broker
//! doesn't load the shim.
use std::ffi::{CString, OsString};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};

fn main() {
    let mut cfg = innkeeper_broker::Config::from_env();
    let mut args: Vec<OsString> = std::env::args_os().skip(1).collect();
    let mut env: Vec<_> = std::env::vars_os()
        .filter(|(k, _)| k != "LD_PRELOAD" && k != "INNKEEPER_SHIM_ROLE")
        .collect();
    if args.first().is_some_and(|a| a == "--preload") && args.len() > 1 {
        let lib = args.drain(..2).nth(1).unwrap();
        env.push(("LD_PRELOAD".into(), lib));
    }
    env.push(("INNKEEPER_SHIM_ROLE".into(), "init".into()));
    assert!(
        !args.is_empty(),
        "usage: innkeeper-brk [--preload <lib>] <init> [<arg>...]"
    );
    let cstr = |b: &[u8]| CString::new(b).expect("NUL in init's argv or environment");
    let argv: Vec<_> = args.iter().map(|a| cstr(a.as_bytes())).collect();
    let env: Vec<_> = env
        .into_iter()
        .map(|(k, v)| {
            let mut s = k.into_vec();
            s.push(b'=');
            s.extend_from_slice(v.as_bytes());
            cstr(&s)
        })
        .collect();

    unsafe {
        if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1) != 0 {
            panic!("subreaper: {}", std::io::Error::last_os_error());
        }
    }
    // The session's bpffs, which holds the bpf emulator's pins. Open it before
    // the guest can mount anything over it.
    let pins = unsafe {
        let fd = libc::open(
            c"/sys/fs/bpf".as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        );
        (fd >= 0).then(|| OwnedFd::from_raw_fd(fd))
    };
    let mut sock = [0; 2];
    if unsafe {
        libc::socketpair(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
            0,
            sock.as_mut_ptr(),
        )
    } != 0
    {
        panic!("socketpair: {}", std::io::Error::last_os_error());
    }
    // The process is still single-threaded, so the child may run Rust until
    // it execs.
    let init = unsafe { libc::fork() };
    if init < 0 {
        panic!("fork: {}", std::io::Error::last_os_error());
    }
    if init == 0 {
        unsafe { libc::close(sock[0]) };
        start_init(cfg.guest, sock[1], &argv, &env);
    }
    unsafe { libc::close(sock[1]) };
    let listener = receive_fd(sock[0]).unwrap_or_else(|| {
        eprintln!("broker: init did not start");
        std::process::exit(1)
    });
    unsafe { libc::close(sock[0]) };
    std::thread::spawn(move || reap(init));
    unsafe { libc::umask(0) };
    cfg.init = init;
    innkeeper_broker::notify::serve(listener, pins, cfg);
}

fn reap(init: i32) -> ! {
    loop {
        let mut status = 0;
        let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
        if pid == init {
            let code = if libc::WIFEXITED(status) {
                libc::WEXITSTATUS(status)
            } else {
                128 + libc::WTERMSIG(status)
            };
            std::process::exit(code);
        }
        if pid < 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            eprintln!("broker: waitpid: {}", std::io::Error::last_os_error());
            std::process::exit(1);
        }
    }
}

/// Runs in the forked child. Installs the filter, sends its listener over
/// `sock` and execs init.
fn start_init(guest: innkeeper_broker::Guest, sock: i32, argv: &[CString], env: &[CString]) -> ! {
    let mut argv_ptr: Vec<_> = argv.iter().map(|s| s.as_ptr()).collect();
    argv_ptr.push(std::ptr::null());
    let mut env_ptr: Vec<_> = env.iter().map(|s| s.as_ptr()).collect();
    env_ptr.push(std::ptr::null());
    let listener = match innkeeper_broker::filter::install(guest) {
        Ok(fd) => fd,
        Err(e) => {
            eprintln!("broker: install seccomp filter: {e}");
            unsafe { libc::_exit(1) };
        }
    };
    unsafe {
        // A usize buffer gives cmsghdr its required alignment.
        let mut control = [0usize; 4];
        let mut byte = b'L';
        let mut io = libc::iovec {
            iov_base: (&mut byte as *mut u8).cast(),
            iov_len: 1,
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut io;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(size_of::<i32>() as _) as _;
        let cm = libc::CMSG_FIRSTHDR(&msg);
        (*cm).cmsg_level = libc::SOL_SOCKET;
        (*cm).cmsg_type = libc::SCM_RIGHTS;
        (*cm).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as _) as _;
        libc::CMSG_DATA(cm).cast::<i32>().write(listener);
        if libc::sendmsg(sock, &msg, 0) != 1 {
            libc::_exit(1);
        }
        libc::close(listener);
        libc::close(sock);
        libc::execve(argv[0].as_ptr(), argv_ptr.as_ptr(), env_ptr.as_ptr());
        eprintln!("broker: exec init: {}", std::io::Error::last_os_error());
        libc::_exit(1);
    }
}

/// Receives the fd sent over `sock`.
fn receive_fd(sock: i32) -> Option<i32> {
    unsafe {
        let mut control = [0usize; 4];
        let mut byte = 0u8;
        let mut io = libc::iovec {
            iov_base: (&mut byte as *mut u8).cast(),
            iov_len: 1,
        };
        let mut msg: libc::msghdr = std::mem::zeroed();
        msg.msg_iov = &mut io;
        msg.msg_iovlen = 1;
        msg.msg_control = control.as_mut_ptr().cast();
        msg.msg_controllen = libc::CMSG_SPACE(size_of::<i32>() as _) as _;
        if libc::recvmsg(sock, &mut msg, libc::MSG_CMSG_CLOEXEC) != 1 {
            return None;
        }
        let cm = libc::CMSG_FIRSTHDR(&msg);
        if cm.is_null() || (*cm).cmsg_type != libc::SCM_RIGHTS {
            return None;
        }
        Some(libc::CMSG_DATA(cm).cast::<i32>().read_unaligned())
    }
}
