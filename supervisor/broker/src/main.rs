//! linuxapk-broker: a small dynamic bionic binary that the session's
//! `pivot-exec` forks and execs with two fds, the socket to its bpf emulator
//! thread and the session's seccomp listener. It runs in the outer namespaces
//! and serves binder for every Android process. See `src/notify.rs` and
//! `supervisor/README.md`.

fn main() {
    let mut args = std::env::args().skip(1);
    let sock: i32 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: linuxapk-broker <forward-sock-fd> <listener-fd>");
    let listener: i32 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("missing <listener-fd>");
    // Flatpak mode makes directories for Android processes (src/flat.rs),
    // applying their umask itself.
    if std::env::var("LINUXAPK_BIONIC_HARNESS").as_deref() != Ok("1") {
        unsafe { libc::umask(0) };
    }
    linuxapk_broker::notify::serve(sock, listener);
}
