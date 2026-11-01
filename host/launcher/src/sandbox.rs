//! The session (docs/session.md) runs in Innkeeper's Flatpak sandbox. The
//! sandbox allows no namespaces, mounts or systemd units, but its `/`, `/dev`
//! and `/sys` are writable tmpfs dirs private to it.
//!
//! - The root is composed on disk. Each partition holds hardlinks to the
//!   read-only image cache plus copies of the layers above it, and all files
//!   are read-only. The root is composed once per combination of image, stage
//!   and addons. Only the start-time layer is applied at each start.
//! - `/` gets symlinks to the root, the binder nodes and the socket dirs. The
//!   broker shows the Android paths behind the links
//!   (supervisor/broker/src/fs.rs).
//! - init is the broker's child. The session lives as long as the process
//!   that started it, which serves the control socket ([`crate::control`]).

pub use innkeeper_profile::remove_tree;
use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::conf::Config;
use crate::image::walk_files;
use crate::paths::Paths;
use crate::profile::System;
use crate::root::{self, LAYERED, PARTITIONS};
use crate::session::{self, append};
use crate::{control, init, lifecycle, sys};

/// Bumped when the composition below changes.
const COMPOSE_VERSION: &str = "1";

fn cp(args: &[&std::ffi::OsStr]) -> Result<()> {
    let st = Command::new("cp").args(args).status().context("run cp")?;
    if !st.success() {
        bail!("cp {args:?} failed");
    }
    Ok(())
}

/// Copies a layer's files over the composed tree. `--remove-destination`
/// unlinks a hardlinked file before replacing it, so the image cache is never
/// written.
pub(crate) fn over(layer: &Path, dst: &Path) -> Result<()> {
    if !layer.is_dir() {
        return Ok(());
    }
    let mut src = layer.as_os_str().to_owned();
    src.push("/.");
    let mut to = dst.as_os_str().to_owned();
    to.push("/");
    cp(&[
        "-a".as_ref(),
        "--reflink=auto".as_ref(),
        "--remove-destination".as_ref(),
        &src,
        &to,
    ])
}

/// Makes every file under `dir` read-only, as on read-only mounts. ART
/// refuses to load a dex file that its process could write.
pub(crate) fn files_read_only(dir: &Path) -> Result<()> {
    walk_files(dir, &mut |p, m| {
        if m & 0o222 != 0 {
            fs::set_permissions(p, fs::Permissions::from_mode(m & !0o222 & 0o7777))?;
        }
        Ok(())
    })
}

