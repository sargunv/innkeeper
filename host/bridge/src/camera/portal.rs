//! The XDG Camera portal. `IsCameraPresent` needs no consent. `AccessCamera`
//! shows the desktop's dialog, unless the user already answered for this
//! app, and `OpenPipeWireRemote` then gives a PipeWire connection that sees
//! only the cameras.

use std::collections::HashMap;
use std::os::fd::OwnedFd;
use std::sync::atomic::{AtomicU64, Ordering};

use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

use super::proto::status;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const PORTAL_CAMERA: &str = "org.freedesktop.portal.Camera";
const PORTAL_REQUEST: &str = "org.freedesktop.portal.Request";

static NEXT_TOKEN: AtomicU64 = AtomicU64::new(1);

fn camera(conn: &Connection) -> zbus::Result<Proxy<'static>> {
    Proxy::new(conn, PORTAL, PORTAL_PATH, PORTAL_CAMERA)
}

/// Whether the desktop has a camera. Any error reads as no camera.
pub fn present(conn: &Connection) -> bool {
    camera(conn)
        .and_then(|p| p.get_property::<bool>("IsCameraPresent"))
        .map_err(|e| crate::debug!("camera: IsCameraPresent: {e}"))
        .unwrap_or(false)
}

/// Asks for the camera and waits for the answer. The error is the `STATUS`
/// to report and why.
pub fn access(conn: &Connection) -> Result<(), (u8, String)> {
    let failed = |e: zbus::Error| (classify(&e), e.to_string());
    let token = format!(
        "innkeeper_camera_{}_{}",
        std::process::id(),
        NEXT_TOKEN.fetch_add(1, Ordering::Relaxed)
    );
    let sender = conn
        .unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .unwrap_or_default();
    let request = format!("{PORTAL_PATH}/request/{sender}/{token}");
    // Subscribe before the call so the Response can't be missed.
    let rule = zbus::MatchRule::builder()
        .msg_type(zbus::message::Type::Signal)
        .sender(PORTAL)
        .and_then(|b| b.path(request.clone()))
        .and_then(|b| b.interface(PORTAL_REQUEST))
        .and_then(|b| b.member("Response"))
        .map(|b| b.build())
        .map_err(failed)?;
    let mut responses = MessageIterator::for_match_rule(rule, conn, Some(4)).map_err(failed)?;
    let mut opts: HashMap<&str, Value> = HashMap::new();
    opts.insert("handle_token", Value::from(token.as_str()));
    let _: OwnedObjectPath = camera(conn)
        .and_then(|p| p.call("AccessCamera", &(opts,)))
        .map_err(failed)?;
    let Some(Ok(msg)) = responses.next() else {
        return Err((status::UNAVAILABLE, "the portal connection closed".into()));
    };
    let code = msg
        .body()
        .deserialize::<(u32, HashMap<String, OwnedValue>)>()
        .map(|(c, _)| c)
        .unwrap_or(u32::MAX);
    response(code)
}

/// The outcome of a portal `Response` code.
fn response(code: u32) -> Result<(), (u8, String)> {
    match code {
        0 => Ok(()),
        1 => Err((status::DENIED, "the user denied the camera".into())),
        c => Err((
            status::UNAVAILABLE,
            format!("the portal ended the request without an answer ({c})"),
        )),
    }
}

/// A PipeWire connection that sees the cameras the user allowed.
pub fn open_remote(conn: &Connection) -> Result<OwnedFd, (u8, String)> {
    let opts: HashMap<&str, Value> = HashMap::new();
    let fd: zbus::zvariant::OwnedFd = camera(conn)
        .and_then(|p| p.call("OpenPipeWireRemote", &(opts,)))
        .map_err(|e| (classify(&e), e.to_string()))?;
    Ok(fd.into())
}

/// The `STATUS` a failed portal call ends with.
fn classify(e: &zbus::Error) -> u8 {
    if let zbus::Error::MethodError(name, _, _) = e {
        match name.as_str() {
            "org.freedesktop.portal.Error.NotAllowed"
            | "org.freedesktop.portal.Error.Cancelled"
            | "org.freedesktop.DBus.Error.AccessDenied" => return status::DENIED,
            _ => {}
        }
    }
    status::UNAVAILABLE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn responses() {
        assert_eq!(response(0), Ok(()));
        assert_eq!(response(1).unwrap_err().0, status::DENIED);
        assert_eq!(response(2).unwrap_err().0, status::UNAVAILABLE);
    }

    #[test]
    fn errors() {
        let err = |name: &str| {
            zbus::Error::MethodError(
                zbus::names::OwnedErrorName::try_from(name.to_owned()).unwrap(),
                None,
                zbus::message::Message::method_call("/", "x")
                    .unwrap()
                    .build(&())
                    .unwrap(),
            )
        };
        assert_eq!(
            classify(&err("org.freedesktop.portal.Error.NotAllowed")),
            status::DENIED
        );
        assert_eq!(
            classify(&err("org.freedesktop.DBus.Error.ServiceUnknown")),
            status::UNAVAILABLE
        );
        assert_eq!(classify(&zbus::Error::InvalidReply), status::UNAVAILABLE);
    }
}
