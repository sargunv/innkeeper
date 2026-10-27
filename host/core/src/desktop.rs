//! Desktop Entry (`.desktop`) generation, escaped per the Desktop Entry
//! Specification 1.5: string escapes (`\s \n \t \r \\`), list separators
//! (`\;`) and the Exec quoting rules (reserved characters, `%%`).

use std::fmt::Write as _;

use anyhow::{Result, bail};

use crate::validate::is_forbidden_char;

/// Desktop-file ID and icon name prefix; `StartupWMClass` uses it too.
pub const PREFIX: &str = "dev.linuxapk.";
pub const INSTALLER_ID: &str = "dev.linuxapk.installer";
/// The manager app (`linuxapk-manager`): its application ID, desktop-file ID and
/// icon name. One segment after the prefix, so no Android package (which
/// has at least two) can collide with it or with `INSTALLER_ID`.
pub const MANAGER_ID: &str = "dev.linuxapk.Manager";
/// The manager's icon (also used where an app has no icon of its own).
pub const MANAGER_ICON_SVG: &str = include_str!("../../manager/data/dev.linuxapk.Manager.svg");

/// Escapes a `string`/`localestring` value. Refuses control characters other
/// than the ones the spec has escapes for (we never write those either, but
/// the escaping must hold on its own).
pub fn escape_value(s: &str) -> Result<String> {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            ' ' if i == 0 => out.push_str("\\s"),
            c if is_forbidden_char(c) => bail!(
                "control character U+{:04X} in a desktop entry value",
                c as u32
            ),
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Escapes one item of a `;`-separated list value and appends the separator.
fn list(items: &[impl AsRef<str>]) -> Result<String> {
    let mut out = String::new();
    for it in items {
        out.push_str(&escape_value(it.as_ref())?.replace(';', "\\;"));
        out.push(';');
    }
    Ok(out)
}

/// Quotes one Exec argument (before the general string escaping, which
/// `exec_line` applies afterwards). Field codes are passed through unquoted by
/// `exec_line`, so every `%` here is literal.
pub fn quote_exec_arg(arg: &str) -> Result<String> {
    if arg.is_empty() {
        return Ok("\"\"".into());
    }
    if arg.chars().any(is_forbidden_char) {
        bail!("control character in an Exec argument");
    }
    const RESERVED: &str = " \t\n\"'\\><~|&;$*?#`()";
    let arg = arg.replace('%', "%%");
    if !arg.chars().any(|c| RESERVED.contains(c)) {
        return Ok(arg);
    }
    let mut q = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            q.push('\\');
        }
        q.push(c);
    }
    q.push('"');
    Ok(q)
}

/// Builds an Exec value from literal arguments plus an optional field code.
pub fn exec_line(args: &[String], field_code: Option<&str>) -> Result<String> {
    let mut parts = args
        .iter()
        .map(|a| quote_exec_arg(a))
        .collect::<Result<Vec<_>>>()?;
    if let Some(f) = field_code {
        parts.push(f.to_string());
    }
    escape_value(&parts.join(" "))
}

/// An installed Android app's launcher entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    pub package: String,
    pub name: String,
    /// (desktop locale, label), e.g. ("pt_BR", "Calculadora").
    pub names: Vec<(String, String)>,
    /// Command prefix; `<package>` and `%U` are appended (`linuxapk run`).
    pub run: Vec<String>,
    pub icon: Option<String>,
    /// `versionName`, recorded as `X-LinuxApk-Version` (omitted if empty).
    pub version: String,
    pub categories: Vec<String>,
    /// Custom URI schemes, declared as `x-scheme-handler/<scheme>`.
    pub schemes: Vec<String>,
    /// Hosts of the app's verified-style `http(s)` deep links. Metadata only:
    /// `http`/`https` are never declared for an app.
    pub web_hosts: Vec<String>,
    /// Who wrote the entry, as `X-LinuxApk-Origin` (`android`: the bridge, for
    /// an app installed inside Android). None: `linuxapk install`.
    pub origin: Option<String>,
}

impl AppEntry {
    pub fn id(&self) -> String {
        format!("{PREFIX}{}", self.package)
    }

