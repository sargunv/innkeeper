//! The desktop session API, version 1. The manager and the CLI manage apps
//! through it and do not run commands inside Android. Starting a stopped
//! session still goes through the launcher.
//!
//! The bridge owns `dev.sargunv.Innkeeper.Session.p_<profile>` on the session
//! bus, inside the Flatpak app-id namespace. Hyphens in the profile name
//! become underscores. The object is `/dev/sargunv/Innkeeper/Session` and the
//! interface is `dev.sargunv.Innkeeper.Session1`.
//!
//! - `State() → (b booted, as open_packages)` returns a snapshot and doesn't
//!   start Android. `booted` means the system app saw `sys.boot_completed=1`
//!   on the current apps connection, which it reports with `BOOT_COMPLETED`
//!   on each connect. A connected channel alone doesn't count as booted.
//!   `open_packages` are the packages with mapped windows. `Changed()`
//!   invalidates the snapshot on boot, on apps-channel connect or disconnect,
//!   and on window map or unmap. Clients also watch name ownership, subscribe
//!   before reading, and read again on each signal.
//! - `App(s package, s action, s permission, b enabled) → s response` runs one
//!   of the actions `info`, `clear-cache`, `clear-data`, `force-stop`,
//!   `permission`, `escape-back` and `background`. Only `permission` takes a
//!   permission name, and only the last three take `enabled=true`. Bad
//!   arguments fail with `InvalidArgs`. A disconnect, a full queue or the
//!   40 s timeout fails with `Failed`. After a timeout Android may still have
//!   performed the action, so the client should refresh. The response is JSON
//!   of at most 64 KiB, either `{"error":"", "info":{…}}` or
//!   `{"error":"message", "info":null}`. `info` has `app_bytes`, `data_bytes`
//!   (which includes the cache), `cache_bytes`, `escape_back`, `background`
//!   and `permissions`. Each permission has `name`, `granted` and `editable`,
//!   and only requested dangerous permissions are listed. Only installed user
//!   apps can be changed, and Android enforces its grant rules.
//! - `Launch(s package, s component, s uri, at grants, s token, b wait,
//!   b new_window, u width, u height) → s reply` opens `package` in its own
//!   window, or focuses its existing window. It starts the launcher activity
//!   or `component`, and passes `uri` or `Import` grants to view. Empty
//!   strings and zeros mean none. `token` is the desktop's activation token,
//!   `new_window` asks for a separate task, and `width` and `height` size the
//!   window in dp. With `wait` the call answers once a new window shows its
//!   first frame, waiting at most 25 s and appending `shown=no` if it didn't.
//!   The reply is the window manager's line,
//!   `launched display=N package=P size=WxH ...` or `focused ...`.
//! - `Windows() → as` returns one `display=N package=P ...` line per window.
//!   `Close(i display, s package) → s` closes window `display`, or
//!   `package`'s window when `display` is negative. Failures are `Failed`
//!   with the system app's message.
//! - `ReloadSettings()` applies the profile's settings file, which the caller
//!   has just written. Settings are broadcast again. An unreadable or invalid
//!   file fails with `Failed`.
//! - `Import(h fd, s name) → t grant` makes a read-only documents grant for a
//!   regular file, shown to Android as `name` (no `/`), so a launch can pass
//!   it to an app. Only the fd reaches the bridge, not a host path.
//!
//! `App`, `Launch`, `Windows` and `Close` are relayed on the apps channel.
//! [`COMMAND`] events carry `u64 request_id, u32 length` and a sealed JSON
//! memfd. App commands are `{"package","action","permission","enabled"}`.
//! Window commands are `{"action":"launch"|"windows"|"close", …}` with the
//! call's arguments, and grants written as 16 hex digits. The system app
//! answers with a [`RESULT`] request in the same envelope, holding the app
//! response or `{"error":"","reply":"…"}`. Trailing bytes and extra fds are
//! refused, and both payloads are at most 64 KiB. Up to eight requests can
//! wait, each for at most 40 s, and a reconnect cancels them.
//! `wm/WindowCommands.kt` documents the window operations. Escape-as-Back is
//! stored per package in the system app's preferences, off by default, and
//! applies to existing windows at the next key press.
use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use innkeeper_core::app_settings::{self, Command, Response};

