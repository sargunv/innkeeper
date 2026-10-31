//! Observable LauncherEntry and DND behaviour on a private, non-activating bus.
use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::time::Duration;

use innkeeper_bridge::ipc::server::Events;
use innkeeper_bridge::system::notify::{Note, NotifyHub, flags};
use zbus::blocking::{MessageIterator, connection::Builder};
use zbus::zvariant::OwnedValue;

struct Bus(Child);
impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Server {
    // 2 means this server doesn't implement Plasma's property (GNOME).
    inhibited: Arc<AtomicU8>,
    calls: Arc<AtomicU32>,
}
#[zbus::interface(name = "org.freedesktop.Notifications")]
impl Server {
    fn get_capabilities(&self) -> Vec<String> {
        vec![]
    }
    #[zbus(property)]
    fn inhibited(&self) -> zbus::fdo::Result<bool> {
        match self.inhibited.load(Ordering::SeqCst) {
            2 => Err(zbus::fdo::Error::UnknownProperty("Inhibited".into())),
            n => Ok(n == 1),
        }
    }
    #[allow(clippy::too_many_arguments)]
    fn notify(
        &self,
        _app: &str,
        replaces: u32,
        _icon: &str,
        _title: &str,
        _body: &str,
        _actions: Vec<String>,
        _hints: HashMap<String, OwnedValue>,
        _timeout: i32,
    ) -> u32 {
        let id = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        if replaces == 0 { id } else { replaces }
    }
    fn close_notification(&self, _id: u32) {}
}
#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.portal.Error")]
enum SettingsError {
    NotFound(String),
}
struct Settings;
#[zbus::interface(name = "org.freedesktop.portal.Settings")]
impl Settings {
    fn read_one(&self, _namespace: &str, _key: &str) -> Result<OwnedValue, SettingsError> {
        Err(SettingsError::NotFound(
            "Requested setting not found".into(),
        ))
    }
}

