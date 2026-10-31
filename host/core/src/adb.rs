//! adb into a profile ([`innkeeper_session::adb`]): turning it on and off,
//! live if the profile runs, and connecting the user's adb server to it.
//! The server is reached over its own protocol on loopback (Android shares
//! the host's network), so no `adb` binary is needed in the sandbox.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use innkeeper_session::adb;
use innkeeper_session::profile::Settings;

use crate::launcher::Session;
use crate::ops::{Event, Report};
use crate::paths::Prefix;

/// The address adb knows profile `settings` by.
pub fn serial(settings: &Settings) -> String {
    serial_of(settings.adb_port)
}

/// `emulator-<port - 1>` for an emulator port, as adb names a device there;
/// `127.0.0.1:<port>` for another one set explicitly.
fn serial_of(port: u16) -> String {
    if innkeeper_session::profile::ports().any(|p| p == port) {
        format!("emulator-{}", port - 1)
    } else {
        format!("127.0.0.1:{port}")
    }
}

/// Where adbd listens.
pub fn address(settings: &Settings) -> String {
    format!("127.0.0.1:{}", settings.adb_port)
}

/// Turns adb on or off for profile `name`, at once if it runs. Turning it on
/// gives it a port and authorizes the user's key. Returns the new settings.
pub fn set(name: &str, on: bool, report: Report) -> Result<Settings> {
    let paths = Prefix::locate()?.profile_paths(name)?;
    let mut s = Settings::load(&paths.data)?;
    s.adb = on;
    if on {
        s.assign_port(&paths.data_home, &paths.data)?;
        authorize(&paths.data, report)?;
    }
    s.save(&paths.data)?;
    let session = Session::of_profile(name)?;
    if session.running() {
        session.wait_booted(&|m| report(Event::Status(m.to_string())))?;
        apply(&session, &s)?;
    }
    Ok(s)
}

/// Starts or stops adbd in running `session` as `s` says.
fn apply(session: &Session, s: &Settings) -> Result<()> {
    let on = s.adb;
    if on {
        let addrs = adb::listen_addrs(s.adb_port);
        session.exec_ok(15, &["setprop", "service.adb.listen_addrs", &addrs])?;
    }
    session.exec_ok(
        15,
        &[
            "setprop",
            "vendor.innkeeper.adb",
            if on { "1" } else { "0" },
        ],
    )?;
    Ok(())
}

/// Why adb will refuse the user, if it will: they have no adb key.
pub fn missing_key() -> Option<String> {
    adb::user_key().is_none().then(|| {
        format!(
            "no adb key at {} (`adb start-server` creates one), so adb will be refused",
            adb::user_key_path().display()
        )
    })
}

/// Authorizes the user's adb key in data directory `data`, warning if there
/// is none.
fn authorize(data: &std::path::Path, report: Report) -> Result<()> {
    match adb::user_key() {
        Some(key) => {
            adb::authorize(data, &key)?;
        }
        None => report(Event::Warning(missing_key().unwrap_or_default())),
    }
    Ok(())
}

/// Connects the user's adb server to profile `name`, starting the profile if
/// needed. Returns its serial.
pub fn connect(name: &str, report: Report) -> Result<String> {
    let paths = Prefix::locate()?.profile_paths(name)?;
    let mut s = Settings::load(&paths.data)?;
    ensure!(
        s.adb,
        "adb is off for profile {name}; turn it on with `innkeeper profile set {name} --adb on`"
    );
    if s.assign_port(&paths.data_home, &paths.data)? {
        s.save(&paths.data)?;
    }
    authorize(&paths.data, report)?;
    let session = Session::of_profile(name)?;
    session.ensure(&|m| report(Event::Status(m.to_string())))?;
    // Also when it started while adb was being turned on.
    apply(&session, &s)?;
    let (serial, address) = (serial(&s), address(&s));
    announce(server_port(), s.adb_port, &serial).with_context(|| {
        format!("no adb server took it; `adb devices` starts one, or run `adb connect {address}`")
    })?;
    Ok(serial)
}

