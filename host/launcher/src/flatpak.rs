//! Flatpak mode (docs/session.md, "Flatpak mode"): the session runs in
//! Linux.apk's own sandbox, which allows no namespaces, mounts or systemd
//! units, but whose `/`, `/dev` and `/sys` are writable tmpfs dirs of its own.
//!
//! - The root is composed on disk: per partition, hardlinks to the (read-only)
//!   image cache plus copies of the layers above it, files read-only. It is
//!   composed once per image, stage and addons, and only the start-time layer
//!   goes on at each start.
//! - `/` gets symlinks to it, the binder nodes and socket dirs; the broker
//!   shows the Android paths behind the links (supervisor/broker/src/flat.rs).
//! - init is pivot-exec's child, and the session lives as long as the
//!   process that started it, which serves the control socket
//!   ([`crate::control`]).

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::thread::sleep;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use crate::conf::Config;
use crate::image::walk_files;
use crate::paths::Paths;
use crate::root::{self, LAYERED, PARTITIONS};
use crate::session::{self, append};
use crate::{control, init, sys};

/// Bumped when the composition below changes.
const COMPOSE_VERSION: &str = "1";

fn cp(args: &[&std::ffi::OsStr]) -> Result<()> {
    let st = Command::new("cp").args(args).status().context("run cp")?;
    if !st.success() {
        bail!("cp {args:?} failed");
    }
    Ok(())
}

/// A layer's files over the composed tree. `--remove-destination` unlinks a
/// hardlinked file before replacing it, so the image cache is never written.
fn over(layer: &Path, dst: &Path) -> Result<()> {
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

/// Read-only files, as on read-only mounts: ART refuses to load a dex file
/// its process could write.
fn files_read_only(dir: &Path) -> Result<()> {
    walk_files(dir, &mut |p, m| {
        if m & 0o222 != 0 {
            fs::set_permissions(p, fs::Permissions::from_mode(m & !0o222 & 0o7777))?;
        }
        Ok(())
    })
}

/// Deletes a tree whose dirs may be read-only.
pub fn remove_tree(dir: &Path) -> Result<()> {
    if dir.symlink_metadata().is_err() {
        return Ok(());
    }
    fn writable(d: &Path) {
        if let Ok(m) = d.symlink_metadata()
            && m.is_dir()
        {
            let _ = fs::set_permissions(d, fs::Permissions::from_mode((m.mode() & 0o7777) | 0o700));
            for e in fs::read_dir(d).into_iter().flatten().flatten() {
                writable(&e.path());
            }
        }
    }
    writable(dir);
    fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))
}

/// What the composed root is made of: the image, its derived layers, the
/// stage (its manifest has every file's hash) and the addons' files.
fn compose_key(cfg: &Config) -> Result<String> {
    let mut h = Sha256::new();
    h.update(COMPOSE_VERSION);
    h.update(cfg.image.img.as_os_str().as_encoded_bytes());
    h.update(cfg.image.apexdir.as_os_str().as_encoded_bytes());
    for l in ["common", cfg.uids.mode.name()] {
        h.update(fs::read(cfg.image.derived.join(format!("{l}.source"))).unwrap_or_default());
    }
    h.update(fs::read(cfg.paths.share.join("manifest.tsv"))?);
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
    let p = &cfg.paths;
    let addons = p.addon_layers();
    // Each partition's stack, bottom first (the reverse of host mode's
    // overlays); the start-time layer goes on at each start.
    for part in PARTITIONS {
        let src = root::image_partition(&cfg.image.img, part);
        if !src.is_dir() {
            continue;
        }
        let dst = r.join(part);
        cp(&["-al".as_ref(), src.as_os_str(), dst.as_os_str()])?;
        if LAYERED.contains(part) {
            let mut layers = vec![
                cfg.image.derived.join("common").join(part),
                cfg.uid_layer().join(part),
                p.android().join(part),
            ];
            layers.extend(addons.iter().map(|a| a.join(part)));
            for l in layers {
                over(&l, &dst)?;
            }
        }
    }
    let apex = r.join("apex");
    cp(&[
        "-al".as_ref(),
        cfg.image.apexdir.as_os_str(),
        apex.as_os_str(),
    ])?;
    over(&cfg.uid_layer().join("apex"), &apex)?;
    files_read_only(r)?;
    // The uid mode's layer sets exact modes that code verifies (clatd's
    // 06755), so it goes on again.
    over(&cfg.uid_layer().join("apex"), &apex)?;
    fs::write(&key_file, key)?;
    Ok(())
}

