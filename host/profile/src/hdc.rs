//! hdc into an OpenHarmony profile, the counterpart of [`crate::adb`]. The
//! image's hdcd listens on a loopback TCP port of the profile's
//! ([`crate::profile::Settings::hdc_port`]), with hdc authentication on
//! (`const.hdc.secure=1` in our layer). The shim binds hdcd's sockets to
//! loopback (ohos/shim). The user's hdc public key
//! (`~/.harmony/hdckey.pub`, which hdc creates when it first connects to a
//! device) is authorized in the profile's data, so a local `hdc tconn`
//! connects without a prompt. hdcd asks in OpenHarmony's window before it
//! accepts any other key.

use std::fs;
use std::io::Write;
use std::net::{Ipv4Addr, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// The user's hdc public key file.
pub fn user_key_path() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".harmony/hdckey.pub")
}

/// The user's hdc public key, if the file holds one PEM public key. hdcd
/// compares the PEM text that the client sends with the keys it knows.
pub fn user_key() -> Option<String> {
    parse_key(&fs::read_to_string(user_key_path()).ok()?)
}

/// The PEM public key in `text`, ending with a newline.
fn parse_key(text: &str) -> Option<String> {
    let body = text
        .trim_end()
        .strip_prefix("-----BEGIN PUBLIC KEY-----\n")?
        .strip_suffix("\n-----END PUBLIC KEY-----")?;
    (body.len() > 100
        && body
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+/=\n".contains(&b)))
    .then(|| format!("{}\n", text.trim_end()))
}

/// The file hdcd reads authorized keys from, in data directory `data`.
fn keys_file(data: &Path) -> PathBuf {
    data.join("service/el1/public/hdc/hdc_keys")
}

/// Authorizes `key` in data directory `data`, keeping the keys that hdcd
/// added. Returns whether the key was new.
pub fn authorize(data: &Path, key: &str) -> Result<bool> {
    let file = keys_file(data);
    let old = fs::read_to_string(&file).unwrap_or_default();
    if old.contains(key) {
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
    f.write_all(format!("{sep}{key}").as_bytes())?;
    Ok(true)
}

/// The address hdcd listens on for `port`.
pub fn address(port: u16) -> String {
    format!("127.0.0.1:{port}")
}

/// The command that connects the user's hdc server to the profile.
pub fn connect_command(port: u16) -> String {
    format!("hdc tconn {}", address(port))
}

/// The commands that start hdcd on `port` in the guest, or stop it, in
/// order. hdcd takes its port from `persist.hdc.port`.
pub fn commands(port: u16, on: bool) -> Vec<Vec<String>> {
    let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect();
    if on {
        vec![
            argv(&["param", "set", "persist.hdc.port", &port.to_string()]),
            argv(&["begetctl", "start_service", "hdcd"]),
        ]
    } else {
        vec![argv(&["begetctl", "stop_service", "hdcd"])]
    }
}

/// Waits up to `wait` for something to accept connections on loopback port
/// `port`.
pub fn wait_listening(port: u16, wait: Duration) -> Result<()> {
    let deadline = Instant::now() + wait;
    let addr = (Ipv4Addr::LOCALHOST, port).into();
    while TcpStream::connect_timeout(&addr, Duration::from_millis(500)).is_err() {
        if Instant::now() >= deadline {
            bail!("hdcd isn't listening on port {port}");
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pem(body: &str) -> String {
        format!("-----BEGIN PUBLIC KEY-----\n{body}\n-----END PUBLIC KEY-----\n")
    }

    #[test]
    fn keys_are_pem_public_keys() {
        let body = ["A".repeat(64), "B".repeat(64), "QIDAQAB".into()].join("\n");
        assert_eq!(parse_key(&pem(&body)), Some(pem(&body)));
        // A missing final newline gets one.
        assert_eq!(parse_key(pem(&body).trim_end()), Some(pem(&body)));
        for bad in [
            String::new(),
            pem("QUJD"),
            pem(&format!("{body}\n-----BEGIN PUBLIC KEY-----")),
            pem(&body).replace("PUBLIC", "RSA PRIVATE"),
            format!("x{}", pem(&body)),
        ] {
            assert_eq!(parse_key(&bad), None, "{bad}");
        }
    }

    #[test]
    fn keys_are_appended_once_beside_hdcds_own() {
        let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../out/test/hdc-keys");
        let _ = fs::remove_dir_all(&d);
        let key = pem(&"A".repeat(600));
        assert!(authorize(&d, &key).unwrap());
        assert!(!authorize(&d, &key).unwrap());
        // A key that hdcd added after a confirmation, without a final
        // newline.
        let theirs = pem(&"B".repeat(600));
        fs::write(keys_file(&d), theirs.trim_end()).unwrap();
        assert!(authorize(&d, &key).unwrap());
        assert_eq!(
            fs::read_to_string(keys_file(&d)).unwrap(),
            format!("{theirs}{key}")
        );
    }

    #[test]
    fn commands_set_the_port_then_start_hdcd() {
        assert_eq!(
            commands(55556, true),
            [
                vec!["param", "set", "persist.hdc.port", "55556"],
                vec!["begetctl", "start_service", "hdcd"],
            ]
        );
        assert_eq!(
            commands(55556, false),
            [vec!["begetctl", "stop_service", "hdcd"]]
        );
        assert_eq!(connect_command(55556), "hdc tconn 127.0.0.1:55556");
    }

    #[test]
    fn waits_for_a_listener() {
        let l = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = l.local_addr().unwrap().port();
        wait_listening(port, Duration::from_secs(5)).unwrap();
        drop(l);
        assert!(wait_listening(port, Duration::from_millis(300)).is_err());
    }
}
