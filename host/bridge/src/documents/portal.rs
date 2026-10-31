//! The FileChooser portal (`org.freedesktop.portal.FileChooser`): OpenFile and
//! SaveFile, parented to the app's window through xdg-foreign.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use zbus::zvariant::{OwnedValue, Value};

use super::proto::{Pick, PickMode};
use crate::ipc::wire::errno::*;

const PORTAL: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.FileChooser";

static TOKEN: AtomicU64 = AtomicU64::new(0);

/// Shows the dialog and waits for the user. Returns the chosen paths, or
/// `-ECANCELED` if the user cancelled, `-EIO` if the portal failed.
pub fn pick(
    conn: &zbus::blocking::Connection,
    parent: &str,
    p: &Pick,
) -> Result<Vec<PathBuf>, i32> {
    pick_with_activation(conn, parent, p, "")
}

pub fn pick_with_activation(
    conn: &zbus::blocking::Connection,
    parent: &str,
    p: &Pick,
    activation: &str,
) -> Result<Vec<PathBuf>, i32> {
    let token = format!("innkeeper{}", TOKEN.fetch_add(1, Ordering::Relaxed));
    let sender = conn
        .unique_name()
        .map(|n| n.trim_start_matches(':').replace('.', "_"))
        .ok_or(-EIO)?;
    let request = format!("{PATH}/request/{sender}/{token}");
    // Subscribe before calling, so the response can't be missed.
    let proxy = zbus::blocking::Proxy::new(
        conn,
        PORTAL,
        request.as_str(),
        "org.freedesktop.portal.Request",
    )
    .map_err(|e| fail("request proxy", e))?;
    let mut responses = proxy
        .receive_signal("Response")
        .map_err(|e| fail("subscribe", e))?;

    let mut opts: HashMap<&str, Value<'_>> = HashMap::new();
    opts.insert("handle_token", Value::from(token.as_str()));
    opts.insert("modal", Value::from(true));
    if !activation.is_empty() {
        opts.insert("activation_token", Value::from(activation));
    }
    if p.mode == PickMode::OpenMultiple {
        opts.insert("multiple", Value::from(true));
    }
    if p.mode == PickMode::Folder {
        opts.insert("directory", Value::from(true));
    }
    let filters = filters(&p.mimes);
    if !filters.is_empty() {
        opts.insert("filters", Value::from(filters.clone()));
        opts.insert("current_filter", Value::from(filters[0].clone()));
    }
    let (method, default_title) = match p.mode {
        PickMode::Save => ("SaveFile", "Save File"),
        PickMode::Folder => ("OpenFile", "Choose Folder"),
        _ => ("OpenFile", "Open File"),
    };
    if p.mode == PickMode::Save && !p.name.is_empty() {
        opts.insert("current_name", Value::from(p.name.as_str()));
    }
    let title = if p.title.is_empty() {
        default_title
    } else {
        &p.title
    };
    conn.call_method(
        Some(PORTAL),
        PATH,
        Some(IFACE),
        method,
        &(parent, title, opts),
    )
    .map_err(|e| fail(method, e))?;

    let msg = responses
        .next()
        .ok_or_else(|| fail("response", "no signal"))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = msg
        .body()
        .deserialize()
        .map_err(|e| fail("response body", e))?;
    match code {
        0 => {}
        1 => return Err(-ECANCELED),
        _ => return Err(-EIO),
    }
    let uris: Vec<String> = results
        .get("uris")
        .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
        .unwrap_or_default();
    let paths: Vec<PathBuf> = uris
        .iter()
        .filter_map(|u| innkeeper_core::uri::file_path(u))
        .collect();
    if paths.is_empty() || paths.len() != uris.len() {
        crate::warn!(
            "documents: portal returned no local files ({} URIs)",
            uris.len()
        );
        return Err(-EIO);
    }
    Ok(paths)
}

fn fail(what: &str, e: impl std::fmt::Display) -> i32 {
    crate::warn!("documents: portal {what}: {e}");
    -EIO
}

/// The portal's `filters`: the app's exact MIME types as one filter, plus
/// "All files". Wildcards (`text/*`) are left out: KDE's portal refuses them,
/// and an app asking only for those gets no filter.
fn filters(mimes: &[String]) -> Vec<(String, Vec<(u32, String)>)> {
    let specific: Vec<&String> = mimes.iter().filter(|m| !m.ends_with("/*")).collect();
    if specific.is_empty() {
        return vec![];
    }
    let mut v = vec![(
        "Supported files".to_string(),
        specific.iter().map(|m| (1u32, m.to_string())).collect(),
    )];
    v.push(("All files".to_string(), vec![(0u32, "*".to_string())]));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_list() {
        assert!(filters(&["*/*".into()]).is_empty());
        assert!(filters(&["text/*".into()]).is_empty());
        let f = filters(&["text/plain".into(), "image/*".into(), "image/png".into()]);
        assert_eq!(
            f[0].1,
            vec![(1, "text/plain".into()), (1, "image/png".into())]
        );
        assert_eq!(f[1].0, "All files");
    }
}