use crate::ipc::{
    blob,
    server::Events,
    wire::{Reader, Writer},
};

pub const COMMAND: u16 = 0x4501;
pub const RESULT: u16 = 0x0504;
pub const BOOT_COMPLETED: u16 = 0x0505;
type Answer = Result<String, String>;

#[derive(Default)]
struct State {
    next: u64,
    connection: Option<(u64, Events)>,
    booted: bool,
    pending: HashMap<u64, mpsc::SyncSender<Answer>>,
    bus: Option<zbus::blocking::Connection>,
}

#[derive(Clone, Default)]
pub struct Relay(Arc<Mutex<State>>);

impl Relay {
    fn booted(&self) -> bool {
        self.0.lock().unwrap().booted
    }

    pub fn attach(&self, events: Events) -> u64 {
        let mut state = self.0.lock().unwrap();
        if let Some((_, old)) = state.connection.take() {
            old.shutdown();
        }
        state.pending.clear();
        state.next += 1;
        let id = state.next;
        state.connection = Some((id, events));
        state.booted = false;
        drop(state);
        self.changed();
        id
    }

    pub fn detach(&self, id: u64) {
        let mut state = self.0.lock().unwrap();
        if state.connection.as_ref().is_some_and(|(k, _)| *k == id) {
            state.connection = None;
            state.booted = false;
            state.pending.clear();
        }
        drop(state);
        self.changed();
    }

    pub fn boot_completed(&self, connection: u64) {
        let mut state = self.0.lock().unwrap();
        if state.booted
            || !state
                .connection
                .as_ref()
                .is_some_and(|(id, _)| *id == connection)
        {
            return;
        }
        state.booted = true;
        drop(state);
        self.changed();
    }

    fn changed(&self) {
        let bus = self.0.lock().unwrap().bus.clone();
        if let Some(bus) = bus {
            let _ = bus.emit_signal(
                None::<&str>,
                app_settings::PATH,
                app_settings::INTERFACE,
                "Changed",
                &(),
            );
        }
    }

    /// Relays JSON `bytes` to the system app and returns its JSON answer.
    fn call(&self, bytes: &[u8]) -> Answer {
        let fd = blob::create("app-command", bytes).map_err(|e| e.to_string())?;
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
        // A launch may wait 25 s for its window's first frame.
        let result = rx.recv_timeout(Duration::from_secs(40));
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
        serde_json::from_slice::<serde_json::Map<String, serde_json::Value>>(&bytes)
            .map_err(|_| -22)?;
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

    pub fn serve(
        &self,
        shared: Arc<crate::system::Shared>,
        documents: Arc<crate::documents::Store>,
        input: Arc<crate::input::Hub>,
        profile: &innkeeper_profile::profile::ProfileName,
    ) -> anyhow::Result<zbus::blocking::Connection> {
        let relay = self.clone();
        input.watch_windows(move || relay.changed());
        let bus = zbus::blocking::connection::Builder::session()?
            .name(app_settings::bus_name(profile))?
            .serve_at(
                app_settings::PATH,
                SessionApi {
                    relay: self.clone(),
                    input,
                    shared,
                    documents,
                },
            )?
            .build()?;
        self.0.lock().unwrap().bus = Some(bus.clone());
        Ok(bus)
    }
}

struct SessionApi {
    input: Arc<crate::input::Hub>,
    relay: Relay,
    shared: Arc<crate::system::Shared>,
    documents: Arc<crate::documents::Store>,
}

impl SessionApi {
    fn window(&self, w: app_settings::WindowCommand) -> zbus::fdo::Result<String> {
        let bytes = w
            .encode()
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let json = self.relay.call(&bytes).map_err(zbus::fdo::Error::Failed)?;
        app_settings::WindowResponse::decode(json.as_bytes())
            .map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))
    }
}

#[zbus::interface(name = "dev.sargunv.Innkeeper.Session1")]
impl SessionApi {
    /// `booted` is true only after the system app observes
    /// sys.boot_completed=1.
    fn state(&self) -> (bool, Vec<String>) {
        (self.relay.booted(), self.input.open_packages())
    }

