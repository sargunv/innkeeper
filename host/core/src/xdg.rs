//! Host desktop integration: app desktop entries, their hicolor icons, and the
//! desktop database. Inside the Flatpak the entries in `applications` are the
//! app's own record (in its private data dir); the app menu gets them through
//! the DynamicLauncher portal ([`crate::dynlauncher`]).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::desktop::{self, AppEntry, MANAGER_ICON_SVG, PREFIX};
use crate::dynlauncher::{self, Answer};
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
    /// The app menu is reached through the DynamicLauncher portal (inside the
    /// Flatpak), as `<app id>.<pkg>.desktop`.
    pub portal: Option<String>,
}

/// The `Key=value` an app's record carries when the user declined its
/// app-menu entry, so repairs don't ask again (an install does).
const DECLINED: &str = "X-LinuxApk-Menu";

impl Dirs {
    pub fn from_env() -> Self {
        Self {
            data: xdg_home("XDG_DATA_HOME", ".local/share"),
            config: xdg_home("XDG_CONFIG_HOME", ".config"),
            cache: xdg_home("XDG_CACHE_HOME", ".cache").join("linuxapk"),
            portal: desktop::flatpak_app_id(),
        }
    }

    /// The portal's id for `pkg`'s entry (`<app id>.<pkg>.desktop`).
    fn portal_id(&self, pkg: &str) -> Option<String> {
        Some(format!("{}.{pkg}.desktop", self.portal.as_ref()?))
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
    if d.portal.is_some() {
        return;
    }
    tool("update-desktop-database", &[d.applications().as_os_str()]);
}

fn update_icon_cache(d: &Dirs) {
    let h = d.hicolor();
    // Only if a cache already exists: a stale one hides new icons, but we don't
    // create one where the desktop doesn't use it.
    if d.portal.is_none() && h.join("icon-theme.cache").is_file() {
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

/// Where an app's menu entry went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Menu {
    /// A desktop file in the user's `applications` dir.
    File(PathBuf),
    /// Added through the DynamicLauncher portal, as this id.
    Portal(String),
    /// The user declined the portal's dialog.
    Declined,
}

impl std::fmt::Display for Menu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Menu::File(p) => write!(f, "{}", p.display()),
            Menu::Portal(id) => write!(f, "{id} (through the desktop portal)"),
            Menu::Declined => write!(f, "none (declined)"),
        }
    }
}

/// Installs an app's icons and desktop entry, replacing any previous ones.
/// `icons` are (size, PNG); sizes that aren't hicolor directories are skipped.
/// Inside the Flatpak the entry is the app's record, and the user is asked to
/// add it to the app menu unless the menu already runs it.
pub fn install_app(d: &Dirs, entry: &mut AppEntry, icons: &[(u32, Vec<u8>)]) -> Result<Menu> {
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
    let text = entry.render()?;
    write_atomic(&path, text.as_bytes())?;
    if d.portal.is_some() {
        return add_to_menu(d, &entry.package, &text, false);
    }
    update_icon_cache(d);
    update_desktop_database(d);
    Ok(Menu::File(path))
}

/// The portal's entry for record `text` of `pkg`: unless the app menu
/// already has one that runs the record's command, asks the user to add it.
/// With `repair`, a record whose entry the user declined is left alone.
fn add_to_menu(d: &Dirs, pkg: &str, text: &str, repair: bool) -> Result<Menu> {
    let (Some(app_id), Some(id)) = (d.portal.as_deref(), d.portal_id(pkg)) else {
        bail!("no portal");
    };
    let exec = desktop::read_key(text, "Exec").context("record without Exec")?;
    let want = desktop::exec_args(&exec);
    if let Some(have) = dynlauncher::get(&id)? {
        let runs =
            desktop::read_key(&have, "Exec").and_then(|e| dynlauncher::app_command(&e, app_id));
        if runs.as_ref() == Some(&want) {
            return Ok(Menu::Portal(id));
        }
    } else if repair && desktop::read_key(text, DECLINED).as_deref() == Some("declined") {
        return Ok(Menu::Declined);
    }
    let name = desktop::read_key(text, "Name").unwrap_or_else(|| pkg.to_string());
    // The largest PNG the portal takes (512 px), else the manager's icon.
    let icon = HICOLOR_SIZES
        .iter()
        .rev()
        .filter(|s| **s <= 512)
        .find_map(|s| fs::read(d.icon_path(*s, &format!("{PREFIX}{pkg}"))).ok())
        .unwrap_or_else(|| MANAGER_ICON_SVG.as_bytes().to_vec());
    // The portal sets Name and Icon from its dialog; the record's own keys
    // (DECLINED) stay out.
    let entry: String = text
        .lines()
        .filter(|l| !l.starts_with("Icon=") && !l.starts_with(&format!("{DECLINED}=")))
        .map(|l| format!("{l}\n"))
        .collect();
    let answer = dynlauncher::install(&id, &name, &icon, &entry)?;
    let mut record = text
        .lines()
        .filter(|l| !l.starts_with(&format!("{DECLINED}=")))
        .map(|l| format!("{l}\n"))
        .collect::<String>();
    if answer == Answer::Declined {
        record.push_str(&format!("{DECLINED}=declined\n"));
    }
    if record != text {
        write_atomic(&d.app_desktop(pkg), record.as_bytes())?;
    }
    Ok(match answer {
        Answer::Added => Menu::Portal(id),
        Answer::Declined => Menu::Declined,
    })
}