#[test]
fn launcher_updates_and_host_dnd() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/test/notifications");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = dir.join("bus.conf");
    // No service directories: this bus cannot activate host desktop services.
    std::fs::write(&config, "<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><policy context=\"default\"><allow send_destination=\"*\"/><allow receive_sender=\"*\"/><allow own=\"*\"/></policy></busconfig>").unwrap();
    let mut bus = Bus(Command::new("dbus-daemon")
        .args(["--nofork", "--print-address", "--config-file"])
        .arg(&config)
        .env("XDG_RUNTIME_DIR", &dir)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap());
    let mut address = String::new();
    BufReader::new(bus.0.stdout.take().unwrap())
        .read_line(&mut address)
        .unwrap();
    let address = address.trim();
    let inhibited = Arc::new(AtomicU8::new(0));
    let calls = Arc::new(AtomicU32::new(0));
    let _server = Builder::address(address)
        .unwrap()
        .name("org.freedesktop.Notifications")
        .unwrap()
        .name("org.freedesktop.portal.Desktop")
        .unwrap()
        .serve_at(
            "/org/freedesktop/Notifications",
            Server {
                inhibited: inhibited.clone(),
                calls: calls.clone(),
            },
        )
        .unwrap()
        .serve_at("/org/freedesktop/portal/desktop", Settings)
        .unwrap()
        .build()
        .unwrap();
    let monitor = Builder::address(address).unwrap().build().unwrap();
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .interface("com.canonical.Unity.LauncherEntry")
        .unwrap()
        .member("Update")
        .unwrap()
        .build();
    let iter = MessageIterator::for_match_rule(rule, &monitor, Some(64)).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        for msg in iter.flatten() {
            let update: (String, HashMap<String, OwnedValue>) = msg.body().deserialize().unwrap();
            if tx.send(update).is_err() {
                break;
            }
        }
    });
    let conn = Builder::address(address).unwrap().build().unwrap();
    let hub = NotifyHub::start(conn.clone()).unwrap();
    let (sock, peer) = rustix::net::socketpair(
        rustix::net::AddressFamily::UNIX,
        rustix::net::SocketType::SEQPACKET,
        rustix::net::SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    let events = Events::new(Arc::new(sock));
    let mut note = Note {
        package: "dev.sargunv.innkeeper.probes".into(),
        app_name: "Probe".into(),
        title: "Download".into(),
        body: "42%".into(),
        flags: flags::BADGE,
        urgency: 1,
        progress: 42,
        actions: vec![],
        icon: None,
        picture: None,
    };
    let update = |progress: f64, visible: bool, count: i64, urgent: bool| {
        let (uri, props) = rx
            .recv_timeout(Duration::from_secs(2))
            .expect("launcher Update");
        assert_eq!(
            uri,
            format!(
                "application://{}.desktop",
                innkeeper_core::desktop::app_id(&note.package)
            )
        );
        assert_eq!(f64::try_from(&props["progress"]).unwrap(), progress);
        assert_eq!(bool::try_from(&props["progress-visible"]).unwrap(), visible);
        assert_eq!(i64::try_from(&props["count"]).unwrap(), count);
        assert_eq!(bool::try_from(&props["count-visible"]).unwrap(), count != 0);
        assert_eq!(bool::try_from(&props["urgent"]).unwrap(), urgent);
        println!("{uri}: progress={progress} visible={visible} count={count} urgent={urgent}");
    };
    hub.post(1, &events, 1, &note).unwrap();
    update(0.42, true, 1, false);
    hub.post(
        1,
        &events,
        2,
        &Note {
            progress: 84,
            ..note.clone()
        },
    )
    .unwrap();
    update(0.63, true, 2, false);
    hub.close(1, 2);
    update(0.42, true, 1, false);
    // Replacement does not increment count. Ongoing never contributes badges.
    note.flags |= flags::ONGOING;
    note.progress = -2;
    note.urgency = 2;
    let host = hub.post(1, &events, 1, &note).unwrap();
    update(0.5, true, 0, true);
    // Plasma allows dismissing resident popups. Android may keep the task:
    // wait for the DISMISSED event, then verify launcher state survived.
    _server
        .emit_signal(
            None::<&str>,
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
            "NotificationClosed",
            &(host, 2u32),
        )
        .unwrap();
    let mut poll = [rustix::event::PollFd::new(
        &peer,
        rustix::event::PollFlags::IN,
    )];
    let timeout = rustix::time::Timespec {
        tv_sec: 2,
        tv_nsec: 0,
    };
    assert_eq!(rustix::event::poll(&mut poll, Some(&timeout)).unwrap(), 1);
    let mut packet = [0; innkeeper_bridge::ipc::wire::MAX_MSG + 1];
    innkeeper_bridge::ipc::transport::recv(&peer, &mut packet)
        .unwrap()
        .unwrap();
    assert!(hub.contains(1, 1));
    assert!(
        rx.try_recv().is_err(),
        "popup dismissal must retain launcher state"
    );
    note.progress = 50;
    note.flags = 0; // channel disallows badges
    hub.post(1, &events, 2, &note).unwrap();
    update(0.5, true, 0, true);
    hub.close_all(1);
    // HashMap iteration may clear either task first; consume both then verify reset.
    let _ = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    update(0.0, false, 0, false);
    // DND still publishes launcher state, but makes no Notify call.
    let before = calls.load(Ordering::SeqCst);
    inhibited.store(1, Ordering::SeqCst);
    assert_eq!(hub.post(1, &events, 3, &note).unwrap(), 0);
    update(0.5, true, 0, true);
    assert_eq!(calls.load(Ordering::SeqCst), before);
    assert!(hub.contains(1, 3));
    assert_eq!(hub.count(1), 1);
    // GNOME 48's portal doesn't export show-banners. Leave notification
    // delivery to Shell's native DND policy when Inhibited is unsupported.
    inhibited.store(2, Ordering::SeqCst);
    let missing = conn.call_method(
        Some("org.freedesktop.portal.Desktop"),
        "/org/freedesktop/portal/desktop",
        Some("org.freedesktop.portal.Settings"),
        "ReadOne",
        &("org.gnome.desktop.notifications", "show-banners"),
    );
    assert!(matches!(missing, Err(zbus::Error::MethodError(name, _, _))
        if name.as_str() == "org.freedesktop.portal.Error.NotFound"));
    assert_ne!(hub.post(1, &events, 3, &note).unwrap(), 0);
    update(0.5, true, 0, true);
    assert_eq!(calls.load(Ordering::SeqCst), before + 1);
    hub.close_everything();
    update(0.0, false, 0, false);
    drop(hub);
    drop(bus); // terminate the iterator's bus and join the reader
    reader.join().unwrap();
}
