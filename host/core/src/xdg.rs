//! Host desktop integration: app desktop entries, their hicolor icons, and the
//! desktop database.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result};

use crate::desktop::{self, AppEntry, PREFIX};
use crate::paths::{self, xdg_home};
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

impl Dirs {
    pub fn from_env() -> Self {
        Self {
            data: xdg_home("XDG_DATA_HOME", ".local/share"),
            config: xdg_home("XDG_CONFIG_HOME", ".config"),
            cache: xdg_home("XDG_CACHE_HOME", ".cache").join("linuxapk"),
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

/// An app installed by Linux.apk, as its desktop entry describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    pub package: String,
    pub name: String,
    pub version: Option<String>,
    /// The session instance the entry runs in (baked into its `Exec`), empty
    /// for the main session.
    pub instance: String,
    /// The largest icon file, if any.
    pub icon: Option<PathBuf>,
}

/// The session instance an entry's `Exec` runs against.
pub fn exec_instance(exec: &str) -> String {
    paths::invocation(&desktop::exec_args(exec))
        .instance
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

/// Our app entries: (package, path, text) for every `dev.linuxapk.<pkg>`
/// desktop file that says it is `pkg`'s.
fn app_entries(d: &Dirs) -> Vec<(String, PathBuf, String)> {
    let Ok(rd) = fs::read_dir(d.applications()) else {
        return vec![];
    };
    rd.flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let pkg = name.strip_prefix(PREFIX)?.strip_suffix(".desktop")?;
            crate::validate::package(pkg).ok()?;
            let text = fs::read_to_string(e.path()).ok()?;
            (desktop::read_key(&text, "X-LinuxApk-Package").as_deref() == Some(pkg))
                .then(|| (pkg.to_string(), e.path(), text))
        })
        .collect()
}

/// Apps with a Linux.apk desktop entry (any instance), sorted by name.
pub fn desktop_apps(d: &Dirs) -> Vec<DesktopApp> {
    let mut v: Vec<DesktopApp> = app_entries(d)
        .into_iter()
        .map(|(pkg, _, t)| DesktopApp {
            name: desktop::read_key(&t, "Name").unwrap_or_else(|| pkg.clone()),
            version: desktop::read_key(&t, "X-LinuxApk-Version"),
            instance: exec_instance(&desktop::read_key(&t, "Exec").unwrap_or_default()),
            icon: largest_icon(d, &format!("{PREFIX}{pkg}")),
            package: pkg,
        })
        .collect();
    v.sort_by_key(|a| (a.name.to_lowercase(), a.package.clone()));
    v
}

/// Repairs the `Exec` of our app entries that can no longer run: their
/// program is gone (Linux.apk moved or was reinstalled elsewhere), or they
/// set variables an older Linux.apk used (`LINUXAPK_SESSION`). They are
/// pointed at `linuxapk` (this installation's), keeping their instance.
/// Returns how many were rewritten.
pub fn refresh_entries(d: &Dirs, linuxapk: &Path) -> usize {
    let mut n = 0;
    for (pkg, path, text) in app_entries(d) {
        let exec = desktop::read_key(&text, "Exec").unwrap_or_default();
        let args = desktop::exec_args(&exec);
        let inv = paths::invocation(&args);
        let missing = inv
            .program
            .is_some_and(|p| Path::new(p).is_absolute() && !Path::new(p).exists());
        if !(missing || inv.foreign_env || inv.program.is_none()) {
            continue;
        }
        let mut run = paths::command(linuxapk, inv.instance);
        run.extend(["run".into(), pkg]);
        let Ok(line) = desktop::exec_line(&run, Some("%U")) else {
            continue;
        };
        let Some(new) = desktop::replace_key(&text, "Exec", &line) else {
            continue;
        };
        match write_atomic(&path, new.as_bytes()) {
            Ok(()) => n += 1,
            Err(e) => eprintln!("linuxapk: {e:#}"),
        }
    }
    if n > 0 {
        update_desktop_database(d);
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_of_exec() {
        for (program, instance) in [("/x/linuxapk", "gui"), ("/a b/linuxapk", "")] {
            let mut run = paths::command(Path::new(program), instance);
            run.extend(["run".into(), "a.b".into()]);
            let exec = desktop::exec_line(&run, Some("%U")).unwrap();
            assert_eq!(exec_instance(&exec), instance);
        }
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
        let root = crate::test_dir("xdg-app-files");
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
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn stale_entries_are_repaired() {
        let root = crate::test_dir("xdg-refresh");
        let d = Dirs {
            data: root.join("share"),
            config: root.join("config"),
            cache: root.join("cache"),
        };
        let linuxapk = root.join("bin/linuxapk");
        fs::create_dir_all(linuxapk.parent().unwrap()).unwrap();
        fs::write(&linuxapk, "").unwrap();
        let entry = |pkg: &str, exec: &str| {
            let p = d.app_desktop(pkg);
            let text =
                format!("[Desktop Entry]\nName={pkg}\nExec={exec}\nX-LinuxApk-Package={pkg}\n");
            write_atomic(&p, text.as_bytes()).unwrap();
            p
        };
        let exec_of = |p: &Path| desktop::read_key(&fs::read_to_string(p).unwrap(), "Exec");
        let mut old = paths::command(Path::new("/gone/linuxapk"), "cli");
        old.insert(1, "LINUXAPK_SESSION=/gone/session".into());
        old.extend(["run".into(), "org.ex.old".into()]);
        let old = entry("org.ex.old", &desktop::exec_line(&old, Some("%U")).unwrap());
        let moved = entry("org.ex.moved", "/gone/linuxapk run org.ex.moved %U");
        let mut fine = paths::command(&linuxapk, "");
        fine.extend(["run".into(), "org.ex.fine".into()]);
        let fine_line = desktop::exec_line(&fine, Some("%U")).unwrap();
        let fine = entry("org.ex.fine", &fine_line);
        let on_path = entry("org.ex.path", "linuxapk run org.ex.path %U");

        assert_eq!(refresh_entries(&d, &linuxapk), 2);
        let mut want = paths::command(&linuxapk, "cli");
        want.extend(["run".into(), "org.ex.old".into()]);
        assert_eq!(
            exec_of(&old),
            Some(desktop::exec_line(&want, Some("%U")).unwrap())
        );
        assert_eq!(exec_instance(&exec_of(&old).unwrap()), "cli");
        let mut want = paths::command(&linuxapk, "");
        want.extend(["run".into(), "org.ex.moved".into()]);
        assert_eq!(
            exec_of(&moved),
            Some(desktop::exec_line(&want, Some("%U")).unwrap())
        );
        assert_eq!(exec_of(&fine), Some(fine_line));
        assert_eq!(
            exec_of(&on_path).as_deref(),
            Some("linuxapk run org.ex.path %U")
        );
        assert_eq!(refresh_entries(&d, &linuxapk), 0);
        fs::remove_dir_all(&root).unwrap();
    }
}
