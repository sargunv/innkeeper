//! adb into a profile: Android's own adbd on a loopback TCP port of the
//! profile's ([`crate::profile::Settings::adb_port`]), with adb
//! authentication on (`ro.adb.secure=1`). The user's adb key
//! (`~/.android/adbkey.pub`, which `adb` creates on its first start) is
//! authorized in the profile's data, so a local `adb` connects without a
//! prompt and other users of the host can't. The user's adb server is told
//! about the profile as the emulator tells it, over the server's own protocol
//! on loopback (Android shares the host's network): no `adb` binary is needed
//! in the sandbox.

use std::fs;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// The user's adb public key file.
pub fn user_key_path() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".android/adbkey.pub")
}

/// The user's adb public key: its first line, if it looks like one.
pub fn user_key() -> Option<String> {
    let text = fs::read_to_string(user_key_path()).ok()?;
    let key = text.lines().next()?.trim();
    // Base64 of the key, then a comment (user@host).
    let (b64, _) = key.split_once(' ').unwrap_or((key, ""));
    (b64.len() > 100
        && b64
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b)))
    .then(|| key.to_string())
}

/// The file adbd reads authorized keys from, in data directory `data`.
fn keys_file(data: &Path) -> PathBuf {
    data.join("misc/adb/adb_keys")
}

/// Authorizes `key` in data directory `data`, keeping the keys Android
/// itself added. Returns whether it was new.
pub fn authorize(data: &Path, key: &str) -> Result<bool> {
    let file = keys_file(data);
    let old = fs::read_to_string(&file).unwrap_or_default();
    if old.lines().any(|l| l.trim() == key) {
        return Ok(false);
    }
    fs::create_dir_all(file.parent().unwrap())?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file)
        .with_context(|| format!("open {}", file.display()))?;
    let sep = if old.is_empty() || old.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    f.write_all(format!("{sep}{key}\n").as_bytes())?;
    Ok(true)
}

/// adbd's listen address for `port`: loopback only.
pub fn listen_addrs(port: u16) -> String {
    format!("tcp:localhost:{port}")
}

/// The serial adb knows the profile on `port` by: `emulator-<port - 1>`.
pub fn serial(port: u16) -> String {
    format!("emulator-{}", port - 1)
}

/// The user's adb server port (`ANDROID_ADB_SERVER_PORT`, default 5037).
pub fn server_port() -> u16 {
    std::env::var("ANDROID_ADB_SERVER_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(5037)
}

/// Once adbd listens on `port` (within `wait`), has the adb server on
/// `server` list it, as the emulator does when it starts after the server.
/// A server started later finds it by itself.
pub fn announce_when_listening(server: u16, port: u16, wait: Duration) -> Result<()> {
    let deadline = Instant::now() + wait;
    let addr = (Ipv4Addr::LOCALHOST, port).into();
    while TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err() {
        if Instant::now() >= deadline {
            bail!("adbd isn't listening on port {port}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    announce(server, port)
}

/// Tells the adb server on `server` about adbd on emulator port `port` and
/// waits for it to list the device.
fn announce(server: u16, port: u16) -> Result<()> {
    // No reply: the server connects in the background.
    let request = format!("host:emulator:{port}");
    TcpStream::connect_timeout(
        &(Ipv4Addr::LOCALHOST, server).into(),
        Duration::from_secs(2),
    )
    .context("no adb server is running")?
    .write_all(format!("{:04x}{request}", request.len()).as_bytes())?;
    let serial = serial(port);
    for _ in 0..30 {
        let devices = server_request(server, "host:devices")?;
        if devices
            .lines()
            .any(|l| l.split('\t').next() == Some(serial.as_str()))
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    bail!("the adb server didn't list {serial}")
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

    #[test]
    fn keys_are_appended_once_beside_androids_own() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/test/adb-keys");
        let _ = fs::remove_dir_all(&d);
        let key = format!("{} user@host", "A".repeat(700));
        assert!(authorize(&d, &key).unwrap());
        assert!(!authorize(&d, &key).unwrap());
        // A key Android added, without a final newline.
        fs::write(keys_file(&d), "QUJD android\nQkNE other").unwrap();
        assert!(authorize(&d, &key).unwrap());
        assert_eq!(
            fs::read_to_string(keys_file(&d)).unwrap(),
            format!("QUJD android\nQkNE other\n{key}\n")
        );
    }

    /// An adb server answering one request per connection with `replies`;
    /// returns its port and the requests it got.
    fn server(replies: Vec<&'static [u8]>) -> (u16, std::thread::JoinHandle<Vec<String>>) {
        use std::net::TcpListener;
        let l = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        let t = std::thread::spawn(move || {
            let mut requests = vec![];
            for reply in replies {
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
        (port, t)
    }

    #[test]
    fn announces_once_adbd_listens_then_waits_for_the_device() {
        let adbd = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = adbd.local_addr().unwrap().port();
        let listed = format!("emulator-{}\tdevice\n", port - 1);
        let reply: &'static [u8] = Box::leak(
            format!("OKAY{:04x}{listed}", listed.len())
                .into_bytes()
                .into_boxed_slice(),
        );
        let (server, t) = server(vec![b"", b"OKAY0000", reply]);
        announce_when_listening(server, port, Duration::from_secs(5)).unwrap();
        assert_eq!(
            t.join().unwrap(),
            [
                format!("host:emulator:{port}"),
                "host:devices".into(),
                "host:devices".into()
            ]
        );
        drop(adbd);
        // Nothing listening: it gives up at the deadline.
        assert!(announce_when_listening(server, port, Duration::from_millis(300)).is_err());
    }

    #[test]
    fn talks_the_adb_server_protocol() {
        let (port, t) = server(vec![b"OKAY0004abcd"]);
        assert_eq!(server_request(port, "host:devices").unwrap(), "abcd");
        assert_eq!(t.join().unwrap(), ["host:devices"]);
        let (port, _t) = server(vec![b"FAIL0004nope"]);
        assert_eq!(
            server_request(port, "x").unwrap_err().to_string(),
            "adb: nope"
        );
        let (port, _t) = server(vec![b"HTTP/1.1 400"]);
        assert!(server_request(port, "x").is_err());
        assert_eq!(serial(5557), "emulator-5556");
    }
}
