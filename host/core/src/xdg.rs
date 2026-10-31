//! Host desktop integration: app desktop entries and their hicolor icons. The
//! entries in `applications` are the app's own record (in the Flatpak's
//! private data dir); the app menu gets them through the DynamicLauncher
//! portal ([`crate::dynlauncher`]).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::desktop::{self, AppEntry, MANAGER_ICON_SVG, PREFIX};
use crate::dynlauncher::{self, Answer};
use crate::paths;
use innkeeper_profile::paths::xdg_home;

/// Sizes we render icons at; all are hicolor theme directories.
pub const ICON_SIZES: &[u32] = &[48, 64, 128, 256, 512];
/// hicolor's fixed-size directories (for APK PNG fallbacks).
const HICOLOR_SIZES: &[u32] = &[16, 22, 24, 32, 36, 48, 64, 72, 96, 128, 192, 256, 512];

pub struct Dirs {
    pub data: PathBuf,
    pub config: PathBuf,
    pub cache: PathBuf,
    /// The app id whose DynamicLauncher portal entries make the app menu, as
    /// `<app id>.<pkg>.desktop`. None only in tests: records without a menu.
    pub portal: Option<String>,
}

/// The `Key=value` an app's record carries when the user declined its
/// app-menu entry, so repairs don't ask again (an install does).
const DECLINED: &str = "X-Innkeeper-Menu";

impl Dirs {
    pub fn from_env() -> Self {
        Self {
            data: xdg_home("XDG_DATA_HOME", ".local/share"),
            config: xdg_home("XDG_CONFIG_HOME", ".config"),
            cache: xdg_home("XDG_CACHE_HOME", ".cache").join("innkeeper"),
            portal: Some(desktop::flatpak_app_id()),
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
    /// Added through the DynamicLauncher portal, as this id.
    Portal(String),
    /// The user declined the portal's dialog.
    Declined,
    /// No app menu ([`Dirs::portal`] is None).
    None,
}

impl std::fmt::Display for Menu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Menu::Portal(id) => write!(f, "{id} (through the desktop portal)"),
            Menu::Declined => write!(f, "none (declined)"),
            Menu::None => write!(f, "none"),
        }
    }
}

/// Writes an app's record and icons, replacing any previous ones, and
/// returns the record. `icons` are (size, PNG); sizes that aren't hicolor
/// directories are skipped.
pub fn write_record(d: &Dirs, entry: &mut AppEntry, icons: &[(u32, Vec<u8>)]) -> Result<String> {
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
    let text = entry.render()?;
    write_atomic(&d.app_desktop(&entry.package), text.as_bytes())?;
    Ok(text)
}

/// The portal's entry for record `text` of `pkg`: unless the app menu
/// already has one that runs the record's command, asks the user to add it.
/// With `repair`, a record whose entry the user declined is left alone.
pub fn add_to_menu(d: &Dirs, pkg: &str, text: &str, repair: bool) -> Result<Menu> {
    let (Some(app_id), Some(id)) = (d.portal.as_deref(), d.portal_id(pkg)) else {
        return Ok(Menu::None);
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
    let record = menu_record(text, answer == Answer::Declined);
    if record != text {
        write_atomic(&d.app_desktop(pkg), record.as_bytes())?;
    }
    Ok(match answer {
        Answer::Added => Menu::Portal(id),
        Answer::Declined => Menu::Declined,
    })
}

fn menu_record(text: &str, declined: bool) -> String {
    let mut record = String::new();
    for line in text
        .lines()
        .filter(|l| !l.starts_with(&format!("{DECLINED}=")))
    {
        record.push_str(line);
        record.push('\n');
        if declined && line == "[Desktop Entry]" {
            record.push_str(&format!("{DECLINED}=declined\n"));
        }
    }
    record
}

/// Removes an app's desktop entry and icons. Returns whether anything existed.
pub fn remove_app(d: &Dirs, pkg: &str) -> bool {
    if let Some(id) = d.portal_id(pkg)
        && let Ok(Some(_)) = dynlauncher::get(&id)
        && let Err(e) = dynlauncher::uninstall(&id)
    {
        eprintln!("innkeeper: {e:#}");
    }
    let had_entry = fs::remove_file(d.app_desktop(pkg)).is_ok();
    remove_icons(d, &format!("{PREFIX}{pkg}")) > 0 || had_entry
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
    let splash = desktop::read_key(&t, "X-Innkeeper-Splash")
        .and_then(|c| desktop::Splash::parse(&c, &desktop::read_key(&t, "X-Innkeeper-Window")?));
    Some((name, splash, largest_icon(d, &format!("{PREFIX}{pkg}"))))
}

/// An app installed by Innkeeper, as its desktop entry describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopApp {
    pub package: String,
    pub name: String,
    pub version: Option<String>,
    /// The profile the entry runs in (`X-Innkeeper-Profile`), if it names
    /// one.
    pub profile: Option<String>,
    /// The largest icon file, if any.
    pub icon: Option<PathBuf>,
}

