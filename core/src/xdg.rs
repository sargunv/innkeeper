//! Host desktop integration: desktop entries, hicolor icons, the desktop and
//! MIME databases, and default handlers in `mimeapps.list`.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::desktop::{self, AppEntry, INSTALLER_ID, MANAGER_ID, PREFIX};
use crate::proc;

/// Sizes we render icons at; all are hicolor theme directories.
pub const ICON_SIZES: &[u32] = &[48, 64, 128, 256, 512];
/// hicolor's fixed-size directories (for APK PNG fallbacks).
const HICOLOR_SIZES: &[u32] = &[16, 22, 24, 32, 36, 48, 64, 72, 96, 128, 192, 256, 512];

pub struct Dirs {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
}

fn home() -> PathBuf {
    PathBuf::from(env::var_os("HOME").unwrap_or_else(|| "/".into()))
}

fn xdg(var: &str, default: &str) -> PathBuf {
    env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home().join(default))
}

impl Dirs {
    pub fn from_env() -> Self {
        Self {
            data: xdg("XDG_DATA_HOME", ".local/share"),
            config: xdg("XDG_CONFIG_HOME", ".config"),
            cache: xdg("XDG_CACHE_HOME", ".cache").join("linuxapk"),
        }
    }

    pub fn applications(&self) -> PathBuf {
        self.data.join("applications")
    }

    pub fn hicolor(&self) -> PathBuf {
        self.data.join("icons/hicolor")
    }

    /// `pkg` must already be validated.
    pub fn app_desktop(&self, pkg: &str) -> PathBuf {
        self.applications().join(format!("{PREFIX}{pkg}.desktop"))
    }

    pub fn icon_path(&self, size: u32, id: &str) -> PathBuf {
        self.hicolor().join(format!("{size}x{size}/apps/{id}.png"))
    }
}

/// Writes `data` to `path` via a temporary file and rename.
pub fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    let dir = path.parent().context("no parent dir")?;
    fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp",
        path.file_name().unwrap().to_string_lossy()
    ));
    fs::write(&tmp, data)?;
    fs::rename(&tmp, path).with_context(|| format!("write {}", path.display()))
}

/// Runs a desktop helper tool if it's installed; failures are warnings.
fn tool(name: &str, args: &[&std::ffi::OsStr]) {
    if proc::which(name).is_none() {
        return;
    }
    match proc::run(
        Command::new(name).args(args),
        Stdio::null(),
        Duration::from_secs(60),
    ) {
        Ok(o) if o.ok() => {}
        Ok(o) => eprintln!("linuxapk: {name}: {}", o.stderr.trim()),
        Err(e) => eprintln!("linuxapk: {name}: {e}"),
    }
}

pub fn update_desktop_database(d: &Dirs) {
    tool("update-desktop-database", &[d.applications().as_os_str()]);
}

fn update_icon_cache(d: &Dirs) {
    let h = d.hicolor();
    // Only if a cache already exists: a stale one hides new icons, but we don't
    // create one where the desktop doesn't use it.
    if h.join("icon-theme.cache").is_file() {
        tool(
            "gtk-update-icon-cache",
            &["-q".as_ref(), "-f".as_ref(), "-t".as_ref(), h.as_os_str()],
        );
    }
}

/// Removes every hicolor PNG named `<id>.png` (all sizes). Returns how many.
fn remove_icons(d: &Dirs, id: &str) -> usize {
    let mut n = 0;
    if let Ok(rd) = fs::read_dir(d.hicolor()) {
        for e in rd.flatten() {
            let p = e.path().join("apps").join(format!("{id}.png"));
            if fs::remove_file(&p).is_ok() {
                n += 1;
            }
        }
    }
    n
}

/// Width and height of a PNG (from IHDR).
pub fn png_size(png: &[u8]) -> Option<(u32, u32)> {
    if png.len() < 24 || !png.starts_with(b"\x89PNG\r\n\x1a\n") || &png[12..16] != b"IHDR" {
        return None;
    }
    let w = u32::from_be_bytes(png[16..20].try_into().ok()?);
    let h = u32::from_be_bytes(png[20..24].try_into().ok()?);
    Some((w, h))
}