    pub fn render(&self) -> Result<String> {
        let mut s = String::from("[Desktop Entry]\nType=Application\nVersion=1.5\n");
        writeln!(s, "Name={}", escape_value(&self.name)?)?;
        for (loc, n) in &self.names {
            // Locale keys are generated from a validated grammar; check again.
            if !loc.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                bail!("bad locale key {loc:?}");
            }
            writeln!(s, "Name[{loc}]={}", escape_value(n)?)?;
        }
        writeln!(s, "Comment=Android app ({})", escape_value(&self.package)?)?;
        if let Some(icon) = &self.icon {
            writeln!(s, "Icon={}", escape_value(icon)?)?;
        }
        let mut run = self.run.clone();
        run.push(self.package.clone());
        writeln!(s, "Exec={}", exec_line(&run, Some("%U"))?)?;
        s.push_str("Terminal=false\n");
        writeln!(s, "StartupWMClass={}", escape_value(&self.id())?)?;
        if !self.categories.is_empty() {
            writeln!(s, "Categories={}", list(&self.categories)?)?;
        }
        if !self.schemes.is_empty() {
            let mimes: Vec<String> = self
                .schemes
                .iter()
                .map(|sc| format!("x-scheme-handler/{sc}"))
                .collect();
            writeln!(s, "MimeType={}", list(&mimes)?)?;
        }
        s.push_str("Keywords=Android;\n");
        writeln!(s, "X-LinuxApk-Package={}", escape_value(&self.package)?)?;
        if !self.version.is_empty() {
            writeln!(s, "X-LinuxApk-Version={}", escape_value(&self.version)?)?;
        }
        if !self.web_hosts.is_empty() {
            writeln!(s, "X-LinuxApk-Web-Hosts={}", list(&self.web_hosts)?)?;
        }
        if let Some(o) = &self.origin {
            writeln!(s, "X-LinuxApk-Origin={}", escape_value(o)?)?;
        }
        Ok(s)
    }
}

/// MIME types handled by `linuxapk setup`'s installer entry.
pub const PACKAGE_MIME_TYPES: &[&str] = &[
    "application/vnd.android.package-archive",
    "application/x-android-apks",
    "application/x-android-xapk",
    "application/x-android-apkm",
];

/// The "Install with Linux.apk" handler for APK files: `manager` (the
/// `linuxapk-manager` command line) opens them on its install page.
pub fn installer_entry(manager: &[String]) -> Result<String> {
    let mut s = String::from("[Desktop Entry]\nType=Application\nVersion=1.5\n");
    s.push_str("Name=Linux.apk Installer\n");
    s.push_str("GenericName=Android app installer\n");
    s.push_str("Comment=Install Android apps on this desktop\n");
    writeln!(s, "Icon={MANAGER_ID}")?;
    writeln!(s, "StartupWMClass={MANAGER_ID}")?;
    writeln!(s, "Exec={}", exec_line(manager, Some("%f"))?)?;
    s.push_str("Terminal=false\nNoDisplay=true\n");
    writeln!(s, "MimeType={}", list(PACKAGE_MIME_TYPES)?)?;
    s.push_str("Categories=Settings;PackageManager;\n");
    Ok(s)
}

/// The manager's own app-menu entry ("Linux.apk").
pub fn manager_entry(cmd: &[String]) -> Result<String> {
    let mut s = String::from("[Desktop Entry]\nType=Application\nVersion=1.5\n");
    s.push_str("Name=Linux.apk\n");
    s.push_str("GenericName=Android App Manager\n");
    s.push_str("Comment=Install, run and remove Android apps\n");
    writeln!(s, "Icon={MANAGER_ID}")?;
    writeln!(s, "Exec={}", exec_line(cmd, None)?)?;
    s.push_str("Terminal=false\nStartupNotify=true\n");
    writeln!(s, "StartupWMClass={MANAGER_ID}")?;
    s.push_str("Categories=System;PackageManager;\n");
    s.push_str("Keywords=Android;APK;\n");
    Ok(s)
}