/// Removes an app's desktop entry and icons. Returns whether anything existed.
pub fn remove_app(d: &Dirs, pkg: &str) -> bool {
    if let Some(id) = d.portal_id(pkg)
        && let Ok(Some(_)) = dynlauncher::get(&id)
        && let Err(e) = dynlauncher::uninstall(&id)
    {
        eprintln!("linuxapk: {e:#}");
    }
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

/// What a launch shows before the app's first frame: its name, splash and
/// icon, from its desktop entry. None if it has no entry.
pub fn launch_look(
    d: &Dirs,
    pkg: &str,
) -> Option<(String, Option<desktop::Splash>, Option<PathBuf>)> {
    let t = fs::read_to_string(d.app_desktop(pkg)).ok()?;
    let name = desktop::read_key(&t, "Name")?;
    let splash = desktop::read_key(&t, "X-LinuxApk-Splash")
        .and_then(|c| desktop::Splash::parse(&c, &desktop::read_key(&t, "X-LinuxApk-Window")?));
    Some((name, splash, largest_icon(d, &format!("{PREFIX}{pkg}"))))
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

/// Repairs our app entries. The `Exec` of one that can no longer run is
/// pointed at `linuxapk` (this installation's command line, keeping its
/// instance): its program is gone (Linux.apk, or its AppImage, moved or was
/// reinstalled elsewhere), it sets variables an older Linux.apk used
/// (`LINUXAPK_SESSION`), or, inside the Flatpak, it runs anything but
/// `linuxapk`. Inside the Flatpak, a record whose app-menu entry is missing or
/// runs another command is offered to the portal again, unless the user
/// declined it. Returns how many were rewritten.
pub fn refresh_entries(d: &Dirs, linuxapk: &[String]) -> usize {
    let mut n = 0;
    for (pkg, path, mut text) in app_entries(d) {
        let exec = desktop::read_key(&text, "Exec").unwrap_or_default();
        let args = desktop::exec_args(&exec);
        let inv = paths::invocation(&args);
        let missing = inv
            .program
            .is_some_and(|p| Path::new(p).is_absolute() && !Path::new(p).exists());
        let elsewhere = d.portal.is_some() && inv.program != linuxapk.first().map(String::as_str);
        if missing || elsewhere || inv.foreign_env || inv.program.is_none() {
            let mut run = paths::command(linuxapk, inv.instance);
            run.extend(["run".into(), pkg.clone()]);
            let Some(new) = desktop::exec_line(&run, Some("%U"))
                .ok()
                .and_then(|line| desktop::replace_key(&text, "Exec", &line))
            else {
                continue;
            };
            match write_atomic(&path, new.as_bytes()) {
                Ok(()) => n += 1,
                Err(e) => eprintln!("linuxapk: {e:#}"),
            }
            text = new;
        }
        if d.portal.is_some()
            && let Err(e) = add_to_menu(d, &pkg, &text, true)
        {
            eprintln!("linuxapk: {pkg}: app menu: {e:#}");
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
            let mut run = paths::command(&[program.into()], instance);
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
            portal: None,
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
            splash: None,
        };
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(48u32.to_be_bytes());
        png.extend(48u32.to_be_bytes());
        // A wrong-size PNG is skipped rather than misfiled.
        let Menu::File(p) =
            install_app(&d, &mut e, &[(48, png.clone()), (64, png.clone())]).unwrap()
        else {
            panic!("not a file");
        };
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
            portal: None,
        };
        // This installation is an AppImage (Prefix::cli).
        let appimage = root.join("Linux.apk.AppImage");
        fs::write(&appimage, "").unwrap();
        let cli = vec![appimage.to_string_lossy().into_owned(), "linuxapk".into()];
        let entry = |pkg: &str, exec: &str| {
            let p = d.app_desktop(pkg);
            let text =
                format!("[Desktop Entry]\nName={pkg}\nExec={exec}\nX-LinuxApk-Package={pkg}\n");
            write_atomic(&p, text.as_bytes()).unwrap();
            p
        };
        let exec_of = |p: &Path| desktop::read_key(&fs::read_to_string(p).unwrap(), "Exec");
        let want = |pkg: &str, instance: &str| {
            let mut run = paths::command(&cli, instance);
            run.extend(["run".into(), pkg.into()]);
            Some(desktop::exec_line(&run, Some("%U")).unwrap())
        };
        let mut old = paths::command(&["/gone/linuxapk".into()], "cli");
        old.insert(1, "LINUXAPK_SESSION=/gone/session".into());
        old.extend(["run".into(), "org.ex.old".into()]);
        let old = entry("org.ex.old", &desktop::exec_line(&old, Some("%U")).unwrap());
        let moved = entry("org.ex.moved", "/gone/linuxapk run org.ex.moved %U");
        let moved_appimage = entry(
            "org.ex.ai",
            "env LINUXAPK_INSTANCE=t /gone/Linux.apk.AppImage linuxapk run org.ex.ai %U",
        );
        let fine = entry("org.ex.fine", &want("org.ex.fine", "").unwrap());
        let on_path = entry("org.ex.path", "linuxapk run org.ex.path %U");

        assert_eq!(refresh_entries(&d, &cli), 3);
        assert_eq!(exec_of(&old), want("org.ex.old", "cli"));
        assert_eq!(exec_instance(&exec_of(&old).unwrap()), "cli");
        assert_eq!(exec_of(&moved), want("org.ex.moved", ""));
        assert_eq!(exec_of(&moved_appimage), want("org.ex.ai", "t"));
        assert_eq!(exec_of(&fine), want("org.ex.fine", ""));
        assert_eq!(
            exec_of(&on_path).as_deref(),
            Some("linuxapk run org.ex.path %U")
        );
        assert_eq!(refresh_entries(&d, &cli), 0);
        fs::remove_dir_all(&root).unwrap();
    }
}