/// The start-time layer over the composed root, read-only too.
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
    let l = r.join("linuxapk");
    remove_tree(&l)?;
    fs::create_dir_all(&l)?;
    root::linuxapk_dir(cfg, &l)?;
    symlink("/apex", l.join("apex"))?;
    Ok(())
}

/// A symlink `link` -> `target`, replacing whatever is at `link`.
fn link(target: &Path, link: &Path) -> Result<()> {
    if let Ok(m) = link.symlink_metadata() {
        if m.is_dir() {
            fs::remove_dir(link).with_context(|| format!("replace {}", link.display()))?;
        } else {
            fs::remove_file(link)?;
        }
    }
    symlink(target, link).with_context(|| format!("link {}", link.display()))
}

/// The sandbox's `/` as Android's root.
fn sandbox_root(cfg: &Config) -> Result<()> {
    let r = &cfg.paths.root;
    let slash = Path::new("/");
    for p in PARTITIONS.iter().chain(&["apex", "linuxapk"]) {
        if r.join(p).exists() {
            link(&r.join(p), &slash.join(p))?;
        }
    }
    let data = &cfg.paths.data;
    fs::create_dir_all(data)?;
    link(data, Path::new("/data"))?;
    // vold binds /data/data onto /data/user/0, where apps' data dirs are (the
    // broker's lstat shows the symlink as that directory).
    fs::create_dir_all(data.join("user"))?;
    fs::create_dir_all(data.join("data"))?;
    if data.join("user/0").symlink_metadata().is_err() {
        symlink("/data/data", data.join("user/0"))?;
    }
    // The image's other top-level entries, where the sandbox has none (its
    // /etc and /bin stay).
    root::copy_top_level(&cfg.image.img, slash, false)?;
    // Android's /etc is /system/etc. The sandbox's /etc stays (the glibc side
    // needs it) and gains the entries it lacks, such as task_profiles.json:
    // dirs as symlinks, files as copies (libbase opens files O_NOFOLLOW).
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
    let dev = Path::new("/dev");
    root::binder_nodes(dev)?;
    let run = &cfg.paths.run;
    fs::set_permissions(run.join("bridge"), fs::Permissions::from_mode(0o700))?;
    fs::create_dir_all(dev.join("socket"))?;
    link(&run.join("bridge"), &dev.join("socket/linuxapk"))?;
    link(&run.join("kmsg"), &dev.join("kmsg"))?;
    // The bpf() emulator's pins (supervisor/pivot-exec/bpfemu.c).
    fs::create_dir_all("/sys/fs/bpf")?;
    fs::set_permissions("/sys/fs/bpf", fs::Permissions::from_mode(0o1777))?;
    // No cgroup tree in the sandbox: /sys/fs/cgroup is a plain directory, so
    // process groups are bookkeeping only and kills fall back to the process
    // group. netd attaches its (emulated) eBPF programs there.
    fs::create_dir_all("/sys/fs/cgroup/system")?;
    fs::create_dir_all("/sys/fs/cgroup/apps")?;
    Ok(())
}

/// Ends the session: kills everything it started and removes its socket and
/// pid files. When this process is the sandbox's main one (its parent is the
/// sandbox's pid 1), that is everything else in the sandbox: Android
/// processes orphaned by a dying init would otherwise keep it alive.
fn teardown(p: &Paths) {
    session::ENDING.store(true, std::sync::atomic::Ordering::SeqCst);
    sys::kill_descendants();
    if sys::ppid() == 1 {
        unsafe { libc::kill(-1, libc::SIGKILL) };
    }
    let _ = fs::remove_file(p.control());
    session::remove_pids(p);
}

