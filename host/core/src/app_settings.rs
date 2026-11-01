//! The client side of the session's `Session1` D-Bus API, which offers per-app
//! settings and window operations. Both go through the bridge to the
//! privileged system app and do not use the session's exec socket.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const INTERFACE: &str = "dev.sargunv.Innkeeper.Session1";
pub const PATH: &str = "/dev/sargunv/Innkeeper/Session";
pub const MAX_JSON: usize = 64 * 1024;

/// The session's bus name, which ends in `.p_<name>` with hyphens replaced by
/// underscores.
pub fn bus_name(profile: &innkeeper_profile::profile::ProfileName) -> String {
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

/// A system app's answer to a [`Command`]. Android's answers carry the app's
/// info. OpenHarmony's system app answers `background` without info.
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
        Ok(reply)
    }
}

/// A window operation the bridge relays to the system app as JSON
/// (`WindowCommands.kt`).
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "kebab-case")]
pub enum WindowCommand {
    Launch {
        package: String,
        /// Empty for the launcher activity.
        component: String,
        /// Empty for no URI.
        uri: String,
        /// Documents grants, 16 hex digits each.
        grants: Vec<String>,
        token: String,
        wait: bool,
        new_window: bool,
        /// The window's size in dp, or 0 for the app's own size.
        width: u32,
        height: u32,
    },
    Windows,
    Close {
        /// A negative display selects the window of `package`.
        display: i32,
        package: String,
    },
}

impl WindowCommand {
    pub fn validate(&self) -> Result<()> {
        match self {
            WindowCommand::Launch {
                package,
                component,
                uri,
                token,
                width,
                height,
                ..
            } => {
                crate::validate::package(package)?;
                ensure!(component.len() <= 512 && uri.len() <= 8192, "too long");
                ensure!(
                    token.is_empty() || crate::validate::activation_token(token).is_some(),
                    "invalid activation token"
                );
                ensure!(*width <= 16384 && *height <= 16384, "window too large");
            }
            WindowCommand::Windows => {}
            WindowCommand::Close { display, package } => {
                if *display < 0 {
                    crate::validate::package(package)?;
                }
            }
        }
        Ok(())
    }

    pub fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        Ok(serde_json::to_vec(self)?)
    }
}

/// The system app's answer to a [`WindowCommand`].
#[derive(Debug, Deserialize)]
pub struct WindowResponse {
    pub error: String,
    pub reply: Option<String>,
}

impl WindowResponse {
    /// Returns the reply, or the error that the system app reported.
    pub fn decode(bytes: &[u8]) -> Result<String> {
        ensure!(bytes.len() <= MAX_JSON, "window response too large");
        let r: Self = serde_json::from_slice(bytes)?;
        ensure!(
            r.error.is_empty(),
            "{}",
            crate::validate::truncate(&r.error, 1024)
        );
        r.reply.context("empty window response")
    }
}

/// The Session1 object of profile `profile`'s running session.
fn proxy(
    profile: &innkeeper_profile::profile::ProfileName,
) -> Result<zbus::blocking::Proxy<'static>> {
    let connection = zbus::blocking::connection::Builder::session()?
        .method_timeout(Duration::from_secs(45))
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
    profile: &innkeeper_profile::profile::ProfileName,
    command: Command,
    report: crate::ops::Report,
) -> Result<AppInfo> {
    command.validate()?;
    crate::profiles::need_android(profile, "app settings")?;
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

/// Allows or forbids `package` to run in the background, through profile
/// `profile`'s session, which it starts if needed. OpenHarmony's system app
/// answers with no app info, so the info in the response is ignored.
pub fn set_background(
    profile: &innkeeper_profile::profile::ProfileName,
    package: &str,
    enabled: bool,
    report: crate::ops::Report,
) -> Result<()> {
    let mut command = Command::new(package, "background");
    command.enabled = enabled;
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
    Ok(())
}

/// The arguments of Session1 `Launch`: package, component, URI, grants,
/// token, wait, new window, width and height.
type LaunchArgs<'a> = (
    &'a str,
    &'a str,
    &'a str,
    &'a [u64],
    &'a str,
    bool,
    bool,
    u32,
    u32,
);

fn launch_args<'a>(
    l: &'a crate::android::Launch,
    o: &'a crate::android::LaunchOptions,
) -> LaunchArgs<'a> {
    use crate::android::Launch;
    let (package, component, uri, grants) = match l {
        Launch::Main { component } => (
            component
                .split_once('/')
                .map_or(component.as_str(), |c| c.0),
            component.as_str(),
            "",
            &[][..],
        ),
        Launch::View { package, uri } => (package.as_str(), "", uri.as_str(), &[][..]),
        Launch::File {
            package,
            grants,
            component,
        } => (
            package.as_str(),
            component.as_deref().unwrap_or_default(),
            "",
            grants.as_slice(),
        ),
    };
    let token = o.token.as_deref().unwrap_or_default();
    let (width, height) = o.size.unwrap_or_default();
    (
        package,
        component,
        uri,
        grants,
        token,
        o.wait,
        o.new_window,
        width,
        height,
    )
}

