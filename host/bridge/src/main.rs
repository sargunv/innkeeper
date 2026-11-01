//! `innkeeper-bridge`. The launcher starts it for the profile in
//! `INNKEEPER_PROFILE`, and it takes no arguments.
//!
//! The bridge listens on `<run>/bridge/bridge.sock`. The launcher binds
//! `<run>/bridge` into the Android session. The bridge keeps MPRIS artwork in
//! `<run>/art` and writes the system app's activity reports to
//! `<run>/lifecycle` for the launcher. Document grants persist in
//! `<state>/documents.grants`. `<run>` and `<state>` are the profile's
//! [`innkeeper_core::paths`] dirs.
//!
//! - `INNKEEPER_TEST=1` marks a test session. It is isolated from the
//!   desktop's clipboard, portals, OpenURI, notifications, MPRIS, AT-SPI and
//!   location, and apps installed inside Android get no desktop entries.
//! - `INNKEEPER_AUDIO_SINK`, `INNKEEPER_AUDIO_SOURCE` name the PipeWire sink
//!   and capture source. The default is the desktop's.
//! - `INNKEEPER_BRIDGE_DEBUG=1` turns on debug logging and the frame and
//!   input trace ([`innkeeper_bridge::log`]).

use std::sync::Arc;

use anyhow::Result;
use std::sync::atomic::AtomicUsize;

use innkeeper_bridge::audio::{AudioService, Backend, Limits};
use innkeeper_bridge::display::{DisplayService, Limits as DisplayLimits};
use innkeeper_bridge::input::{Hub, InputService};
use innkeeper_bridge::ipc::proto::service;
use innkeeper_bridge::ipc::server::{self, Config, Registry};
use innkeeper_bridge::ipc::transport;
use innkeeper_bridge::system::{Shared, SystemService};

fn main() -> Result<()> {
    if std::env::args().len() > 1 {
        anyhow::bail!("innkeeper-bridge takes no arguments (see the source header)");
    }
    let env = |v: &str| std::env::var(v).ok().filter(|v| !v.is_empty());
    let desktop = env("INNKEEPER_TEST").as_deref() != Some("1");
    let paths = innkeeper_core::paths::Prefix::locate()?.session_paths()?;
    let (run_dir, state_dir, data_dir, profile) =
        (paths.run, paths.state, paths.data, paths.profile);
    let socket = run_dir.join("bridge/bridge.sock");
    // A report from before a restart is stale. The system app sends a new
    // one.
    let _ = std::fs::remove_file(run_dir.join("lifecycle"));
    let backend = Backend::Pulse {
        sink: env("INNKEEPER_AUDIO_SINK"),
        source: env("INNKEEPER_AUDIO_SOURCE"),
    };
    innkeeper_bridge::info!("audio: {backend:?}");
    let mut services = Registry::default();
    let streams = Arc::new(AtomicUsize::new(0));
    // The composer's displays in host windows need $WAYLAND_DISPLAY. The
    // system app's side covers window requests, the seat and the IME.
    let hub = Hub::new();
    *hub.evdev.lock().unwrap() = Some(innkeeper_bridge::input::evdev::Evdev::new(
        run_dir.join("bridge/evdev.sock"),
    )?);
    innkeeper_bridge::input::gamepad::start(run_dir.join("bridge/evdev.sock"));
    // Shared keeps its files in `<run>`, outside the socket directory, which
    // is shared with Android.
    let shared = Shared::new(&run_dir, desktop, hub.clone(), &profile);
    // The system app names the playing app. Each audio connection applies
    // that name to the playback streams it has open and the next one it opens.
    let audio_shared = shared.clone();
    services.register(service::AUDIO, "audio", move |peer, _events| {
        let opened = Arc::default();
        let app = audio_shared.register_playback(&opened);
        Ok(Box::new(AudioService::with_playback(
            backend.clone(),
            Limits::default(),
            streams.clone(),
            peer,
            app,
            opened,
        )))
    });
    let display_shared = shared.clone();
    let display_hub = hub.clone();
    let session_hub = hub.clone();
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
    let for_preferences = shared.clone();
    let for_signals = shared.clone();
    services.register(service::SYSTEM, "system", move |peer, events| {
        Ok(Box::new(SystemService::new(shared.clone(), peer, events)))
    });
    // On SIGTERM (session stop) or SIGINT, close our desktop notifications
    // before exiting. They would otherwise outlive the session.
    let mut signals = signal_hook::iterator::Signals::new([
        signal_hook::consts::SIGTERM,
        signal_hook::consts::SIGINT,
    ])?;
    std::thread::Builder::new()
        .name("signals".into())
        .spawn(move || {
            if let Some(sig) = signals.forever().next() {
                innkeeper_bridge::info!("signal {sig}: shutting down");
                for_signals.shutdown();
                std::process::exit(0);
            }
        })?;
    // Android apps' accessibility trees are published on AT-SPI while the
    // desktop's assistive technologies are on. A test session does not
    // publish them on the user's bus.
    let a11y = innkeeper_bridge::a11y::Shared::new(desktop);
    let focus = a11y.clone();
    a11y_hub.watch_focus(Box::new(move |w| focus.keyboard_focus(w)));
    services.register(service::A11Y, "a11y", move |peer, events| {
        Ok(Box::new(innkeeper_bridge::a11y::A11yService::new(
            a11y.clone(),
            peer,
            events,
        )))
    });
    // The host's battery, power saver, network and location. Location asks
    // the user, so a test session on the real desktop gets none.
    let host_state = innkeeper_bridge::hoststate::HostState::new(desktop);
    services.register(service::HOSTSTATE, "hoststate", move |peer, events| {
        Ok(Box::new(
            innkeeper_bridge::hoststate::HostStateService::new(host_state.clone(), peer, events),
        ))
    });
    let store = innkeeper_bridge::documents::Store::new(Some(state_dir.join("documents.grants")));
    let session_store = store.clone();
    services.register(service::DOCUMENTS, "documents", move |peer, events| {
        Ok(Box::new(
            innkeeper_bridge::documents::DocumentsService::new(
                store.clone(),
                peer,
                events,
                desktop,
            ),
        ))
    });
    // Apps installed inside the guest get desktop entries, except in a test
    // session.
    let session_api = innkeeper_bridge::session::Relay::default();
    let system = innkeeper_core::profiles::own()
        .map(|s| s.system)
        .unwrap_or_default();
    let apps = Arc::new(innkeeper_bridge::apps::Config::from_env(
        desktop,
        profile.clone(),
        system,
        data_dir,
    ));
    let _session_bus =
        match session_api.serve(for_preferences, session_store, session_hub, &profile) {
            Ok(bus) => Some(bus),
            Err(e) => {
                innkeeper_bridge::warn!("session D-Bus: {e:#}");
                None
            }
        };
    innkeeper_bridge::info!("apps: mirroring {}", if desktop { "on" } else { "off" });
    services.register(service::APPS, "apps", move |peer, events| {
        Ok(Box::new(
            innkeeper_bridge::apps::AppsService::new(apps.clone(), peer)
                .with_session(session_api.clone(), events),
        ))
    });
    let cfg = Config {
        max_clients: 32,
        services,
    };
    let listener = transport::listen(&socket)?;
    innkeeper_bridge::info!(
        "listening on {} (services {:?})",
        socket.display(),
        cfg.services.names()
    );
    server::serve(listener, Arc::new(cfg))
}