/// Has the adb server on `server` pick up adbd on `port` as `serial`: an
/// emulator port by announcing it, as the emulator does when it starts after
/// the server, else by `adb connect`. A server started later finds an
/// emulator port by itself.
fn announce(server: u16, port: u16, serial: &str) -> Result<()> {
    if !serial.starts_with("emulator-") {
        let reply = server_request(server, &format!("host:connect:127.0.0.1:{port}"))?;
        ensure!(reply.contains("connected to"), "adb: {reply}");
        return Ok(());
    }
    // No reply: the server connects in the background.
    let request = format!("host:emulator:{port}");
    TcpStream::connect_timeout(
        &(Ipv4Addr::LOCALHOST, server).into(),
        Duration::from_secs(2),
    )?
    .write_all(format!("{:04x}{request}", request.len()).as_bytes())?;
    for _ in 0..30 {
        let devices = server_request(server, "host:devices")?;
        if devices
            .lines()
            .any(|l| l.split('\t').next() == Some(serial))
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("the adb server didn't list {serial}")
}

/// The user's adb server port (`ANDROID_ADB_SERVER_PORT`, default 5037).
fn server_port() -> u16 {
    std::env::var("ANDROID_ADB_SERVER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5037)
}

/// One request to an adb server: a hex length and the request, answered by
/// OKAY or FAIL and a hex-length message.
fn server_request(port: u16, request: &str) -> Result<String> {
    let mut sock =
        TcpStream::connect_timeout(&(Ipv4Addr::LOCALHOST, port).into(), Duration::from_secs(2))?;
    sock.set_read_timeout(Some(Duration::from_secs(30)))?;
    sock.write_all(format!("{:04x}{request}", request.len()).as_bytes())?;
    let mut status = [0; 4];
    sock.read_exact(&mut status)?;
    let mut len = [0; 4];
    sock.read_exact(&mut len)?;
    let len = usize::from_str_radix(std::str::from_utf8(&len)?, 16).context("adb server reply")?;
    let mut msg = vec![0; len.min(4096)];
    sock.read_exact(&mut msg)?;
    let msg = String::from_utf8_lossy(&msg).into_owned();
    match &status {
        b"OKAY" => Ok(msg),
        b"FAIL" => bail!("adb: {msg}"),
        _ => bail!("not an adb server on port {port}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    /// A one-request adb server answering `reply`; returns its port and the
    /// request it got.
    fn server(reply: &'static [u8]) -> (u16, std::thread::JoinHandle<String>) {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut len = [0; 4];
            c.read_exact(&mut len).unwrap();
            let mut req =
                vec![0; usize::from_str_radix(std::str::from_utf8(&len).unwrap(), 16).unwrap()];
            c.read_exact(&mut req).unwrap();
            c.write_all(reply).unwrap();
            String::from_utf8(req).unwrap()
        });
        (port, t)
    }

    #[test]
    fn emulator_ports_get_emulator_serials() {
        assert_eq!(serial_of(5557), "emulator-5556");
        assert_eq!(serial_of(5585), "emulator-5584");
        assert_eq!(serial_of(5556), "127.0.0.1:5556");
        assert_eq!(serial_of(16500), "127.0.0.1:16500");
    }

    #[test]
    fn announces_an_emulator_port_then_waits_for_its_device() {
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let mut requests = vec![];
            for reply in [&b""[..], b"OKAY0000", b"OKAY0015emulator-5556\tdevice\n"] {
                let (mut c, _) = l.accept().unwrap();
                let mut len = [0; 4];
                c.read_exact(&mut len).unwrap();
                let n = usize::from_str_radix(std::str::from_utf8(&len).unwrap(), 16).unwrap();
                let mut req = vec![0; n];
                c.read_exact(&mut req).unwrap();
                c.write_all(reply).unwrap();
                requests.push(String::from_utf8(req).unwrap());
            }
            requests
        });
        announce(port, 5557, "emulator-5556").unwrap();
        assert_eq!(
            t.join().unwrap(),
            ["host:emulator:5557", "host:devices", "host:devices"]
        );
    }

    #[test]
    fn talks_the_adb_server_protocol() {
        let (port, t) = server(b"OKAY001cconnected to 127.0.0.1:16500");
        let r = server_request(port, "host:connect:127.0.0.1:16500").unwrap();
        assert_eq!(r, "connected to 127.0.0.1:16500");
        assert_eq!(t.join().unwrap(), "host:connect:127.0.0.1:16500");
        let (port, _t) = server(b"FAIL0004nope");
        assert_eq!(
            server_request(port, "x").unwrap_err().to_string(),
            "adb: nope"
        );
        let (port, _t) = server(b"HTTP/1.1 400");
        assert!(server_request(port, "x").is_err());
    }
}