/// shared-mime-info definitions for the bundle formats (plain APKs already
/// have `application/vnd.android.package-archive`).
pub const MIME_PACKAGE_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!-- Installed and removed by linuxapk setup (dev.linuxapk). -->
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="application/x-android-apks">
    <comment>Android app bundle (APK set)</comment>
    <sub-class-of type="application/zip"/>
    <glob pattern="*.apks"/>
  </mime-type>
  <mime-type type="application/x-android-xapk">
    <comment>Android app bundle (XAPK)</comment>
    <sub-class-of type="application/zip"/>
    <glob pattern="*.xapk"/>
  </mime-type>
  <mime-type type="application/x-android-apkm">
    <comment>Android app bundle (APKM)</comment>
    <sub-class-of type="application/zip"/>
    <glob pattern="*.apkm"/>
  </mime-type>
</mime-info>
"#;

/// Category mapping for `ApplicationInfo.category` / `android:appCategory`.
pub fn categories_for(app_category: Option<i32>, is_game: bool) -> Vec<String> {
    let v: &[&str] = if is_game {
        &["Game"]
    } else {
        match app_category {
            Some(0) => &["Game"],
            Some(1) => &["AudioVideo", "Audio"],
            Some(2) => &["AudioVideo", "Video"],
            Some(3) => &["Graphics"],
            Some(4) => &["Network", "InstantMessaging"],
            Some(5) => &["Network", "News"],
            Some(6) => &["Utility", "Maps"],
            Some(7) => &["Office"],
            Some(8) => &["Utility", "Accessibility"],
            _ => &["Utility"],
        }
    };
    v.iter().map(|s| s.to_string()).collect()
}

