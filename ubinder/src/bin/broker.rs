//! The ubinder broker helper: a tiny dynamic bionic binary the session's
//! `pivot-exec` forks and execs with the binder seccomp listener fd as argv[1].
//! It runs in the supervisor's (outer) namespaces and serves binder for every
//! Android process. See `ubinder/src/sys.rs` and `docs/ubinder.md`.

fn main() {
    let fd: i32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .expect("usage: linuxapk-ubinder <listener-fd>");
    #[cfg(target_os = "linux")]
    ubinder::sys::serve(fd);
    #[cfg(not(target_os = "linux"))]
    {
        let _ = fd;
        eprintln!("ubinder broker is Linux-only");
    }
}