/// Runs the session in this sandbox until init exits; with `follow`, shows
/// its log meanwhile. It ends with this process: on a signal, `stop`, or
/// init's exit.
pub fn run(cfg: &Config, follow: bool) -> Result<()> {
    let sigs = sys::block_signals(&[libc::SIGINT, libc::SIGTERM, libc::SIGHUP]);
    let p = cfg.paths.clone();
    thread::spawn(move || {
        let sig = sys::wait_signal(&sigs);
        teardown(&p);
        std::process::exit(128 + sig);
    });
    let r = run_session(cfg, follow);
    teardown(&cfg.paths);
    r
}

fn run_session(cfg: &Config, follow: bool) -> Result<()> {
    let p = &cfg.paths;
    compose(cfg).context("compose the root")?;
    apply_run_layer(cfg).context("add the start-time layer")?;
    sandbox_root(cfg).context("set up the sandbox's /")?;
    session::start_kmsg_reader(p)?;
    session::start_bridge(cfg)?;
    let log = append(&p.session_log())?;
    let root_map = format!("{}=/\n{}=/data", p.root.display(), p.data.display());
    let mut cmd = init::command(
        cfg,
        Path::new("/"),
        &[
            ("LINUXAPK_FLATPAK", "1".into()),
            ("LINUXAPK_ROOT_MAP", root_map),
        ],
    );
    // pivot-exec, init's subreaper, dies with this thread.
    unsafe {
        cmd.pre_exec(|| {
            sys::die_with_parent(libc::SIGKILL);
            Ok(())
        })
    };
    let mut pivot = cmd
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .spawn()
        .context("start pivot-exec")?;
    let outer = pivot.id() as i32;
    let mut init = None;
    for _ in 0..300 {
        init = sys::children(outer, &["init"]).first().copied();
        if init.is_some() || pivot.try_wait()?.is_some() {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    let Some(init) = init else {
        bail!("init did not start (see {})", p.session_log().display());
    };
    session::write_pids(p, outer, init)?;
    let paths = p.clone();
    control::serve(
        &p.control(),
        control::Served {
            status: format!(
                "running: host pid {init}\nuid-mode={}\n",
                cfg.uids.mode.name()
            ),
            stop: Box::new(move || teardown(&paths)),
        },
    )?;
    println!(
        "session started ({}, uid mode {}, in Flatpak): Android init = pid {init}; log: {}",
        cfg.image.name,
        cfg.uids.mode.name(),
        p.kmsg_log().display()
    );
    if follow {
        // A closed stdout ends the log, not the session.
        let _ = session::tail(&p.kmsg_log(), &|| sys::alive(init));
    }
    pivot.wait()?;
    Ok(())
}

/// Starts the session in a sandbox of its own, through the Flatpak portal's
/// Spawn (`flatpak-spawn` without `--host`: a new instance of this app with
/// its permissions, not an escape), so it outlives the caller's sandbox.
/// Returns once it is up.
pub fn spawn(cfg_paths: &crate::paths::Paths, exe: &Path, headless: bool) -> Result<()> {
    let p = cfg_paths;
    let mut c = Command::new("flatpak-spawn");
    for (k, v) in env::vars_os() {
        if k.to_string_lossy().starts_with("LINUXAPK_") {
            let mut a = OsString::from("--env=");
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
            // flatpak-spawn would wait for the session to end (and keep this
            // sandbox alive meanwhile); SIGKILL isn't forwarded to it.
            let _ = child.kill();
            let _ = child.wait();
            let pid = s
                .lines()
                .next()
                .and_then(|l| l.strip_prefix("running: host pid "))
                .unwrap_or("?");
            println!(
                "session started in a sandbox of its own: Android init = pid {pid} there; log: {}",
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