/// Hashes the inputs of the composed root. These are the image, its derived
/// layer, the stage, the display mode and the addons' files. The stage's
/// manifest has every file's hash.
fn compose_key(cfg: &Config) -> Result<String> {
    let mut h = Sha256::new();
    h.update(COMPOSE_VERSION);
    h.update(cfg.image.img.as_os_str().as_encoded_bytes());
    h.update(cfg.image.apexdir.as_os_str().as_encoded_bytes());
    h.update(fs::read(cfg.image.derived_source()).unwrap_or_default());
    h.update(fs::read(cfg.paths.share.join("manifest.tsv"))?);
    h.update(format!("{:?}", crate::phone::display(&cfg.paths.data)?));
    for a in cfg.paths.addon_layers() {
        h.update(a.as_os_str().as_encoded_bytes());
        let mut files = vec![];
        walk_files(&a, &mut |p, m| {
            let md = fs::metadata(p)?;
            files.push(format!(
                "{} {m:o} {} {}",
                p.display(),
                md.len(),
                md.mtime_nsec() + md.mtime() * 1_000_000_000
            ));
            Ok(())
        })?;
        files.sort();
        h.update(files.join("\n"));
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Returns a function that copies a tree of the image cache into the root dir
/// `root`. It uses hardlinks, or reflink copies when hardlinks can't cross
/// mounts. That happens when the dev driver grants the image cache and the
/// root separately. `probe` is a file in the cache.
pub(crate) fn tree_linker(
    probe: &Path,
    root: &Path,
) -> impl Fn(&Path, &Path) -> Result<()> + use<> {
    let test = root.join(".link");
    let tree: &'static [&'static str] = if fs::hard_link(probe, &test).is_ok() {
        &["-al"]
    } else {
        &["-a", "--reflink=auto"]
    };
    let _ = fs::remove_file(&test);
    move |src: &Path, dst: &Path| {
        let mut args: Vec<&std::ffi::OsStr> = tree.iter().map(|a| a.as_ref()).collect();
        args.extend([src.as_os_str(), dst.as_os_str()]);
        cp(&args)
    }
}

/// Composes the partitions and `/apex` under the root dir, unless the last
/// composition had the same inputs.
fn compose(cfg: &Config) -> Result<()> {
    let r = &cfg.paths.root;
    let key = compose_key(cfg)?;
    let key_file = r.join(".compose-key");
    if fs::read_to_string(&key_file).is_ok_and(|k| k == key) {
        return Ok(());
    }
    remove_tree(r)?;
    fs::create_dir_all(r)?;
    let link = tree_linker(&cfg.image.apexdir.join("apex-info-list.xml"), r);
    let p = &cfg.paths;
    let addons = p.addon_layers();
    // Build each partition's stack, bottom first. The start-time layer goes on
    // at each start.
    for part in PARTITIONS {
        let src = root::image_partition(&cfg.image.img, part);
        if !src.is_dir() {
            continue;
        }
        let dst = r.join(part);
        link(&src, &dst)?;
        if LAYERED.contains(part) {
            let mut layers = vec![cfg.image.derived.join(part), p.android().join(part)];
            layers.extend(addons.iter().map(|a| a.join(part)));
            for l in layers {
                over(&l, &dst)?;
            }
        }
    }
    if crate::phone::display(&cfg.paths.data)? != crate::profile::Display::Windowed {
        for keep in crate::phone::KEEPS {
            let (part, rest) = keep.split_once('/').unwrap_or((keep, ""));
            let src = root::image_partition(&cfg.image.img, part).join(rest);
            if src.exists() {
                remove_tree(&r.join(keep))?;
                link(&src, &r.join(keep))?;
            }
        }
    }
    let apex = r.join("apex");
    link(&cfg.image.apexdir, &apex)?;
    over(&cfg.image.derived.join("apex"), &apex)?;
    files_read_only(r)?;
    // Code verifies the exact modes of the derived APEX files (clatd's
    // 06755), so they are copied again after the files are made read-only.
    over(&cfg.image.derived.join("apex"), &apex)?;
    fs::write(&key_file, key)?;
    Ok(())
}

/// Applies the start-time layer over the composed root, makes its files
/// read-only and writes `/innkeeper`.
fn apply_run_layer(cfg: &Config) -> Result<()> {
    let r = &cfg.paths.root;
    let layer = cfg.run_layer();
    for part in LAYERED.iter().chain(&["apex"]) {
        over(&layer.join(part), &r.join(part))?;
    }
    walk_files(&layer, &mut |p, _| {
        let dst = r.join(p.strip_prefix(&layer)?);
        let m = fs::metadata(&dst)?.permissions().mode();
        fs::set_permissions(&dst, fs::Permissions::from_mode(m & !0o222 & 0o7777))?;
        Ok(())
    })?;
    let l = r.join("innkeeper");
    remove_tree(&l)?;
    fs::create_dir_all(&l)?;
    root::innkeeper_dir(cfg, &l)?;
    symlink("/apex", l.join("apex"))?;
    Ok(())
}

/// Creates a symlink `link` -> `target`, replacing whatever is at `link`.
pub(crate) fn link(target: &Path, link: &Path) -> Result<()> {
    if let Ok(m) = link.symlink_metadata() {
        if m.is_dir() {
            fs::remove_dir(link).with_context(|| format!("replace {}", link.display()))?;
        } else {
            fs::remove_file(link)?;
        }
    }
    symlink(target, link).with_context(|| format!("link {}", link.display()))
}

/// Sets up the sandbox's `/` as Android's root.
fn sandbox_root(cfg: &Config) -> Result<()> {
    let r = &cfg.paths.root;
    let slash = Path::new("/");
    for p in PARTITIONS.iter().chain(&["apex", "innkeeper"]) {
        if r.join(p).exists() {
            link(&r.join(p), &slash.join(p))?;
        }
    }
    let data = &cfg.paths.data;
    fs::create_dir_all(data)?;
    link(data, Path::new("/data"))?;
    // vold binds /data/data onto /data/user/0, where apps' data dirs are. A
    // symlink stands in here, and the broker's lstat shows it as a directory.
    fs::create_dir_all(data.join("user"))?;
    fs::create_dir_all(data.join("data"))?;
    if data.join("user/0").symlink_metadata().is_err() {
        symlink("/data/data", data.join("user/0"))?;
    }
    // Add the image's other top-level entries where the sandbox has none. The
    // sandbox's /etc and /bin stay.
    root::copy_top_level(&cfg.image.img, slash, false)?;
    // Shared storage works without FUSE, because vold can't mount
    // MediaProvider's FUSE filesystem or bind /mnt/user/0 over /storage. Apps
    // reach /data/media directly, so it is on disk and not in the sandbox's
    // tmpfs.
    link(Path::new("/data/media"), Path::new("/storage/emulated"))?;
    for base in ["/mnt/user/0", "/mnt/pass_through/0"] {
        fs::create_dir_all(base)?;
        link(Path::new("/data/media"), &Path::new(base).join("emulated"))?;
    }
    fs::create_dir_all("/storage/self")?;
    link(
        Path::new("/storage/emulated/0"),
        Path::new("/storage/self/primary"),
    )?;
    // Android's /etc is /system/etc. The sandbox's /etc stays because the
    // glibc side needs it, and it gains the entries it lacks, such as
    // task_profiles.json. Directories become symlinks and files become copies,
    // because libbase opens files with O_NOFOLLOW.
    for e in fs::read_dir(r.join("system/etc"))? {
        let e = e?;
        let to = Path::new("/etc").join(e.file_name());
        if to.symlink_metadata().is_ok() {
            continue;
        }
        let t = e.file_type()?;
        if t.is_dir() {
            symlink(Path::new("/system/etc").join(e.file_name()), &to)?;
        } else if t.is_symlink() {
            symlink(fs::read_link(e.path())?, &to)?;
        } else {
            root::copy_file(&e.path(), &to)?;
        }
    }
    root::binder_nodes(Path::new("/dev"))?;
    dev_sockets(cfg)?;
    // The bpf() emulator pins objects here (supervisor/broker/src/bpf.rs).
    fs::create_dir_all("/sys/fs/bpf")?;
    fs::set_permissions("/sys/fs/bpf", fs::Permissions::from_mode(0o1777))?;
    // The sandbox has no cgroup tree. /sys/fs/cgroup is a plain directory, so
    // process groups are bookkeeping only and kills fall back to the process
    // group. netd attaches its emulated eBPF programs there.
    fs::create_dir_all("/sys/fs/cgroup/system")?;
    fs::create_dir_all("/sys/fs/cgroup/apps")?;
    Ok(())
}

/// Links the guest's `/dev/socket/innkeeper` to the bridge's socket dir and
/// `/dev/kmsg` to the FIFO that feeds kmsg.log.
pub(crate) fn dev_sockets(cfg: &Config) -> Result<()> {
    let (dev, run) = (Path::new("/dev"), &cfg.paths.run);
    fs::set_permissions(run.join("bridge"), fs::Permissions::from_mode(0o700))?;
    fs::create_dir_all(dev.join("socket"))?;
    link(&run.join("bridge"), &dev.join("socket/innkeeper"))?;
    link(&run.join("kmsg"), &dev.join("kmsg"))
}

/// Ends the session by killing everything it started and removing its
/// control socket. When this process is the sandbox's main process, with the
/// sandbox's pid 1 as its parent, it kills everything else in the sandbox.
/// Otherwise Android processes orphaned by a dying init would keep the
/// sandbox alive.
fn teardown(p: &Paths) {
    session::ENDING.store(true, std::sync::atomic::Ordering::SeqCst);
    sys::kill_descendants();
    // The bridge is killed with Android, so its signal handler cannot reap
    // files kept alive for desktop readers. End their lifetime here.
    let cache = crate::paths::xdg_home("XDG_CACHE_HOME", ".cache")
        .join(crate::paths::per_profile("innkeeper", &p.profile))
        .join("handoff");
    let _ = fs::remove_dir_all(cache);
    if sys::ppid() == 1 {
        unsafe { libc::kill(-1, libc::SIGKILL) };
    }
    let _ = fs::remove_file(p.control());
}

/// Set to `Some(wakeup)` when the session stops itself for idleness, where
/// `wakeup` is the next alarm, if any.
type Idle = Arc<Mutex<Option<Option<u64>>>>;

/// Runs the session in this sandbox until init exits, showing its log if
/// `follow` is set. The session ends with this process, on a signal, on
/// `stop` or when init exits. When it stops for idleness with an alarm ahead,
/// this process stays as the waiter that starts it again ([`wait_to_wake`]).
pub fn run(
    cfg: &Config,
    follow: bool,
    image_lease: crate::prune::Lease,
    trace: &session::BootTrace,
) -> Result<()> {
    let sigs = sys::block_signals(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP]);
    let p = cfg.paths.clone();
    thread::spawn(move || {
        let sig = sys::wait_signal(&sigs);
        teardown(&p);
        std::process::exit(128 + sig);
    });
    let idle = Idle::default();
    let r = run_session(cfg, follow, &idle, trace);
    teardown(&cfg.paths);
    // The alarm waiter has no Android processes left. Its next start must
    // be able to prepare changed derived layers and acquire a fresh lease.
    drop(image_lease);
    let wakeup = *idle.lock().unwrap();
    match wakeup {
        Some(Some(at)) if r.is_ok() => wait_to_wake(cfg, at),
        _ => r,
    }
}

