//! `linuxapk-bridge --run-dir DIR --state-dir DIR [--allow-uid UID]...
//!                  [--system-app-dir DIR] [--mirror-apps] [--no-open-urls]
//!                  [--no-desktop-presence] [--sink NAME] [-v]`
//!
//! Listens on `<run-dir>/bridge/bridge.sock` for clients from the Android
//! session (the launcher binds `<run-dir>/bridge` into it), and keeps MPRIS
//! artwork in `<run-dir>/art`. Only the given host uids may connect (default:
//! our own uid). Audio plays to the desktop's default PipeWire sink, or
//! `--sink NAME`.
//! `--system-app-dir DIR` (the Linux.apk System app's data directory, as
//! seen from the host) admits the uid that owns DIR to the system, documents
//! and apps services. `--mirror-apps` turns on desktop entries for apps
//! installed inside Android (`$XDG_DATA_HOME`). The documents service's
//! grants (host files the user picked) persist in
//! `<state-dir>/documents.grants`.
//! `--no-open-urls` and `--no-desktop-presence` turn OpenURI, and
//! notifications and MPRIS, into log-only sinks (test sessions).
//! `-v` logs per-stream statistics.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use std::sync::atomic::AtomicUsize;

use linuxapk_bridge::audio::{AudioService, Backend, Limits};
use linuxapk_bridge::display::{DisplayService, Limits as DisplayLimits};
use linuxapk_bridge::ipc::proto::service;
use linuxapk_bridge::ipc::server::{self, Config, Registry, UidCheck};
use linuxapk_bridge::ipc::transport;
use linuxapk_bridge::system::{Shared, SystemService};

fn main() -> Result<()> {
    let mut run_dir: Option<PathBuf> = None;
    let mut state_dir: Option<PathBuf> = None;
    let mut allow = Vec::new();
    let mut sink = None;
    let mut system_app_dir: Option<PathBuf> = None;
    let mut mirror_apps = false;
    let mut policy = linuxapk_bridge::system::Policy::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--run-dir" => run_dir = Some(args.next().context("--run-dir needs a path")?.into()),
            "--state-dir" => {
                state_dir = Some(args.next().context("--state-dir needs a path")?.into())
            }
            "--allow-uid" => allow.push(
                args.next()
                    .context("--allow-uid needs a uid")?
                    .parse()
                    .context("bad uid")?,
            ),
            "--sink" => sink = Some(args.next().context("--sink needs a name")?),
            "--mirror-apps" => mirror_apps = true,
            "--no-open-urls" => policy.open_urls = false,
            "--no-desktop-presence" => policy.presence = false,
            "--system-app-dir" => {
                system_app_dir = Some(args.next().context("--system-app-dir needs a path")?.into())
            }
            "-v" | "--verbose" => linuxapk_bridge::log::set_verbose(true),
            _ => bail!("unknown argument {a:?} (see the source header for usage)"),
        }
    }
    let run_dir = run_dir.context("--run-dir is required")?;
    let state_dir = state_dir.context("--state-dir is required")?;
    let socket = run_dir.join("bridge/bridge.sock");
    if allow.is_empty() {
        allow.push(rustix::process::getuid().as_raw());
    }
    let backend = Backend::Pulse { sink };
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
    // Not in the socket directory, which is shared with Android.
    let shared = Shared::with_policy(run_dir.join("art"), policy);
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
    // Host files the user picks for Android (docs/bridge.md, "Documents
    // service").
    let store = linuxapk_bridge::documents::Store::new(Some(state_dir.join("documents.grants")));
    services.register(service::DOCUMENTS, "documents", move |peer, events| {
        Ok(Box::new(linuxapk_bridge::documents::DocumentsService::new(
            store.clone(),
            peer,
            events,
        )))
    });
    // Apps installed inside Android get desktop entries (docs/bridge.md,
    // "Apps service"); only with --mirror-apps (the default session).
    let apps = Arc::new(linuxapk_bridge::apps::Config::from_env(mirror_apps));
    linuxapk_bridge::info!("apps: mirroring {}", if mirror_apps { "on" } else { "off" });
    services.register(service::APPS, "apps", move |peer, _events| {
        Ok(Box::new(linuxapk_bridge::apps::AppsService::new(
            apps.clone(),
            peer,
        )))
    });
    let mut service_uids: Vec<(u32, UidCheck)> = Vec::new();
    if let Some(dir) = system_app_dir {
        // The system app's uid is assigned at install time; the owner of its
        // data directory (created by installd) is the source of truth.
        let check: UidCheck = Arc::new(move |uid| {
            use std::os::unix::fs::MetadataExt;
            std::fs::metadata(&dir).is_ok_and(|m| m.uid() == uid)
        });
        for id in [service::SYSTEM, service::DOCUMENTS, service::APPS] {
            service_uids.push((id, check.clone()));
        }
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
