//! App-menu entries written from inside the Flatpak through the
//! DynamicLauncher portal (`org.freedesktop.portal.DynamicLauncher`). A
//! sandboxed app can't write the user's `applications` dir, so the portal
//! writes the entry after the user confirms it in a dialog. The portal rewrites
//! `Exec` to `flatpak run --command=<program> <app id> <args>`, which keeps
//! working across updates of the Flatpak. Entry ids must have the form
//! `<app id>.<something>`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.DynamicLauncher";
/// The `launcher_type` value for an application.
const APPLICATION: u32 = 1;

static TOKEN: AtomicU64 = AtomicU64::new(0);

/// What the user answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Added,
    Declined,
}

fn connect() -> Result<Connection> {
    Connection::session().context("connect to the session bus")
}

/// Asks the user to add `entry` to the app menu as `id`, which has the form
/// `<app id>.<suffix>.desktop`. `entry` is a desktop file without `Icon`,
/// because the portal sets `Name` and `Icon` from the dialog. `name` and
/// `icon` are the dialog's defaults, and `icon` is a square PNG of at most 512
/// px. Blocks until the user answers.
pub fn install(id: &str, name: &str, icon: &[u8], entry: &str) -> Result<Answer> {
    let conn = connect()?;
    let token = format!(
        "innkeeper{}_{}",
        std::process::id(),
        TOKEN.fetch_add(1, Ordering::Relaxed)
    );
    let sender = conn
        .unique_name()
        .context("no unique bus name")?
        .trim_start_matches(':')
        .replace('.', "_");
    let request = format!("{PATH}/request/{sender}/{token}");
    // Subscribe before calling, so the response can't be missed.
    let proxy = Proxy::new(
        &conn,
        PORTAL,
        request.as_str(),
        "org.freedesktop.portal.Request",
    )?;
    let mut responses = proxy.receive_signal("Response")?;
    // A serialized GBytesIcon: ('bytes', <ay>).
    let icon = Value::from(("bytes", Value::from(icon)));
    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("handle_token", Value::from(token.as_str()));
    opts.insert("modal", Value::from(true));
    opts.insert("launcher_type", Value::from(APPLICATION));
    opts.insert("editable_icon", Value::from(false));
    conn.call_method(
        Some(PORTAL),
        PATH,
        Some(IFACE),
        "PrepareInstall",
        &("", name, icon, opts),
    )
    .context("DynamicLauncher.PrepareInstall")?;
    let msg = responses.next().context("DynamicLauncher: no response")?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg.body().deserialize()?;
    match code {
        0 => {}
        1 => return Ok(Answer::Declined),
        c => bail!("DynamicLauncher.PrepareInstall failed (response {c})"),
    }
    let token: String = results
        .get("token")
        .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
        .context("DynamicLauncher: no token")?;
    let opts: HashMap<&str, Value<'_>> = HashMap::new();
    conn.call_method(
        Some(PORTAL),
        PATH,
        Some(IFACE),
        "Install",
        &(token.as_str(), id, entry, opts),
    )
    .context("DynamicLauncher.Install")?;
    Ok(Answer::Added)
}

/// Returns the installed entry `id` as the portal wrote it, or None if there
/// is none.
pub fn get(id: &str) -> Result<Option<String>> {
    let conn = connect()?;
    match conn.call_method(Some(PORTAL), PATH, Some(IFACE), "GetDesktopEntry", &(id,)) {
        Ok(m) => Ok(Some(m.body().deserialize()?)),
        // The portal reports a missing entry as the unmapped GFileError from
        // reading the file.
        Err(zbus::Error::MethodError(name, _, _))
            if name.contains("g_2dfile_2derror_2dquark")
                || name.as_str() == "org.freedesktop.portal.Error.NotFound" =>
        {
            Ok(None)
        }
        Err(e) => Err(e).context("DynamicLauncher.GetDesktopEntry"),
    }
}

/// Removes entry `id` and its icon without a dialog.
pub fn uninstall(id: &str) -> Result<()> {
    let conn = connect()?;
    let opts: HashMap<&str, Value<'_>> = HashMap::new();
    conn.call_method(Some(PORTAL), PATH, Some(IFACE), "Uninstall", &(id, opts))
        .context("DynamicLauncher.Uninstall")?;
    Ok(())
}

/// Splits a command line as `g_shell_parse_argv` would, for the forms the
/// portal writes: bare words, `'...'`, and `"..."` with backslash escapes.
fn shell_words(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut it = s.chars().peekable();
    loop {
        while it.next_if(|c| c.is_ascii_whitespace()).is_some() {}
        if it.peek().is_none() {
            return out;
        }
        let mut w = String::new();
        while let Some(c) = it.next_if(|c| !c.is_ascii_whitespace()) {
            match c {
                '\'' => w.extend(it.by_ref().take_while(|c| *c != '\'')),
                '"' => {
                    while let Some(c) = it.next() {
                        match c {
                            '"' => break,
                            '\\' => w.extend(it.next()),
                            c => w.push(c),
                        }
                    }
                }
                '\\' => w.extend(it.next()),
                c => w.push(c),
            }
        }
        out.push(w);
    }
}

/// Reads back the command that an installed entry's `Exec` runs inside the
/// app. `exec` is the unescaped value that [`get`] returns.
/// `flatpak run [--opt...] --command=<program> <app id> <args>` becomes
/// `<program> <args>`. Returns None for any other shape.
pub fn app_command(exec: &str, app_id: &str) -> Option<Vec<String>> {
    let w = shell_words(exec);
    let w = w.strip_prefix(&["flatpak".to_string(), "run".to_string()])?;
    let opts = w.iter().take_while(|a| a.starts_with("--")).count();
    if w.get(opts)? != app_id {
        return None;
    }
    let program = w[..opts]
        .iter()
        .find_map(|a| a.strip_prefix("--command="))?;
    let mut v = vec![program.to_string()];
    v.extend(w[opts + 1..].iter().cloned());
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_rewritten_exec() {
        // What xdg-desktop-portal 1.22 writes for `Exec=innkeeper run a.b %U`.
        let exec = "flatpak run --command=innkeeper 'dev.sargunv.Innkeeper' run a.b %U";
        assert_eq!(
            app_command(exec, "dev.sargunv.Innkeeper").unwrap(),
            ["innkeeper", "run", "a.b", "%U"]
        );
        let named = "flatpak run --branch=master --command=env 'dev.sargunv.Innkeeper' \
                     INNKEEPER_PROFILE=gui innkeeper run \"a b\" %U";
        assert_eq!(
            app_command(named, "dev.sargunv.Innkeeper").unwrap(),
            [
                "env",
                "INNKEEPER_PROFILE=gui",
                "innkeeper",
                "run",
                "a b",
                "%U"
            ]
        );
        assert_eq!(
            app_command("/x/innkeeper run a.b", "dev.sargunv.Innkeeper"),
            None
        );
        assert_eq!(app_command(exec, "org.other.App"), None);
        assert_eq!(
            app_command(
                "flatpak run 'dev.sargunv.Innkeeper'",
                "dev.sargunv.Innkeeper"
            ),
            None
        );
    }

    #[test]
    fn shell_quoting() {
        assert_eq!(
            shell_words(r#"a 'b c' "d \" e" f\ g"#),
            ["a", "b c", "d \" e", "f g"]
        );
        assert!(shell_words("  ").is_empty());
    }
}