/// Installs an app's icons and desktop entry, replacing any previous ones.
/// `icons` are (size, PNG); sizes that aren't hicolor directories are skipped.
pub fn install_app(d: &Dirs, entry: &mut AppEntry, icons: &[(u32, Vec<u8>)]) -> Result<PathBuf> {
    let id = entry.id();
    remove_icons(d, &id);
    let mut wrote = false;
    for (size, png) in icons {
        if HICOLOR_SIZES.contains(size) && png_size(png) == Some((*size, *size)) {
            write_atomic(&d.icon_path(*size, &id), png)?;
            wrote = true;
        }
    }
    entry.icon = Some(if wrote {
        id
    } else {
        "android-package-archive".into()
    });
    let path = d.app_desktop(&entry.package);
    write_atomic(&path, entry.render()?.as_bytes())?;
    update_icon_cache(d);
    update_desktop_database(d);
    Ok(path)
}

/// Removes an app's desktop entry and icons. Returns whether anything existed.
pub fn remove_app(d: &Dirs, pkg: &str) -> bool {
    let had_entry = fs::remove_file(d.app_desktop(pkg)).is_ok();
    let icons = remove_icons(d, &format!("{PREFIX}{pkg}"));
    if icons > 0 {
        update_icon_cache(d);
    }
    update_desktop_database(d);
    had_entry || icons > 0
}

/// The `Name=` of our desktop entry for `pkg`, if any.
pub fn app_label(d: &Dirs, pkg: &str) -> Option<String> {
    let t = fs::read_to_string(d.app_desktop(pkg)).ok()?;
    desktop::read_key(&t, "Name")
}

fn installer_desktop(d: &Dirs) -> PathBuf {
    d.applications().join(format!("{INSTALLER_ID}.desktop"))
}

fn mime_package(d: &Dirs) -> PathBuf {
    d.data.join("mime/packages/dev.linuxapk.xml")
}

/// Whether the APK handler entry is registered and opens the manager.
pub fn handler_registered(d: &Dirs) -> bool {
    fs::read_to_string(installer_desktop(d))
        .is_ok_and(|t| desktop::read_key(&t, "Icon").as_deref() == Some(MANAGER_ID))
}

fn manager_desktop(d: &Dirs) -> PathBuf {
    d.applications().join(format!("{MANAGER_ID}.desktop"))
}

fn manager_icon(d: &Dirs) -> PathBuf {
    d.hicolor().join(format!("scalable/apps/{MANAGER_ID}.svg"))
}

/// Adds the manager (`linuxapk-gui`) to the app menu, with its icon.
pub fn install_manager(d: &Dirs, cmd: &[String]) -> Result<Vec<PathBuf>> {
    let icon = manager_icon(d);
    write_atomic(&icon, desktop::MANAGER_ICON_SVG.as_bytes())?;
    update_icon_cache(d);
    let desk = manager_desktop(d);
    write_atomic(&desk, desktop::manager_entry(cmd)?.as_bytes())?;
    update_desktop_database(d);
    Ok(vec![desk, icon])
}

/// Removes the manager's app-menu entry and icon.
pub fn remove_manager(d: &Dirs) {
    let _ = fs::remove_file(manager_desktop(d));
    if fs::remove_file(manager_icon(d)).is_ok() {
        update_icon_cache(d);
    }
    update_desktop_database(d);
}

/// `linuxapk setup`: the APK open-with handler for `manager` (the
/// `linuxapk-gui` command line), bundle MIME types, defaults.
pub fn setup(d: &Dirs, manager: &[String]) -> Result<Vec<PathBuf>> {
    let desk = installer_desktop(d);
    write_atomic(&desk, desktop::installer_entry(manager)?.as_bytes())?;
    let mime = mime_package(d);
    write_atomic(&mime, desktop::MIME_PACKAGE_XML.as_bytes())?;
    tool("update-mime-database", &[d.data.join("mime").as_os_str()]);
    update_desktop_database(d);
    let file = format!("{INSTALLER_ID}.desktop");
    let mut args: Vec<&std::ffi::OsStr> = vec!["default".as_ref(), file.as_ref()];
    args.extend(
        desktop::PACKAGE_MIME_TYPES
            .iter()
            .map(|m| std::ffi::OsStr::new(*m)),
    );
    if proc::which("xdg-mime").is_some() {
        tool("xdg-mime", &args);
    } else {
        set_defaults(
            &d.config.join("mimeapps.list"),
            &file,
            desktop::PACKAGE_MIME_TYPES,
        )?;
    }
    Ok(vec![desk, mime])
}