/// Waits in the sandbox until shortly before `at_ms` (ms since the epoch),
/// then starts the session again in a new sandbox, as `start` does. A new
/// sandbox is needed because this one's `/` holds the last boot's runtime
/// files. Meanwhile `status` reports the session as stopped, and `stop` or a
/// new `start` ends the wait.
fn wait_to_wake(cfg: &Config, at_ms: u64) -> Result<()> {
    let p = &cfg.paths;
    request_background(p);
    let at = at_ms.saturating_sub(lifecycle::wake_lead().as_millis() as u64);
    let sock = p.control();
    let gone = sock.clone();
    control::serve(
        &sock,
        control::Served {
            status: format!("stopped\nwakeup={at_ms}\n"),
            exec: None,
            stop: Box::new(move || {
                let _ = fs::remove_file(&gone);
                std::process::exit(0);
            }),
        },
    )?;
    session::log(
        p,
        &format!(
            "waiting to start again at {}",
            lifecycle::calendar(at / 1000)
        ),
    );
    // Wall-clock time is checked at least every 30 s, so after a host
    // suspend the session starts late instead of never.
    loop {
        let now = lifecycle::now_ms();
        if now >= at {
            break;
        }
        sleep(Duration::from_millis((at - now).min(30_000)));
    }
    // Remove the socket so that `start` doesn't stop this process first.
    let _ = fs::remove_file(&sock);
    let mut c = Command::new(env::current_exe()?);
    c.arg("start");
    if cfg.headless {
        c.arg("--headless");
    }
    let st = c.stdin(Stdio::null()).status().context("run start")?;
    if !st.success() {
        bail!("start the session again: {st}");
    }
    Ok(())
}

