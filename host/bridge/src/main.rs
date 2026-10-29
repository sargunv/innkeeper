//! `linuxapk-bridge --run-dir DIR --state-dir DIR [--allow-uid UID]...
//!                  [--system-app-dir DIR] [--mirror-apps] [--test-session] [--sink NAME] [-v]`
//!
//! Listens on `<run-dir>/bridge/bridge.sock` for clients from the Android
//! session (the launcher binds `<run-dir>/bridge` into it), keeps MPRIS
//! artwork in `<run-dir>/art`, and writes the system app's activity reports
//! to `<run-dir>/lifecycle` for the launcher. Only the given host uids may connect (default:
//! our own uid). Audio plays to the desktop's default PipeWire sink, or
//! `--sink NAME`.
//! `--system-app-dir DIR` (the Linux.apk System app's data directory, as
//! seen from the host) admits the uid that owns DIR to the system, documents
//! and apps services, and to the input and a11y services, which nobody else
//! may use.
//! `--mirror-apps` turns on desktop entries for apps
//! installed inside Android (`$XDG_DATA_HOME`). The documents service's
//! grants (host files the user picked) persist in
//! `<state-dir>/documents.grants`.
//! `--test-session` (also `LINUXAPK_TEST=1`) isolates clipboard, portals,
//! OpenURI, notifications, MPRIS, AT-SPI and location from the desktop.
//! `-v` logs per-stream statistics.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use std::sync::atomic::AtomicUsize;

use linuxapk_bridge::audio::{AudioService, Backend, Limits};
use linuxapk_bridge::display::{DisplayService, Limits as DisplayLimits};
use linuxapk_bridge::input::{Hub, InputService};
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
    let mut test_session = std::env::var("LINUXAPK_TEST").is_ok_and(|v| v == "1");
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
            "--test-session" => test_session = true,
            "--system-app-dir" => {
                system_app_dir = Some(args.next().context("--system-app-dir needs a path")?.into())
            }
            "-v" | "--verbose" => linuxapk_bridge::log::set_verbose(true),
            _ => bail!("unknown argument {a:?} (see the source header for usage)"),
        }
    }
    let policy = linuxapk_bridge::system::Policy::for_session(test_session);
    let mirror_apps = mirror_apps && !test_session;
    let run_dir = run_dir.context("--run-dir is required")?;
    let state_dir = state_dir.context("--state-dir is required")?;
    let socket = run_dir.join("bridge/bridge.sock");
    // A report from before a restart is stale; the system app sends a new one.
    let _ = std::fs::remove_file(run_dir.join("lifecycle"));
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
    // The composer's displays in host windows (needs $WAYLAND_DISPLAY), and
    // the system app's side of them: window requests, the seat, the IME.
    let hub = Hub::new();
    let shared = Shared::with_input(&run_dir, policy, hub.clone());
    let display_shared = shared.clone();
    let display_hub = hub.clone();
    let a11y_hub = hub.clone();
    services.register(service::DISPLAY, "display", move |peer, events| {
        Ok(Box::new(DisplayService::new(
            DisplayLimits::default(),
            display_hub.clone(),
            peer,
            events,
            display_shared.clone(),
        )?))
    });
    services.register(service::INPUT, "input", move |peer, events| {
        Ok(Box::new(InputService::new(hub.clone(), peer, events)))
    });
    // Not in the socket directory, which is shared with Android.
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
    // Android apps' accessibility trees on AT-SPI, while the desktop's
    // assistive technologies are on (never in a test session on the user's
    // own bus).
    let a11y = linuxapk_bridge::a11y::Shared::new(policy.presence);
    let focus = a11y.clone();
    a11y_hub.watch_focus(Box::new(move |w| focus.keyboard_focus(w)));
    services.register(service::A11Y, "a11y", move |peer, events| {
        Ok(Box::new(linuxapk_bridge::a11y::A11yService::new(
            a11y.clone(),
            peer,
            events,
        )))
    });
    // The host's battery, power saver, network and location. Location asks
    // the user, so a test session on the real desktop gets none.
    let host_state = linuxapk_bridge::hoststate::HostState::new(policy.portals);
    services.register(service::HOSTSTATE, "hoststate", move |peer, events| {
        Ok(Box::new(linuxapk_bridge::hoststate::HostStateService::new(
            host_state.clone(),
            peer,
            events,
        )))
    });
    // Host files the user picks for Android.
    let store = linuxapk_bridge::documents::Store::new(Some(state_dir.join("documents.grants")));
    services.register(service::DOCUMENTS, "documents", move |peer, events| {
        Ok(Box::new(linuxapk_bridge::documents::DocumentsService::new(
            store.clone(),
            peer,
            events,
            policy,
        )))
    });
    // Apps installed inside Android get desktop entries; only with
    // --mirror-apps (the main session).
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
        for id in [
            service::SYSTEM,
            service::DOCUMENTS,
            service::APPS,
            service::INPUT,
            service::A11Y,
            service::HOSTSTATE,
        ] {
            service_uids.push((id, check.clone()));
        }
    }
    let cfg = Config {
        allow_uids: allow,
        service_uids,
        private_services: vec![service::INPUT, service::A11Y],
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