/// `linuxapk setup --remove` (the handler part; see [`remove_manager`]).
pub fn unsetup(d: &Dirs) -> Result<()> {
    let _ = fs::remove_file(installer_desktop(d));
    if fs::remove_file(mime_package(d)).is_ok() {
        tool("update-mime-database", &[d.data.join("mime").as_os_str()]);
    }
    let file = format!("{INSTALLER_ID}.desktop");
    for list in [
        d.config.join("mimeapps.list"),
        d.applications().join("mimeapps.list"),
    ] {
        if let Ok(t) = fs::read_to_string(&list) {
            let new = remove_handler(&t, &file);
            if new != t {
                write_atomic(&list, new.as_bytes())?;
            }
        }
    }
    update_desktop_database(d);
    Ok(())
}

/// An app installed by Linux.apk, as its desktop entry describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    pub package: String,
    pub name: String,
    pub version: Option<String>,
    /// The session instance the entry runs in (`LINUXAPK_INSTANCE` baked into
    /// its `Exec`), empty for the default session.
    pub instance: String,
    /// The largest icon file, if any.
    pub icon: Option<PathBuf>,
}

/// The `LINUXAPK_INSTANCE=` value an `Exec` line sets, if any.
pub fn exec_instance(exec: &str) -> String {
    exec.split_whitespace()
        .take_while(|w| *w == "env" || w.contains('='))
        .find_map(|w| w.strip_prefix("LINUXAPK_INSTANCE="))
        .unwrap_or_default()
        .to_string()
}

/// The largest hicolor PNG for `id` (`dev.linuxapk.<pkg>`).
pub fn largest_icon(d: &Dirs, id: &str) -> Option<PathBuf> {
    HICOLOR_SIZES
        .iter()
        .rev()
        .map(|n| d.icon_path(*n, id))
        .find(|p| p.is_file())
}

/// Apps with a Linux.apk desktop entry (any instance), sorted by name.
pub fn desktop_apps(d: &Dirs) -> Vec<DesktopApp> {
    let mut v = vec![];
    let Ok(rd) = fs::read_dir(d.applications()) else {
        return v;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let Some(pkg) = name
            .strip_prefix(PREFIX)
            .and_then(|n| n.strip_suffix(".desktop"))
        else {
            continue;
        };
        if crate::validate::package(pkg).is_err() {
            continue;
        }
        let Ok(t) = fs::read_to_string(e.path()) else {
            continue;
        };
        if desktop::read_key(&t, "X-LinuxApk-Package").as_deref() != Some(pkg) {
            continue;
        }
        v.push(DesktopApp {
            package: pkg.to_string(),
            name: desktop::read_key(&t, "Name").unwrap_or_else(|| pkg.to_string()),
            version: desktop::read_key(&t, "X-LinuxApk-Version"),
            instance: exec_instance(&desktop::read_key(&t, "Exec").unwrap_or_default()),
            icon: largest_icon(d, &format!("{PREFIX}{pkg}")),
        });
    }
    v.sort_by_key(|a| (a.name.to_lowercase(), a.package.clone()));
    v
}

/// Drops `id` from every association in a `mimeapps.list`, and keys left
/// without handlers. Other lines are kept byte for byte.
pub fn remove_handler(text: &str, id: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches('\n');
        if !body.starts_with('[')
            && !body.starts_with('#')
            && let Some((k, v)) = body.split_once('=')
            && v.split(';').any(|x| x.trim() == id)
        {
            let rest: Vec<&str> = v
                .split(';')
                .map(str::trim)
                .filter(|x| !x.is_empty() && *x != id)
                .collect();
            if !rest.is_empty() {
                out.push_str(&format!("{k}={};\n", rest.join(";")));
            }
            continue;
        }
        out.push_str(line);
    }
    out
}