    #[zbus(signal)]
    async fn changed(emitter: &zbus::object_server::SignalEmitter<'_>) -> zbus::Result<()>;

    /// Applies the profile's settings file, which the caller has just
    /// written.
    fn reload_settings(&self) -> zbus::fdo::Result<()> {
        innkeeper_core::profiles::own().map_err(|e| zbus::fdo::Error::Failed(format!("{e:#}")))?;
        self.shared.refresh_preferences();
        Ok(())
    }

    /// Makes a read-only documents grant for the regular file `fd`, named
    /// `name`, for a launch to pass to an app.
    fn import(&self, fd: zbus::zvariant::OwnedFd, name: &str) -> zbus::fdo::Result<u64> {
        if !crate::documents::proto::valid_name(name) {
            return Err(zbus::fdo::Error::InvalidArgs("invalid file name".into()));
        }
        self.documents
            .import_file(fd.into(), name.into())
            .map_err(|e| {
                zbus::fdo::Error::InvalidArgs(std::io::Error::from_raw_os_error(-e).to_string())
            })
    }

    /// Runs an app command and returns the JSON response, which carries error
    /// text when the Android operation failed.
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
        let bytes = command
            .encode()
            .map_err(|e| zbus::fdo::Error::InvalidArgs(e.to_string()))?;
        let json = self.relay.call(&bytes).map_err(zbus::fdo::Error::Failed)?;
        Response::decode(json.as_bytes())
            .map_err(|e| zbus::fdo::Error::Failed(format!("bad answer: {e}")))?;
        Ok(json)
    }

    /// Opens `package` in its own window, or focuses its existing window. It
    /// starts the launcher activity or `component`, and passes `uri` or
    /// document `grants` to view. With `wait`, it answers once a new window
    /// shows its first frame. Returns the window manager's reply,
    /// `launched display=N ...` or `focused ...`.
    #[allow(clippy::too_many_arguments)]
    fn launch(
        &self,
        package: &str,
        component: &str,
        uri: &str,
        grants: Vec<u64>,
        token: &str,
        wait: bool,
        new_window: bool,
        width: u32,
        height: u32,
    ) -> zbus::fdo::Result<String> {
        let w = app_settings::WindowCommand::Launch {
            package: package.into(),
            component: component.into(),
            uri: uri.into(),
            grants: grants.iter().map(|g| format!("{g:016x}")).collect(),
            token: token.into(),
            wait,
            new_window,
            width,
            height,
        };
        self.window(w)
    }

    /// Lists the windows, one `display=N package=P ...` line each.
    fn windows(&self) -> zbus::fdo::Result<Vec<String>> {
        let reply = self.window(app_settings::WindowCommand::Windows)?;
        Ok(reply.lines().map(str::to_string).collect())
    }

    /// Closes window `display`, or `package`'s window if `display` is
    /// negative, and finishes its tasks.
    fn close(&self, display: i32, package: &str) -> zbus::fdo::Result<String> {
        self.window(app_settings::WindowCommand::Close {
            display,
            package: package.into(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    #[test]
    fn connection_does_not_mean_boot_completed() {
        let relay = Relay::default();
        assert!(!relay.booted());
        let (socket, _peer) = UnixStream::pair().unwrap();
        let first = relay.attach(Events::new(Arc::new(socket.into())));
        assert!(!relay.booted());
        relay.boot_completed(first);
        assert!(relay.booted());

        let (socket, _peer) = UnixStream::pair().unwrap();
        let second = relay.attach(Events::new(Arc::new(socket.into())));
        assert!(!relay.booted());
        relay.boot_completed(first);
        assert!(
            !relay.booted(),
            "an obsolete connection cannot complete boot"
        );
        relay.boot_completed(second);
        assert!(relay.booted(), "a reconnect replays completed boot");
        relay.detach(first);
        assert!(relay.booted());
        relay.detach(second);
        assert!(!relay.booted());
        relay.boot_completed(second);
        assert!(
            !relay.booted(),
            "a disconnected channel cannot complete boot"
        );
    }
}
