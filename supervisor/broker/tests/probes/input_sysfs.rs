//! OpenHarmony's MMI finds the broker's evdev devices the way it would find
//! kernel devices, through the real filter. A worker thread watches and lists
//! `/dev/input/`, sees the touchscreen appear, and follows libinput's and
//! MMI's libudev's calls: `stat` for the device number, the
//! `/sys/dev/char` link, `realpath`'s `readlink` of each component, both
//! `uevent` files, and `fstat` of the opened node. The calls are the raw
//! syscalls that OH's musl makes. `/dev` is a private directory, and a fake
//! host node in its `input` stays hidden.

use std::ffi::CString;
use std::path::Path;

use innkeeper_broker::Guest;
use innkeeper_broker::input::sysfs;
use innkeeper_evdev_proto::{Message, TOUCHSCREEN};

pub fn check(scratch: &Path) -> bool {
    let dev = scratch.join("input-dev");
    std::fs::create_dir_all(dev.join("socket/innkeeper")).unwrap();
    std::fs::create_dir_all(dev.join("input")).unwrap();
    std::fs::write(dev.join("input/event3"), []).unwrap();
    let setup = format!("mount --bind '{}' /dev", dev.display());
    let mut c = super::probe(&["input-sysfs"], Some(&setup));
    super::report(
        super::status_ok(super::run(&mut c, 10)),
        "OpenHarmony finds the evdev devices in /dev/input/ and their sysfs view from a worker thread",
    )
}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

fn stat(path: &str) -> Result<libc::stat, i32> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    let r = unsafe { libc::syscall(libc::SYS_stat, c(path).as_ptr(), &mut st) };
    if r == 0 { Ok(st) } else { Err(super::errno()) }
}

fn readlink(path: &str) -> Result<String, i32> {
    let mut b = [0u8; 256];
    let n = unsafe {
        libc::syscall(
            libc::SYS_readlink,
            c(path).as_ptr(),
            b.as_mut_ptr(),
            b.len(),
        )
    };
    if n < 0 {
        return Err(super::errno());
    }
    Ok(String::from_utf8_lossy(&b[..n as usize]).into_owned())
}

fn open(path: &str, flags: i32) -> Result<i32, i32> {
    let fd = unsafe { libc::syscall(libc::SYS_open, c(path).as_ptr(), flags | libc::O_CLOEXEC, 0) };
    if fd < 0 {
        Err(super::errno())
    } else {
        Ok(fd as i32)
    }
}

fn read_file(path: &str) -> Result<String, i32> {
    let fd = open(path, libc::O_RDONLY)?;
    let mut b = vec![0u8; 4096];
    let n = unsafe { libc::read(fd, b.as_mut_ptr().cast(), b.len()) };
    unsafe { libc::close(fd) };
    Ok(String::from_utf8_lossy(&b[..n.max(0) as usize]).into_owned())
}

fn list(path: &str) -> Vec<String> {
    let fd = open(path, libc::O_RDONLY | libc::O_DIRECTORY).unwrap();
    let mut buf = vec![0u8; 4096];
    let n = unsafe { libc::syscall(libc::SYS_getdents64, fd, buf.as_mut_ptr(), buf.len()) };
    unsafe { libc::close(fd) };
    let mut names = Vec::new();
    let mut at = 0;
    while at < n.max(0) as usize {
        let len = u16::from_ne_bytes([buf[at + 16], buf[at + 17]]) as usize;
        let name = &buf[at + 19..at + len];
        let name = &name[..name.iter().position(|&b| b == 0).unwrap_or(name.len())];
        names.push(String::from_utf8_lossy(name).into_owned());
        at += len;
    }
    names
}

/// How MMI's libudev resolves the `/sys/dev/char` link.
fn resolve(link: &str, target: &str) -> String {
    let ups = target.matches("../").count();
    let parts: Vec<_> = link.split('/').collect();
    format!(
        "{}/{}",
        parts[..parts.len() - ups - 1].join("/"),
        target.trim_start_matches("../")
    )
}