/// Opens `l` in a window through profile `profile`'s session. Returns the
/// window manager's reply, such as `launched display=N ...` or `focused ...`.
pub fn launch(
    profile: &innkeeper_profile::profile::ProfileName,
    l: &crate::android::Launch,
    o: &crate::android::LaunchOptions,
) -> Result<String> {
    proxy(profile)?
        .call("Launch", &launch_args(l, o))
        .context("launch failed")
}

/// Shows the screen window of profile `profile`'s OpenHarmony session, and
/// opens it again if the user closed it. `token` is an activation token or
/// empty. Returns false if the session has no screen window.
pub fn show_screen(profile: &innkeeper_profile::profile::ProfileName, token: &str) -> Result<bool> {
    Ok(proxy(profile)?.call("ShowScreen", &(token,))?)
}

/// Lists profile `profile`'s windows, one line each
/// (`display=N package=P ...`).
pub fn windows(profile: &innkeeper_profile::profile::ProfileName) -> Result<Vec<String>> {
    Ok(proxy(profile)?.call("Windows", &())?)
}

/// Creates a read-only grant for `file` on the running session's bridge and
/// returns its id. Android sees the file as `name`. Only the fd reaches the
/// bridge, not the path.
pub fn import(
    profile: &innkeeper_profile::profile::ProfileName,
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
        // OpenHarmony's system app answers without app info.
        let r = Response::decode(br#"{"error":"","info":null}"#).unwrap();
        assert!(r.error.is_empty() && r.info.is_none());
        assert!(Response::decode(br#"{"info":null}"#).is_err());
        use crate::android::{Launch, LaunchOptions};
        let main = Launch::Main {
            component: "org.wikipedia/.main.MainActivity".into(),
        };
        let defaults = LaunchOptions::default();
        let (pkg, component, uri, grants, token, wait, new_window, ..) =
            launch_args(&main, &defaults);
        assert_eq!(
            (pkg, component, uri, grants, token, wait, new_window),
            (
                "org.wikipedia",
                "org.wikipedia/.main.MainActivity",
                "",
                &[][..],
                "",
                false,
                false
            )
        );
        let chooser = Launch::File {
            package: "dev.sargunv.innkeeper.system".into(),
            grants: vec![0xabc, 0xdef],
            component: Some("dev.sargunv.innkeeper.system/.documents.OpenWithActivity".into()),
        };
        let opts = LaunchOptions {
            token: Some("kwin-3".into()),
            wait: true,
            new_window: true,
            size: Some((1280, 800)),
        };
        let (_, component, _, grants, token, wait, new_window, w, h) = launch_args(&chooser, &opts);
        assert_eq!((w, h), (1280, 800));
        assert_eq!(
            component,
            "dev.sargunv.innkeeper.system/.documents.OpenWithActivity"
        );
        assert_eq!(grants, [0xabc, 0xdef]);
        assert_eq!((token, wait, new_window), ("kwin-3", true, true));
        let launch = WindowCommand::Launch {
            package: "a.b".into(),
            component: "a.b/.Main".into(),
            uri: String::new(),
            grants: vec![format!("{:016x}", 7u64)],
            token: String::new(),
            wait: true,
            new_window: false,
            width: 0,
            height: 0,
        };
        assert_eq!(
            String::from_utf8(launch.encode().unwrap()).unwrap(),
            r#"{"action":"launch","package":"a.b","component":"a.b/.Main","uri":"","grants":["0000000000000007"],"token":"","wait":true,"new_window":false,"width":0,"height":0}"#
        );
        assert_eq!(
            String::from_utf8(
                WindowCommand::Close {
                    display: -1,
                    package: "a.b".into()
                }
                .encode()
                .unwrap()
            )
            .unwrap(),
            r#"{"action":"close","display":-1,"package":"a.b"}"#
        );
        assert!(
            WindowCommand::Close {
                display: -1,
                package: "../x".into()
            }
            .encode()
            .is_err()
        );
        assert_eq!(
            WindowResponse::decode(br#"{"error":"","reply":"closed display=3 package=a.b"}"#)
                .unwrap(),
            "closed display=3 package=a.b"
        );
        assert_eq!(
            WindowResponse::decode(br#"{"error":"no such window","reply":null}"#)
                .unwrap_err()
                .to_string(),
            "no such window"
        );
        assert!(Response::decode(&vec![b' '; MAX_JSON + 1]).is_err());
        assert_eq!(
            bus_name(&innkeeper_profile::profile::ProfileName::new("page-test").unwrap()),
            "dev.sargunv.Innkeeper.Session.p_page_test"
        );
    }
}
