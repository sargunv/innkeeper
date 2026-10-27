//! `linuxapk-bridge --socket PATH [--allow-uid UID]... [--system-app-dir DIR]
//!                  [--sink NAME] [--null] [-v]`
//!
//! Listens on PATH for clients from the Android session. Only the given host
//! uids may connect (default: our own uid). Audio plays to the desktop's
//! default PipeWire sink, or `--sink NAME`; `--null` plays nowhere.
//! `--system-app-dir DIR` (the Linux.apk System app's data directory, as
//! seen from the host) admits the uid that owns DIR to the system service.
//! `-v` (or `LINUXAPK_BRIDGE_VERBOSE=1`) logs per-stream statistics.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use std::sync::atomic::AtomicUsize;

use linuxapk_bridge::audio::{AudioService, Backend, Limits};
use linuxapk_bridge::display::{DisplayService, Limits as DisplayLimits};
use linuxapk_bridge::proto::service;
use linuxapk_bridge::server::{self, Config, Registry, UidCheck};
use linuxapk_bridge::services::system::{Shared, SystemService};
use linuxapk_bridge::transport;

fn main() -> Result<()> {
    let mut socket: Option<PathBuf> = None;
    let mut allow = Vec::new();
    let mut sink = None;
    let mut null = false;
    let mut system_app_dir: Option<PathBuf> = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--socket" => socket = Some(args.next().context("--socket needs a path")?.into()),
            "--allow-uid" => allow.push(
                args.next()
                    .context("--allow-uid needs a uid")?
                    .parse()
                    .context("bad uid")?,
            ),
            "--sink" => sink = Some(args.next().context("--sink needs a name")?),
            "--null" => null = true,
            "--system-app-dir" => {
                system_app_dir = Some(args.next().context("--system-app-dir needs a path")?.into())
            }
            "-v" | "--verbose" => linuxapk_bridge::log::set_verbose(true),
            _ => bail!("unknown argument {a:?} (see the source header for usage)"),
        }
    }
    if std::env::var_os("LINUXAPK_BRIDGE_VERBOSE").is_some_and(|v| v == "1") {
        linuxapk_bridge::log::set_verbose(true);
    }
    let socket = socket.context("--socket is required")?;
    if allow.is_empty() {
        allow.push(rustix::process::getuid().as_raw());
    }
    let backend = if null {
        Backend::Null
    } else {
        Backend::Pulse { sink }
    };
    linuxapk_bridge::info!("audio: {backend:?}");
    let mut services = Registry::default();
    let streams = Arc::new(AtomicUsize::new(0));
    services.register(service::AUDIO, "audio", move |peer, _events| {
        Ok(Box::new(AudioService::new(
            backend.clone(),
            Limits::default(),
            streams.clone(),
            peer,
        )))
    });
    // Windows for the composer (docs/windowing.md); needs $WAYLAND_DISPLAY.
    services.register(service::DISPLAY, "display", |peer, events| {
        Ok(Box::new(DisplayService::new(
            DisplayLimits::default(),
            peer,
            events,
        )?))
    });
    // Artwork for MPRIS goes next to the socket directory, not inside it (the
    // socket directory is shared with Android).
    let art_dir = std::path::absolute(&socket)?
        .parent()
        .and_then(|d| d.parent())
        .map(|d| d.join("art"))
        .unwrap_or_else(|| PathBuf::from("art"));
    let shared = Shared::new(art_dir);
    let for_signals = shared.clone();
    services.register(service::SYSTEM, "system", move |peer, events| {
        Ok(Box::new(SystemService::new(shared.clone(), peer, events)))
    });
    // On SIGTERM (session stop) or SIGINT, close our desktop notifications
    // before exiting; they would otherwise outlive the session.
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ])?;
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            if let Some(sig) = signals.forever().next() {
                linuxapk_bridge::info!("signal {sig}: shutting down");
                for_signals.shutdown();
                std::process::exit(0);
            }
        })?;
    let mut service_uids: Vec<(u32, UidCheck)> = Vec::new();
    if let Some(dir) = system_app_dir {
        // The system app's uid is assigned at install time; the owner of its
        // data directory (created by installd) is the source of truth.
        service_uids.push((
            service::SYSTEM,
            Arc::new(move |uid| {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(&dir).is_ok_and(|m| m.uid() == uid)
            }),
        ));
    }
    let cfg = Config {
        allow_uids: allow,
        service_uids,
        max_clients: 32,
        services,
    };
    let listener = transport::listen(&socket)?;
    linuxapk_bridge::info!(
        "listening on {} (uids {:?}, services {:?})",
        socket.display(),
        cfg.allow_uids,
        cfg.services.names()
    );
    server::serve(listener, Arc::new(cfg))
}