/// The largest hicolor PNG for `id` (`dev.sargunv.innkeeper.<pkg>`).
pub fn largest_icon(d: &Dirs, id: &str) -> Option<PathBuf> {
    HICOLOR_SIZES
        .iter()
        .rev()
        .map(|n| d.icon_path(*n, id))
        .find(|p| p.is_file())
}

/// Our app entries: (package, path, text) for every `dev.sargunv.innkeeper.<pkg>`
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
            (desktop::read_key(&text, "X-Innkeeper-Package").as_deref() == Some(pkg))
                .then(|| (pkg.to_string(), e.path(), text))
        })
        .collect()
}

/// Apps with a Innkeeper desktop entry (any profile), sorted by name.
pub fn desktop_apps(d: &Dirs) -> Vec<DesktopApp> {
    let mut v: Vec<DesktopApp> = app_entries(d)
        .into_iter()
        .map(|(pkg, _, t)| DesktopApp {
            name: desktop::read_key(&t, "Name").unwrap_or_else(|| pkg.clone()),
            version: desktop::read_key(&t, "X-Innkeeper-Version"),
            profile: desktop::read_key(&t, "X-Innkeeper-Profile"),
            icon: largest_icon(d, &format!("{PREFIX}{pkg}")),
            package: pkg,
        })
        .collect();
    v.sort_by_key(|a| (a.name.to_lowercase(), a.package.clone()));
    v
}

/// Points the entries of profile `from`'s apps at profile `to` (renamed).
/// Returns how many were rewritten.
pub fn move_entries(d: &Dirs, from: &str, to: &str) -> usize {
    rewrite_entries(d, (from, to))
}

/// The program desktop entries run, which the DynamicLauncher portal runs as
/// `flatpak run --command=innkeeper`.
pub const INNKEEPER: &str = "innkeeper";

