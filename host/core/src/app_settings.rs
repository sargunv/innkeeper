//! Version 1 of the session's per-app API. All Android work goes through the
//! bridge and the privileged system app, never the session's exec socket.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const INTERFACE: &str = "dev.sargunv.Innkeeper.Session1";
pub const PATH: &str = "/dev/sargunv/Innkeeper/Session";
pub const MAX_JSON: usize = 64 * 1024;

/// The session's bus name: `.p_<name>`, with hyphens as underscores.
pub fn bus_name(profile: &crate::paths::ProfileName) -> String {
    format!(
        "dev.sargunv.Innkeeper.Session.p_{}",
        profile.replace('-', "_")
    )
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Command {
    pub package: String,
    pub action: String,
    pub permission: String,
    pub enabled: bool,
}

impl Command {
    pub fn new(package: &str, action: &str) -> Self {
        Self {
            package: package.into(),
            action: action.into(),
            permission: String::new(),
            enabled: false,
        }
    }

    pub fn validate(&self) -> Result<()> {
        crate::validate::package(&self.package)?;
        ensure!(
            matches!(
                self.action.as_str(),
                "info"
                    | "clear-cache"
                    | "clear-data"
                    | "force-stop"
                    | "permission"
                    | "escape-back"
                    | "background"
            ),
            "unknown app action"
        );
        if self.action == "permission" {
            ensure!(
                !self.permission.is_empty()
                    && self.permission.len() <= 255
                    && self
                        .permission
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._".contains(&c)),
                "invalid permission name"
            );
        } else {
            ensure!(self.permission.is_empty(), "unexpected permission");
        }
        ensure!(
            !self.enabled
                || matches!(
                    self.action.as_str(),
                    "permission" | "escape-back" | "background"
                ),
            "unexpected enabled flag"
        );
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Permission {
    pub name: String,
    pub granted: bool,
    pub editable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppInfo {
    pub app_bytes: u64,
    /// Android's data bytes include cache, as in Android Settings.
    pub data_bytes: u64,
    pub cache_bytes: u64,
    pub escape_back: bool,
    pub background: bool,
    pub permissions: Vec<Permission>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub error: String,
    pub info: Option<AppInfo>,
}

impl Response {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        ensure!(bytes.len() <= MAX_JSON, "app response too large");
        let reply: Self = serde_json::from_slice(bytes)?;
        ensure!(reply.error.len() <= 4096, "app error too large");
        if let Some(info) = &reply.info {
            ensure!(info.permissions.len() <= 512, "too many permissions");
        }
        ensure!(
            reply.info.is_some() || !reply.error.is_empty(),
            "empty app response"
        );
        Ok(reply)
    }
}

/// Profile `profile`'s running session's Session1 object.
fn proxy(profile: &crate::paths::ProfileName) -> Result<zbus::blocking::Proxy<'static>> {
    let connection = zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_secs(25))
        .build()?;
    let name = bus_name(profile);
    Ok(zbus::blocking::Proxy::new_owned(
        connection,
        name,
        PATH.to_string(),
        INTERFACE.to_string(),
    )?)
}

/// Starts Android if needed, then calls the bridge over the desktop bus.
pub fn request(
    profile: &crate::paths::ProfileName,
    command: Command,
    report: crate::ops::Report,
) -> Result<AppInfo> {
    command.validate()?;
    crate::ops::ensure_session(profile, report)?;
    let json: String = proxy(profile)?.call(
        "App",
        &(
            &command.package,
            &command.action,
            &command.permission,
            command.enabled,
        ),
    )?;
    let response = Response::decode(json.as_bytes())?;
    ensure!(response.error.is_empty(), "{}", response.error);
    response
        .info
        .ok_or_else(|| anyhow::anyhow!("missing app information"))
}

/// A read-only grant on the running session's bridge for `file` (only its
/// fd reaches the bridge, never its path), shown to Android as `name`.
pub fn import(
    profile: &crate::paths::ProfileName,
    file: &std::fs::File,
    name: &str,
) -> Result<u64> {
    use std::os::fd::AsFd;
    let fd = zbus::zvariant::Fd::from(file.as_fd());
    proxy(profile)?
        .call("Import", &(fd, name))
        .context("import the file into the session")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_encoding_and_validation() {
        let mut c = Command::new("dev.sargunv.innkeeper.probes", "permission");
        c.permission = "android.permission.CAMERA".into();
        c.enabled = true;
        assert_eq!(
            serde_json::from_slice::<Command>(&c.encode().unwrap()).unwrap(),
            c
        );
        for (package, action, permission, enabled) in [
            ("bad/../pkg", "info", "", false),
            ("a.b", "shell", "", false),
            ("a.b", "permission", "", true),
            ("a.b", "permission", "x\ny", true),
            ("a.b", "clear-data", "", true),
            ("a.b", "clear-cache", "android.permission.CAMERA", false),
        ] {
            assert!(
                Command {
                    package: package.into(),
                    action: action.into(),
                    permission: permission.into(),
                    enabled
                }
                .encode()
                .is_err()
            );
        }
        assert!(Response::decode(br#"{"error":"","info":null}"#).is_err());
        assert!(Response::decode(&vec![b' '; MAX_JSON + 1]).is_err());
        assert_eq!(
            bus_name(&crate::paths::ProfileName::new("page-test").unwrap()),
            "dev.sargunv.Innkeeper.Session.p_page_test"
        );
    }
}
