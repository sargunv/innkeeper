//! `linuxapk-bridge --socket PATH [--allow-uid UID]... [--sink NAME] [--null] [-v]`
//!
//! Listens on PATH for clients from the Android session. Only the given host
//! uids may connect (default: our own uid). Audio plays to the desktop's
//! default PipeWire sink, or `--sink NAME`; `--null` plays nowhere.
//! `-v` (or `LINUXAPK_BRIDGE_VERBOSE=1`) logs per-stream statistics.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use std::sync::atomic::AtomicUsize;

use linuxapk_bridge::audio::{AudioService, Backend, Limits};
use linuxapk_bridge::proto::service;
use linuxapk_bridge::server::{self, Config, Registry};
use linuxapk_bridge::transport;

fn main() -> Result<()> {
    let mut socket: Option<PathBuf> = None;
    let mut allow = Vec::new();
    let mut sink = None;
    let mut null = false;
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
    let cfg = Config {
        allow_uids: allow,
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
