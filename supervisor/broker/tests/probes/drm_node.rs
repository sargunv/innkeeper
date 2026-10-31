//! openat dispatch through the real filter: a guest opens only the selected
//! DRM render node (none for software rendering), and binder still opens.
//! Fake nodes in a private mount namespace; never a host GPU.

use std::path::Path;

use innkeeper_broker::Guest;

pub fn check(scratch: &Path) -> bool {
    let dev = scratch.join("dev");
    std::fs::create_dir_all(dev.join("dri")).unwrap();
    for n in ["renderD128", "renderD129"] {
        std::fs::write(dev.join("dri").join(n), []).unwrap();
    }
    let setup = format!("mount --bind '{}' /dev", dev.display());
    let mut ok = true;
    for selected in ["/dev/dri/renderD129", ""] {
        let mut c = super::probe(&["drm-node", selected], Some(&setup));
        ok &= super::report(
            super::status_ok(super::run(&mut c, 10)),
            &format!("only the selected DRM node ({selected:?}) opens; binder still opens"),
        );
    }
    ok
}

pub fn run(selected: &str) -> i32 {
    let cfg = super::session(Guest::Android, (!selected.is_empty()).then_some(selected));
    let selected = selected.to_owned();
    super::under_broker(cfg, None, move || {
        let mut fails = 0;
        for node in ["/dev/dri/renderD128", "/dev/dri/renderD129", "/dev/binder"] {
            let path = std::ffi::CString::new(node).unwrap();
            let fd = unsafe { libc::open(path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
            let errno = super::errno();
            let allowed = node == "/dev/binder" || node == selected;
            if (allowed && fd < 0) || (!allowed && (fd >= 0 || errno != libc::EACCES)) {
                println!("FAIL {node} selected={selected:?} fd={fd} errno={errno}");
                fails += 1;
            }
            if fd >= 0 {
                unsafe { libc::close(fd) };
            }
        }
        fails
    })
}