fn rewrite_entries(d: &Dirs, (from, to): (&str, &str)) -> usize {
    let mut n = 0;
    for (pkg, path, mut text) in app_entries(d) {
        if desktop::read_key(&text, "X-Innkeeper-Profile").as_deref() == Some(from)
            && let Ok(to) = innkeeper_profile::profile::ProfileName::new(to)
        {
            let run = paths::cli(&to, &["run", &pkg]);
            let Some(new) = desktop::exec_line(&run, Some("%U"))
                .ok()
                .and_then(|line| desktop::replace_key(&text, "Exec", &line))
                .and_then(|t| desktop::replace_key(&t, "X-Innkeeper-Profile", &to))
            else {
                continue;
            };
            match write_atomic(&path, new.as_bytes()) {
                Ok(()) => n += 1,
                Err(e) => eprintln!("innkeeper: {e:#}"),
            }
            text = new;
        }
        if let Err(e) = add_to_menu(d, &pkg, &text, true) {
            eprintln!("innkeeper: {pkg}: app menu: {e:#}");
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declines_are_recorded_in_the_record() {
        let entry = "[Desktop Entry]\nName=App\nExec=innkeeper run a.b %U\n";
        let record = menu_record(entry, true);
        // The repair path reads exactly this key before requesting installation again.
        assert_eq!(
            desktop::read_key(&record, DECLINED).as_deref(),
            Some("declined")
        );
        assert_eq!(menu_record(&record, false), entry);
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
            profile: innkeeper_profile::profile::ProfileName::new("work").unwrap(),
            icon: None,
            version: String::new(),
            categories: vec!["Utility".into()],
            schemes: vec![],
            splash: None,
        };
        let mut png = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR".to_vec();
        png.extend(48u32.to_be_bytes());
        png.extend(48u32.to_be_bytes());
        // A wrong-size PNG is skipped rather than misfiled.
        write_record(&d, &mut e, &[(48, png.clone()), (64, png.clone())]).unwrap();
        let p = d.app_desktop("org.example.app");
        assert!(p.ends_with("applications/dev.sargunv.innkeeper.org.example.app.desktop"));
        assert!(
            d.icon_path(48, "dev.sargunv.innkeeper.org.example.app")
                .is_file()
        );
        assert!(
            !d.icon_path(64, "dev.sargunv.innkeeper.org.example.app")
                .exists()
        );
        assert_eq!(
            e.icon.as_deref(),
            Some("dev.sargunv.innkeeper.org.example.app")
        );
        assert_eq!(app_label(&d, "org.example.app").as_deref(), Some("Example"));
        let apps = desktop_apps(&d);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].package, "org.example.app");
        assert_eq!(apps[0].profile.as_deref(), Some("work"));
        assert_eq!(
            apps[0].icon.as_deref(),
            Some(
                d.icon_path(48, "dev.sargunv.innkeeper.org.example.app")
                    .as_path()
            )
        );
        // Reinstall replaces; remove cleans up.
        write_record(&d, &mut e, &[]).unwrap();
        assert!(
            !d.icon_path(48, "dev.sargunv.innkeeper.org.example.app")
                .exists()
        );
        assert_eq!(e.icon.as_deref(), Some("android-package-archive"));
        assert!(remove_app(&d, "org.example.app"));
        assert!(!p.exists());
        assert!(!remove_app(&d, "org.example.app"));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn renamed_profiles_entries_follow() {
        let root = crate::test_dir("xdg-rename");
        let d = Dirs {
            data: root.join("share"),
            config: root.join("config"),
            cache: root.join("cache"),
            portal: None,
        };
        let entry = |pkg: &str, profile: &str| AppEntry {
            package: pkg.into(),
            name: pkg.into(),
            names: vec![],
            profile: innkeeper_profile::profile::ProfileName::new(profile).unwrap(),
            icon: None,
            version: String::new(),
            categories: vec![],
            schemes: vec![],
            splash: None,
        };
        write_record(&d, &mut entry("org.ex.a", "p"), &[]).unwrap();
        write_record(&d, &mut entry("org.ex.b", "other"), &[]).unwrap();
        let text = |pkg: &str| fs::read_to_string(d.app_desktop(pkg)).unwrap();
        let before_b = text("org.ex.b");
        assert_eq!(move_entries(&d, "p", "q"), 1);
        let mut want = entry("org.ex.a", "q");
        want.icon = Some("android-package-archive".into());
        assert_eq!(text("org.ex.a"), want.render().unwrap());
        assert_eq!(text("org.ex.b"), before_b);
        assert_eq!(
            desktop_apps(&d)
                .iter()
                .map(|a| a.profile.as_deref())
                .collect::<Vec<_>>(),
            [Some("q"), Some("other")]
        );
        fs::remove_dir_all(&root).unwrap();
    }
}
