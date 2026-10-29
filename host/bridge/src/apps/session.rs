//! Desktop session API relayed over the system app's existing apps channel.
use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use linuxapk_core::app_settings::{self, Command, Response};

use crate::ipc::{
    blob,
    server::Events,
    wire::{Reader, Writer},
};

pub const COMMAND: u16 = 0x4501;
pub const RESULT: u16 = 0x0504;
type Answer = Result<String, String>;

#[derive(Default)]
struct State {
    next: u64,
    connection: Option<(u64, Events)>,
    pending: HashMap<u64, mpsc::SyncSender<Answer>>,
}

#[derive(Clone, Default)]
pub struct Relay(Arc<Mutex<State>>);

impl Relay {
    pub fn attach(&self, events: Events) -> u64 {
        let mut state = self.0.lock().unwrap();
        if let Some((_, old)) = state.connection.take() {
            old.shutdown();
        }
        state.pending.clear();
        state.next += 1;
        let id = state.next;
        state.connection = Some((id, events));
        id
    }

    pub fn detach(&self, id: u64) {
        let mut state = self.0.lock().unwrap();
        if state.connection.as_ref().is_some_and(|(k, _)| *k == id) {
            state.connection = None;
            state.pending.clear();
        }
    }

    fn call(&self, command: Command) -> Answer {
        let bytes = command.encode().map_err(|e| e.to_string())?;
        let fd = blob::create("app-command", &bytes).map_err(|e| e.to_string())?;
        let (tx, rx) = mpsc::sync_channel(1);
        let id;
        {
            let mut state = self.0.lock().unwrap();
            if state.pending.len() >= 8 {
                return Err("App operations are busy; try again".into());
            }
            state.next += 1;
            id = state.next;
            let events = &state
                .connection
                .as_ref()
                .ok_or("System app is not connected")?
                .1;
            let mut body = Writer::default();
            body.u64(id).u32(bytes.len() as u32);
            events
                .try_send(COMMAND, &body.0, &[fd.as_fd()])
                .map_err(|e| e.to_string())?;
            state.pending.insert(id, tx);
        }
        let result = rx.recv_timeout(Duration::from_secs(20));
        self.0.lock().unwrap().pending.remove(&id);
        result.map_err(|_| {
            "System app disconnected or operation timed out; refresh before retrying".to_string()
        })?
    }

    pub fn complete(&self, connection: u64, body: &[u8], fds: Vec<OwnedFd>) -> Result<(), i32> {
        let mut r = Reader::new(body);
        let id = r.u64().map_err(|e| e.status())?;
        let len = r.u32().map_err(|e| e.status())?;
        r.end().map_err(|e| e.status())?;
        if fds.len() != 1 {
            return Err(-22);
        }
        let bytes = blob::read(&fds[0], len, app_settings::MAX_JSON)?;
        Response::decode(&bytes).map_err(|_| -22)?;
        let json = String::from_utf8(bytes).map_err(|_| -22)?;
        let mut state = self.0.lock().unwrap();
        if state
            .connection
            .as_ref()
            .is_some_and(|(k, _)| *k == connection)
            && let Some(tx) = state.pending.remove(&id)
        {
            let _ = tx.send(Ok(json));
        }
        Ok(())
    }

    fn rescan(&self) {
        if let Some((_, events)) = &self.0.lock().unwrap().connection {
            let _ = events.try_send(0x4502, &[], &[]);
        }
    }

    pub fn serve(
        &self,
        shared: Arc<crate::system::Shared>,
        apps: Arc<super::Config>,
    ) -> anyhow::Result<zbus::blocking::Connection> {
        Ok(zbus::blocking::connection::Builder::session()?
            .name(app_settings::bus_name(&linuxapk_core::paths::instance()?))?
            .serve_at(
                app_settings::PATH,
                SessionApi {
                    relay: self.clone(),
                    shared,
                    apps,
                    preferences: Mutex::new(()),
                },
            )?
            .build()?)
    }
}

struct SessionApi {
    relay: Relay,
    shared: Arc<crate::system::Shared>,
    apps: Arc<super::Config>,
    preferences: Mutex<()>,
}

#[zbus::interface(name = "dev.linuxapk.Session1")]
impl SessionApi {
    fn get_preferences(&self) -> zbus::fdo::Result<String> {
        let _guard = self.preferences.lock().unwrap();
        linuxapk_core::preferences::Preferences::load()
            .and_then(|p| linuxapk_core::preferences::encode(&p))
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))
    }

    fn set_preferences(&self, json: &str) -> zbus::fdo::Result<()> {
        let _guard = self.preferences.lock().unwrap();
        let prefs = linuxapk_core::preferences::decode(json)
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let old = linuxapk_core::preferences::Preferences::load()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        prefs
            .save()
            .map_err(|e| zbus::fdo::Error::Failed(e.to_string()))?;
        self.apps
            .mirror_apps
            .store(prefs.mirror_apps, std::sync::atomic::Ordering::Relaxed);
        self.shared.refresh_preferences();
        if !old.mirror_apps && prefs.mirror_apps {
            self.relay.rescan();
        }
        Ok(())
    }

    /// Version 1: JSON AppInfo response; error text on failed Android operations.
    fn app(
        &self,
        package: &str,
        action: &str,
        permission: &str,
        enabled: bool,
    ) -> zbus::fdo::Result<String> {
        let command = Command {
            package: package.into(),
            action: action.into(),
            permission: permission.into(),
            enabled,
        };
        command
            .validate()
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        self.relay.call(command).map_err(zbus::fdo::Error::Failed)
    }
}