/// Asks the Background portal to let Innkeeper run with no window open, for
/// media, alarms and background work, so the desktop doesn't stop it. This
/// is best effort and doesn't wait for the answer. `gdbus` comes from the
/// Flatpak runtime.
pub fn request_background(p: &Paths) {
    if crate::preferences::Preferences::load().is_ok_and(|p| p.keep_running) {
        return;
    }
    let opts = "{'reason': <'Android apps keep playing media, run foreground services and ring \
                alarms with no window open'>, 'autostart': <false>}";
    let r = Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--dest=org.freedesktop.portal.Desktop",
            "--object-path=/org/freedesktop/portal/desktop",
            "--method=org.freedesktop.portal.Background.RequestBackground",
            "",
            opts,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Err(e) = r {
        session::log(p, &format!("Background portal: {e}"));
    }
}

fn run_session(cfg: &Config, follow: bool, idle: &Idle, trace: &session::BootTrace) -> Result<()> {
    let p = &cfg.paths;
    let root_map = match cfg.system {
        System::Android => {
            compose(cfg).context("compose the root")?;
            trace.mark(p, "root composed");
            apply_run_layer(cfg).context("add the start-time layer")?;
            trace.mark(p, "start-time layer applied");
            crate::image::begin_boot(&p.data, &cfg.image, &p.root.join("innkeeper"))?;
            trace.mark(p, "data version recorded");
            sandbox_root(cfg).context("set up the sandbox's /")?;
            // realpath (via /proc/self/fd) must keep the public volume path,
            // because MediaProvider treats canonical /data/media paths as
            // internal storage.
            format!(
                "{}=/\n{}=/data\n{}=/storage/emulated",
                p.root.display(),
                p.data.display(),
                p.data.join("media").display()
            )
        }
        System::Ohos => {
            crate::ohos::compose(cfg).context("compose the root")?;
            trace.mark(p, "root composed");
            crate::ohos::sandbox_root(cfg).context("set up the sandbox's /")?;
            format!("{}=/\n{}=/data", p.root.display(), p.data.display())
        }
    };
    trace.mark(p, "sandbox root ready");
    session::start_kmsg_reader(p)?;
    session::start_bridge(cfg)?;
    trace.mark(p, "bridge ready");
    let log = append(&p.session_log())?;
    let mut cmd = init::command(cfg, &[("INNKEEPER_ROOT_MAP", root_map)]);
    // The broker, init's parent and subreaper, dies with this thread.
    unsafe {
        cmd.pre_exec(|| {
            sys::die_with_parent(libc::SIGKILL);
            Ok(())
        })
    };
    let mut broker = cmd
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("start the broker")?;
    let outer = broker.id() as i32;
    trace.mark(p, "broker spawned");
    let mut init = None;
    for _ in 0..300 {
        init = sys::children(outer, &["init"]).first().copied();
        if init.is_some() || broker.try_wait()?.is_some() {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    let Some(init) = init else {
        bail!("init did not start (see {})", p.session_log().display());
    };
    trace.mark(p, "init found");
    if cfg.system == System::Android {
        crate::adb::announce_at_start(p);
    }
    let paths = p.clone();
    control::serve(
        &p.control(),
        control::Served {
            status: crate::Status { pid: Some(init) }.text(),
            exec: Some(control::Exec {
                lock: p.exec_lock(),
                system: cfg.system,
            }),
            stop: Box::new(move || teardown(&paths)),
        },
    )?;
    let (idle, paths) = (idle.clone(), p.clone());
    lifecycle::watch(cfg, move |wakeup| {
        *idle.lock().unwrap() = Some(wakeup);
        // Tear down as `stop` does, because killing the broker alone would
        // orphan init.
        teardown(&paths);
    });
    println!(
        "session started ({}, in Flatpak): {} init = pid {init}; log: {}",
        cfg.image.name,
        cfg.system.title(),
        p.kmsg_log().display()
    );
    if follow {
        // A closed stdout ends the log but leaves the session running.
        let _ = session::tail(&p.kmsg_log(), &|| sys::alive(init));
    }
    broker.wait()?;
    Ok(())
}

/// Starts the session in a new sandbox through the Flatpak portal's Spawn, so
/// it outlives the caller's sandbox. `flatpak-spawn` without `--host` starts
/// a new instance of this app with the same permissions and does not escape
/// the sandbox. Returns once the session is up.
pub fn spawn(p: &Paths, exe: &Path, headless: bool) -> Result<()> {
    let mut c = Command::new("flatpak-spawn");
    c.arg("/usr/bin/env");
    for (k, v) in env::vars_os() {
        let name = k.to_string_lossy();
        if name.starts_with("INNKEEPER_")
            || [
                // Selects the adb server that profiles with adb on are
                // announced to.
                "ANDROID_ADB_SERVER_PORT",
                "XDG_DATA_HOME",
                "XDG_CONFIG_HOME",
                "XDG_CACHE_HOME",
                "XDG_STATE_HOME",
            ]
            .contains(&name.as_ref())
        {
            // Flatpak sets XDG_*_HOME in the new sandbox and overrides any
            // --env, so env(1) sets them afterwards.
            let mut a = OsString::new();
            a.push(&k);
            a.push("=");
            a.push(&v);
            c.arg(a);
        }
    }
    c.arg(exe).arg("_serve");
    if headless {
        c.arg("--headless");
    }
    fs::create_dir_all(&p.state)?;
    let mut child = c
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(append(&p.state.join("spawn.log"))?)
        .spawn()
        .context("run flatpak-spawn")?;
    for _ in 0..600 {
        if let Some(s) = control::status(&p.control()) {
            // flatpak-spawn would otherwise wait for the session to end and
            // keep this sandbox alive meanwhile. SIGKILL isn't forwarded to
            // it.
            let _ = child.kill();
            let _ = child.wait();
            let pid = crate::Status::parse(&s)
                .pid
                .map_or("?".into(), |p| p.to_string());
            println!(
                "session started in a sandbox of its own: init = pid {pid} there; log: {}",
                p.kmsg_log().display()
            );
            let _ = std::io::stdout().flush();
            return Ok(());
        }
        if let Some(st) = child.try_wait()? {
            eprint!(
                "{}",
                fs::read_to_string(p.session_log()).unwrap_or_default()
            );
            bail!(
                "session failed to start ({st}; see {})",
                p.state.join("spawn.log").display()
            );
        }
        sleep(Duration::from_millis(100));
    }
    bail!("the session did not come up in 60 s")
}