/// Minimal `xdg-mime default` fallback: sets `[Default Applications]` keys.
fn set_defaults(list: &Path, id: &str, types: &[&str]) -> Result<()> {
    let t = fs::read_to_string(list).unwrap_or_default();
    let mut lines: Vec<String> = t.lines().map(str::to_string).collect();
    let start = match lines
        .iter()
        .position(|l| l.trim() == "[Default Applications]")
    {
        Some(i) => i,
        None => {
            lines.push("[Default Applications]".into());
            lines.len() - 1
        }
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    let mut group: Vec<String> = lines[start + 1..end]
        .iter()
        .filter(|l| !types.iter().any(|ty| l.starts_with(&format!("{ty}="))))
        .cloned()
        .collect();
    group.extend(types.iter().map(|ty| format!("{ty}={id};")));
    lines.splice(start + 1..end, group);
    write_atomic(list, (lines.join("\n") + "\n").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mimeapps_removal() {
        let t = "[Default Applications]\n\
                 application/vnd.android.package-archive=dev.linuxapk.installer.desktop;\n\
                 text/plain=dev.zed.Zed.desktop;\n\
                 application/x-android-apks=a.desktop;dev.linuxapk.installer.desktop;\n\
                 [Added Associations]\n\
                 application/x-android-xapk=dev.linuxapk.installer.desktop\n";
        assert_eq!(
            remove_handler(t, "dev.linuxapk.installer.desktop"),
            "[Default Applications]\ntext/plain=dev.zed.Zed.desktop;\n\
             application/x-android-apks=a.desktop;\n[Added Associations]\n"
        );
        let other = "[Default Applications]\ntext/plain=dev.linuxapk.installer.desktop.bak;\n";
        assert_eq!(
            remove_handler(other, "dev.linuxapk.installer.desktop"),
            other
        );
    }

    #[test]
    fn instance_of_exec() {
        assert_eq!(
            exec_instance("env LINUXAPK_INSTANCE=gui /x/linuxapk run a.b %U"),
            "gui"
        );
        assert_eq!(
            exec_instance("env LINUXAPK_SESSION=/s LINUXAPK_INSTANCE=cli linuxapk run a.b %U"),
            "cli"
        );
        assert_eq!(exec_instance("linuxapk run a.b %U"), "");
        assert_eq!(exec_instance("linuxapk run LINUXAPK_INSTANCE=x"), "");
    }

    #[test]
    fn png_dims() {
        let mut p = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        p.extend(256u32.to_be_bytes());
        p.extend(128u32.to_be_bytes());
        assert_eq!(png_size(&p), Some((256, 128)));
        assert_eq!(png_size(b"GIF89a"), None);
    }

    #[test]
    fn app_files_roundtrip() {
        let root = env::temp_dir().join(format!("linuxapk-xdg-{}", std::process::id()));
        let d = Dirs {
            data: root.join("share"),
            config: root.join("config"),
            cache: root.join("cache"),
        };
        let mut e = AppEntry {
            package: "org.example.app".into(),
            name: "Example".into(),
            names: vec![],
            run: vec!["/x/linuxapk".into(), "run".into()],
            icon: None,
            version: String::new(),
            categories: vec!["Utility".into()],
            schemes: vec![],
            web_hosts: vec![],
            origin: None,
        };
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(48u32.to_be_bytes());
        png.extend(48u32.to_be_bytes());
        // A wrong-size PNG is skipped rather than misfiled.
        let p = install_app(&d, &mut e, &[(48, png.clone()), (64, png.clone())]).unwrap();
        assert!(p.ends_with("applications/dev.linuxapk.org.example.app.desktop"));
        assert!(d.icon_path(48, "dev.linuxapk.org.example.app").is_file());
        assert!(!d.icon_path(64, "dev.linuxapk.org.example.app").exists());
        assert_eq!(e.icon.as_deref(), Some("dev.linuxapk.org.example.app"));
        assert_eq!(app_label(&d, "org.example.app").as_deref(), Some("Example"));
        let apps = desktop_apps(&d);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].package, "org.example.app");
        assert_eq!(apps[0].instance, "");
        assert_eq!(
            apps[0].icon.as_deref(),
            Some(d.icon_path(48, "dev.linuxapk.org.example.app").as_path())
        );
        // Reinstall replaces; remove cleans up.
        install_app(&d, &mut e, &[]).unwrap();
        assert!(!d.icon_path(48, "dev.linuxapk.org.example.app").exists());
        assert_eq!(e.icon.as_deref(), Some("android-package-archive"));
        assert!(remove_app(&d, "org.example.app"));
        assert!(!p.exists());
        assert!(!remove_app(&d, "org.example.app"));
        // Defaults fallback.
        let list = d.config.join("mimeapps.list");
        fs::create_dir_all(&d.config).unwrap();
        fs::write(
            &list,
            "[Default Applications]\ntext/plain=z.desktop;\n[Added Associations]\nx/y=q.desktop;\n",
        )
        .unwrap();
        set_defaults(&list, "dev.linuxapk.installer.desktop", &["a/b"]).unwrap();
        assert_eq!(
            fs::read_to_string(&list).unwrap(),
            "[Default Applications]\ntext/plain=z.desktop;\na/b=dev.linuxapk.installer.desktop;\n\
             [Added Associations]\nx/y=q.desktop;\n"
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
