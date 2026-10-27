//! The ubinder broker helper: a tiny dynamic bionic binary the session's
//! `pivot-exec` forks and execs with the binder seccomp listener fd as argv[1].
//! It runs in the supervisor's (outer) namespaces and serves binder for every
//! Android process. See `ubinder/src/sys.rs` and `docs/ubinder.md`.

fn main() {
    let mut args = std::env::args().skip(1);
    let sock: i32 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("usage: linuxapk-ubinder <forward-sock-fd> <listener-fd>");
    let listener: i32 = args
        .next()
        .and_then(|s| s.parse().ok())
        .expect("missing <listener-fd>");
    #[cfg(any(target_os = "linux", target_os = "android"))]
    ubinder::sys::serve(sock, listener);
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    {
        let _ = (sock, listener);
        eprintln!("ubinder broker is Linux-only");
    }
}
