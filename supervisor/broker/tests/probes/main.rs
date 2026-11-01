//! The real filter and broker against real processes on the host, without an
//! Android image, devices or privileges. Each probe runs in a fresh process of
//! this binary (`--probe <name> ...`), under `unshare -Urm` where it needs a
//! mount namespace, so it starts single-threaded and can fork a filtered
//! child.

mod binder_restarts;
mod bpfemu;
mod drm_node;
mod input_sysfs;
mod seccomp_filter;

use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use innkeeper_broker::{Config, Guest};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--probe") {
        let rest = &args[i + 2..];
        let code = match args[i + 1].as_str() {
            "seccomp-filter" => seccomp_filter::run(&rest[0]),
            "bpfemu" => bpfemu::run(Path::new(&rest[0])),
            "binder-restarts" => binder_restarts::run(),
            "drm-node" => drm_node::run(rest.first().map_or("", String::as_str)),
            "input-sysfs" => input_sysfs::run(),
            p => panic!("unknown probe {p}"),
        };
        std::process::exit(code);
    }
    let scratch = Path::new(env!("CARGO_TARGET_TMPDIR")).join("supervisor-probes");
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let mut failed = false;
    for (name, ok) in [
        ("seccomp-filter", seccomp_filter::check()),
        ("bpfemu", bpfemu::check(&scratch)),
        ("binder-restarts", binder_restarts::check()),
        ("drm-node", drm_node::check(&scratch)),
        ("input-sysfs", input_sysfs::check(&scratch)),
        ("bootstrap", bootstrap()),
    ] {
        if !ok {
            println!("FAIL {name}");
            failed = true;
        }
    }
    std::process::exit(failed as i32);
}

/// This binary as `--probe <args>`, optionally inside `unshare -Urm sh -c
/// <setup>`. The setup's `$@` is the probe command.
fn probe(args: &[&str], setup: Option<&str>) -> Command {
    let me = std::env::current_exe().unwrap();
    match setup {
        None => {
            let mut c = Command::new(me);
            c.arg("--probe").args(args);
            c
        }
        Some(setup) => {
            let mut c = Command::new("unshare");
            c.args(["-Urm", "sh", "-c", &format!("{setup} && exec \"$@\""), "sh"])
                .arg(me)
                .arg("--probe")
                .args(args);
            c
        }
    }
}

/// Runs `c` with a deadline. Returns its status, or None if it had to be
/// killed.
fn run(c: &mut Command, secs: u64) -> Option<ExitStatus> {
    let mut child = c.stdin(Stdio::null()).spawn().unwrap();
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return Some(s);
        }
        if Instant::now() > end {
            let _ = child.kill();
            let _ = child.wait();
            println!("FAIL timed out after {secs} s: {c:?}");
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The broker as the launcher runs it. Init, a shell here, is its child,
/// under the filter and with the shim preloaded. Init's status is the
/// broker's, even with Yama off. Without a guest the broker starts nothing.
fn bootstrap() -> bool {
    let broker = |guest: Option<&str>, script: &str| {
        let mut c = Command::new(env!("CARGO_BIN_EXE_innkeeper-brk"));
        c.args(["--preload", "libc.so.6", "/bin/sh", "-c", script])
            .env_remove("INNKEEPER_GUEST");
        match guest {
            Some(g) => c.env("INNKEEPER_GUEST", g),
            None => c.stderr(std::process::Stdio::null()),
        };
        unsafe {
            c.pre_exec(|| {
                libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0);
                Ok(())
            })
        };
        run(&mut c, 5).and_then(|s| s.code())
    };
    let parent = broker(
        Some("android"),
        r#"test "$(cat /proc/$PPID/comm)" = innkeeper-brk || exit 99
           test "$LD_PRELOAD" = libc.so.6 && test "$INNKEEPER_SHIM_ROLE" = init || exit 98
           exit 42"#,
    );
    let signal = broker(Some("android"), "kill -TERM $$");
    let unset = broker(None, "exit 0");
    report(
        parent == Some(42),
        &format!(
            "init is the broker's child with the shim preloaded, its status the broker's ({parent:?})"
        ),
    ) & report(
        signal == Some(143),
        &format!("init killed by a signal: 128 + the signal ({signal:?})"),
    ) & report(
        unset.is_some_and(|s| s != 0),
        &format!("no INNKEEPER_GUEST: the broker refuses to start init ({unset:?})"),
    )
}

fn report(ok: bool, what: &str) -> bool {
    println!("{} {what}", if ok { "ok  " } else { "FAIL" });
    ok
}

fn status_ok(s: Option<ExitStatus>) -> bool {
    s.is_some_and(|s| s.success())
}

fn session(guest: Guest, render_node: Option<&str>) -> Config {
    Config {
        guest,
        root_map: Vec::new(),
        render_node: render_node.map(Into::into),
        init: 0,
    }
}

/// Forks a child that installs the session's filter and runs `child`, serves
/// its notifications with the broker on a thread, and returns the child's
/// exit status, or 128 plus the signal that killed it. Call it while the
/// process is single-threaded.
fn under_broker(cfg: Config, pins: Option<OwnedFd>, child: impl FnOnce() -> i32) -> i32 {
    let mut sp = [0; 2];
    assert_eq!(
        unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_SEQPACKET, 0, sp.as_mut_ptr()) },
        0
    );
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork");
    if pid == 0 {
        unsafe {
            libc::close(sp[0]);
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0 {
                libc::_exit(1);
            }
        }
        let Ok(fd) = innkeeper_broker::filter::install(cfg.guest) else {
            unsafe { libc::_exit(1) };
        };
        send_fd(sp[1], fd);
        unsafe { libc::close(fd) };
        let code = child();
        use std::io::Write;
        let _ = std::io::stdout().flush();
        unsafe { libc::_exit(code) };
    }
    unsafe { libc::close(sp[1]) };
    let listener = recv_fd(sp[0]).expect("listener");
    std::thread::spawn(move || innkeeper_broker::notify::serve(listener, pins, cfg));
    let mut status = 0;
    unsafe { libc::waitpid(pid, &mut status, 0) };
    ExitStatus::from_raw(status)
        .code()
        .unwrap_or_else(|| 128 + libc::WTERMSIG(status))
}

fn send_fd(sock: RawFd, fd: RawFd) {
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
        let cm = libc::CMSG_FIRSTHDR(&msg);
        (*cm).cmsg_level = libc::SOL_SOCKET;
        (*cm).cmsg_type = libc::SCM_RIGHTS;
        (*cm).cmsg_len = libc::CMSG_LEN(size_of::<i32>() as _) as _;
        libc::CMSG_DATA(cm).cast::<i32>().write_unaligned(fd);
        assert_eq!(libc::sendmsg(sock, &msg, 0), 1, "send listener");
    }
}

fn recv_fd(sock: RawFd) -> Option<RawFd> {
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
        if libc::recvmsg(sock, &mut msg, 0) != 1 {
            return None;
        }
        let cm = libc::CMSG_FIRSTHDR(&msg);
        (!cm.is_null()).then(|| libc::CMSG_DATA(cm).cast::<i32>().read_unaligned())
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// An `O_PATH` directory fd, as the broker opens its pin directory.
fn open_dir(path: &std::ffi::CStr) -> Option<OwnedFd> {
    let fd = unsafe {
        libc::open(
            path.as_ptr(),
            libc::O_PATH | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
}