/// Reads one key from the `[Desktop Entry]` group (unescaped for the simple
/// escapes; enough for reading back our own files).
pub fn read_key(text: &str, key: &str) -> Option<String> {
    let mut in_group = false;
    for line in text.lines() {
        if line.starts_with('[') {
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some((k, v)) = line.split_once('=')
            && k.trim_end() == key
        {
            let v = v.trim_start();
            let mut out = String::new();
            let mut it = v.chars();
            while let Some(c) = it.next() {
                if c == '\\' {
                    match it.next() {
                        Some('s') => out.push(' '),
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('r') => out.push('\r'),
                        Some(o) => out.push(o),
                        None => {}
                    }
                } else {
                    out.push(c);
                }
            }
            return Some(out);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> AppEntry {
        AppEntry {
            package: "org.wikipedia".into(),
            name: "Wikipedia".into(),
            names: vec![
                ("de".into(), "Wikipedia DE".into()),
                ("pt_BR".into(), "Wikipédia".into()),
            ],
            run: vec!["/opt/linux apk/bin/linuxapk".into(), "run".into()],
            icon: Some("dev.linuxapk.org.wikipedia".into()),
            version: "2.7.50".into(),
            categories: vec!["Utility".into()],
            schemes: vec!["wikipedia".into()],
            web_hosts: vec!["*.wikipedia.org".into()],
            origin: None,
        }
    }

    #[test]
    fn renders_app_entry() {
        let s = entry().render().unwrap();
        assert_eq!(
            s,
            "[Desktop Entry]\nType=Application\nVersion=1.5\nName=Wikipedia\n\
             Name[de]=Wikipedia DE\nName[pt_BR]=Wikipédia\nComment=Android app (org.wikipedia)\n\
             Icon=dev.linuxapk.org.wikipedia\n\
             Exec=\"/opt/linux apk/bin/linuxapk\" run org.wikipedia %U\nTerminal=false\n\
             StartupWMClass=dev.linuxapk.org.wikipedia\nCategories=Utility;\n\
             MimeType=x-scheme-handler/wikipedia;\nKeywords=Android;\n\
             X-LinuxApk-Package=org.wikipedia\nX-LinuxApk-Version=2.7.50\n\
             X-LinuxApk-Web-Hosts=*.wikipedia.org;\n"
        );
        assert!(!s.contains("x-scheme-handler/http"));
    }

    #[test]
    fn no_mimetype_without_schemes() {
        let mut e = entry();
        e.schemes.clear();
        e.web_hosts.clear();
        let s = e.render().unwrap();
        assert!(!s.contains("MimeType="));
        assert!(!s.contains("X-LinuxApk-Web-Hosts"));
    }

    #[test]
    fn value_escaping() {
        assert_eq!(escape_value("a\\b").unwrap(), "a\\\\b");
        assert_eq!(escape_value(" lead").unwrap(), "\\slead");
        assert_eq!(escape_value("mid space").unwrap(), "mid space");
        assert_eq!(escape_value("a\nb\tc\rd").unwrap(), "a\\nb\\tc\\rd");
        assert!(escape_value("a\u{1b}[31m").is_err());
        assert!(escape_value("a\u{0}").is_err());
        assert!(escape_value("x\u{202E}y").is_err());
    }

    #[test]
    fn label_cannot_inject_keys() {
        let mut e = entry();
        e.name = "Calc\nExec=rm -rf ~".into();
        // Validated labels never contain newlines; even if one got here it is
        // escaped onto a single line.
        let s = e.render().unwrap();
        assert!(s.contains("Name=Calc\\nExec=rm -rf ~\n"));
        assert_eq!(s.lines().filter(|l| l.starts_with("Exec=")).count(), 1);
        e.name = "Calc\u{7}".into();
        assert!(e.render().is_err());
    }

    #[test]
    fn exec_quoting() {
        assert_eq!(
            quote_exec_arg("/usr/bin/linuxapk").unwrap(),
            "/usr/bin/linuxapk"
        );
        assert_eq!(quote_exec_arg("/a b/x").unwrap(), "\"/a b/x\"");
        assert_eq!(quote_exec_arg("50%").unwrap(), "50%%");
        assert_eq!(quote_exec_arg("$HOME").unwrap(), "\"\\$HOME\"");
        assert_eq!(quote_exec_arg("a\"b").unwrap(), "\"a\\\"b\"");
        assert_eq!(quote_exec_arg("a`b`").unwrap(), "\"a\\`b\\`\"");
        assert_eq!(quote_exec_arg("").unwrap(), "\"\"");
        assert!(quote_exec_arg("a\nb").is_err());
        // Backslash: quoted (\\) then string-escaped (\\\\).
        assert_eq!(exec_line(&["a\\b".into()], None).unwrap(), "\"a\\\\\\\\b\"");
        assert_eq!(
            exec_line(
                &[
                    "env".into(),
                    "LINUXAPK_INSTANCE=cli".into(),
                    "/x/linuxapk".into()
                ],
                Some("%f")
            )
            .unwrap(),
            "env LINUXAPK_INSTANCE=cli /x/linuxapk %f"
        );
    }

    #[test]
    fn list_escaping() {
        assert_eq!(list(&["a;b", "c"]).unwrap(), "a\\;b;c;");
    }

    #[test]
    fn installer() {
        let g = installer_entry(&["linuxapk-manager".into()]).unwrap();
        assert!(g.contains("Exec=linuxapk-manager %f\n"));
        assert!(g.contains("Icon=dev.linuxapk.Manager\n"));
        assert!(g.contains("MimeType=application/vnd.android.package-archive;"));
        assert!(g.contains("NoDisplay=true"));
        let m = manager_entry(&[
            "env".into(),
            "LINUXAPK_INSTANCE=gui".into(),
            "/x/linuxapk-manager".into(),
        ])
        .unwrap();
        assert!(m.contains("Exec=env LINUXAPK_INSTANCE=gui /x/linuxapk-manager\n"));
        assert!(!m.contains("NoDisplay"));
        assert!(!m.contains("MimeType"));
    }

    #[test]
    fn mime_xml_is_well_formed_enough() {
        // update-mime-database rejects "--" inside comments (and exits 0).
        for c in MIME_PACKAGE_XML.split("<!--").skip(1) {
            let body = c.split("-->").next().unwrap();
            assert!(!body.contains("--"), "{body}");
        }
    }

    #[test]
    fn categories() {
        assert_eq!(categories_for(None, true), ["Game"]);
        assert_eq!(categories_for(Some(5), false), ["Network", "News"]);
        assert_eq!(categories_for(Some(-1), false), ["Utility"]);
        assert_eq!(categories_for(Some(99), false), ["Utility"]);
    }

    #[test]
    fn reads_back_keys() {
        let s = entry().render().unwrap();
        assert_eq!(read_key(&s, "Name").as_deref(), Some("Wikipedia"));
        assert_eq!(
            read_key(&s, "X-LinuxApk-Package").as_deref(),
            Some("org.wikipedia")
        );
        assert_eq!(read_key("[Other]\nName=x\n", "Name"), None);
        assert_eq!(
            read_key("[Desktop Entry]\nName=\\sa\\\\b\n", "Name").as_deref(),
            Some(" a\\b")
        );
    }
}