fn walk() -> Result<(), String> {
    let watch = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if unsafe { libc::inotify_add_watch(watch, c("/dev/input/").as_ptr(), libc::IN_CREATE) } < 0 {
        return Err(format!("inotify_add_watch: errno {}", super::errno()));
    }
    let names = list("/dev/input/");
    if names.iter().any(|n| n == "event3") || !names.iter().any(|n| n == "innkeeper-event0") {
        return Err(format!("/dev/input/ lists {names:?}"));
    }
    let socket = std::os::unix::net::UnixDatagram::unbound().unwrap();
    let enable = Message::Device {
        device: TOUCHSCREEN,
        enabled: true,
    };
    socket
        .send_to(&enable.encode(), "/dev/socket/innkeeper/evdev.sock")
        .map_err(|e| format!("evdev.sock: {e}"))?;
    let mut events = [0u8; 256];
    let n = unsafe { libc::read(watch, events.as_mut_ptr().cast(), events.len()) };
    let node = format!("innkeeper-event{TOUCHSCREEN}");
    if n <= 0 || !String::from_utf8_lossy(&events[..n as usize]).contains(&node) {
        return Err("no IN_CREATE for the touchscreen".into());
    }
    let devnode = format!("/dev/input/{node}");
    let st = stat(&devnode).map_err(|e| format!("stat {devnode}: errno {e}"))?;
    if st.st_mode & libc::S_IFMT != libc::S_IFCHR || st.st_rdev != sysfs::rdev(TOUCHSCREEN) {
        return Err(format!("stat {devnode}: mode {:o}", st.st_mode));
    }
    if readlink(&devnode) != Err(libc::EINVAL) {
        return Err(format!("readlink {devnode} is not EINVAL"));
    }
    let link = format!(
        "/sys/dev/char/{}:{}",
        libc::major(st.st_rdev),
        libc::minor(st.st_rdev)
    );
    let target = readlink(&link).map_err(|e| format!("readlink {link}: errno {e}"))?;
    let event = resolve(&link, &target);
    // MMI refuses a device whose sysname is not event<N>.
    if !event.ends_with(&format!("/event{TOUCHSCREEN}")) {
        return Err(format!("the event node is {event}"));
    }
    let device = event.rsplit_once('/').unwrap().0.to_owned();
    for path in [&device, &event, &format!("{event}/uevent")] {
        if readlink(path) != Err(libc::EINVAL) {
            return Err(format!("readlink {path} is not EINVAL"));
        }
    }
    let uevent = stat(&format!("{event}/uevent")).map_err(|e| format!("stat uevent: {e}"))?;
    if uevent.st_mode & libc::S_IFMT != libc::S_IFREG {
        return Err("the event node's uevent is not a file".into());
    }
    let text = read_file(&format!("{event}/uevent")).map_err(|e| format!("uevent: {e}"))?;
    if !text.contains(&format!("DEVNAME=input/{node}\n")) {
        return Err(format!("event uevent: {text}"));
    }
    let text = read_file(&format!("{device}/uevent")).map_err(|e| format!("uevent: {e}"))?;
    if !text.contains("PROP=2\n") || !text.contains("ABS=") {
        return Err(format!("device uevent: {text}"));
    }
    let fd = open(&devnode, libc::O_RDWR | libc::O_NONBLOCK).map_err(|e| format!("open: {e}"))?;
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::syscall(libc::SYS_fstat, fd, &mut st) } != 0
        || st.st_mode & libc::S_IFMT != libc::S_IFCHR
        || st.st_rdev != sysfs::rdev(TOUCHSCREEN)
    {
        return Err("fstat of the opened node is not its device".into());
    }
    let mut name = [0u8; 64];
    let eviocgname = 0x80004506u32 | (name.len() as u32) << 16;
    let n = unsafe { libc::ioctl(fd, eviocgname as _, name.as_mut_ptr()) };
    if n <= 0 || !name.starts_with(b"Innkeeper Touchscreen\0") {
        return Err("EVIOCGNAME".into());
    }
    // A host path outside the view stays the host's.
    if stat("/dev/input/event3").is_err() {
        return Err("stat of a host node failed".into());
    }
    Ok(())
}

pub fn run() -> i32 {
    let cfg = super::session(Guest::Ohos, None);
    super::under_broker(cfg, None, || {
        match std::thread::spawn(walk).join().unwrap() {
            Ok(()) => 0,
            Err(e) => {
                println!("FAIL {e}");
                1
            }
        }
    })
}
