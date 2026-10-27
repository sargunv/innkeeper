//! APK metadata from the SDK's `aapt2` (`dump badging` and `dump xmltree` of
//! `AndroidManifest.xml`). aapt2 runs as a plain child process with argv (never
//! a shell) under a timeout; everything taken from its output is untrusted and
//! validated before use.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::{proc, validate};

/// The oldest Android API level a session runs (Android 16; the image is 17).
pub const SESSION_MIN_SDK: u32 = 36;

/// What we use from `aapt2 dump badging`.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Badging {
    pub package: String,
    pub version_code: String,
    pub version_name: String,
    /// `split='config.x86_64'` for split APKs; empty for the base.
    pub split: String,
    /// Default-locale label.
    pub label: String,
    /// (aapt2 locale tag, label), e.g. ("pt-BR", "Calculadora").
    pub labels: Vec<(String, String)>,
    /// (density, path inside the APK) from `application-icon-<density>`.
    pub icons: Vec<(u32, String)>,
    pub native_code: Vec<String>,
    /// `uses-permission` names (sorted, deduplicated, as declared).
    pub permissions: Vec<String>,
}

/// `key='value'` inside a `package:` line. aapt2 doesn't escape `'` there, so
/// a value containing one is cut short (harmless: the fields we read are
/// validated, and only `versionName` is free-form).
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!(" {key}='");
    let start = line.find(&pat)? + pat.len();
    let len = line[start..].find('\'')?;
    Some(&line[start..start + len])
}

/// The value of a one-value line, `prefix:'value'`. aapt2's
/// `normalizeForOutput` escapes only `\\`, `\n` and `"`; the value runs to
/// the line's last `'`.
fn line_value(v: &str) -> Option<String> {
    let inner = v.strip_prefix('\'')?.strip_suffix('\'')?;
    let mut out = String::with_capacity(inner.len());
    let mut it = inner.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

pub fn parse_badging(text: &str) -> Badging {
    let mut b = Badging::default();
    for line in text.lines() {
        if line.starts_with("package:") {
            let f = |k| field(line, k).unwrap_or_default().to_string();
            b.package = f("name");
            b.version_code = f("versionCode");
            b.version_name = f("versionName");
            b.split = f("split");
        } else if let Some(rest) = line.strip_prefix("application-label") {
            let Some((tag, val)) = rest.split_once(':') else {
                continue;
            };
            let Some(v) = line_value(val) else { continue };
            if tag.is_empty() {
                b.label = v;
            } else if let Some(loc) = tag.strip_prefix('-') {
                b.labels.push((loc.to_string(), v));
            }
        } else if let Some(rest) = line.strip_prefix("application-icon-") {
            if let Some((d, val)) = rest.split_once(':')
                && let Ok(d) = d.parse::<u32>()
                && let Some(v) = line_value(val)
            {
                b.icons.push((d, v));
            }
        } else if (line.starts_with("uses-permission:")
            || line.starts_with("uses-permission-sdk-23:"))
            && let Some(name) = field(line, "name")
        {
            // Requested only up to an old SDK (`maxSdkVersion`): doesn't apply
            // in the session, which runs a current Android.
            let max: Option<u32> = field(line, "maxSdkVersion").and_then(|v| v.parse().ok());
            if max.is_none_or(|m| m >= SESSION_MIN_SDK) {
                b.permissions.push(name.to_string());
            }
        } else if let Some(rest) = line.strip_prefix("native-code:") {
            b.native_code = rest
                .split_whitespace()
                .map(|s| s.trim_matches('\'').to_string())
                .collect();
        }
    }
    b.permissions.sort();
    b.permissions.dedup();
    b
}

/// A node of `aapt2 dump xmltree` output.
#[derive(Debug, Default, Clone)]
pub struct Element {
    pub name: String,
    /// (local attribute name, value) with string values unquoted.
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Element>,
}

impl Element {
    pub fn attr(&self, local: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == local)
            .map(|(_, v)| v.as_str())
    }

    pub fn find<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Element> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }
}

/// `A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="x" (Raw: "x")`
fn parse_attr(s: &str) -> Option<(String, String)> {
    let (name, val) = s.split_once('=')?;
    let name = name.split('(').next()?;
    let local = name.rsplit(':').next()?.to_string();
    let val = if let Some(inner) = val.strip_prefix('"') {
        // `"v" (Raw: "v")` or `"v"`. A value containing the delimiter itself
        // parses as garbage, which then fails validation.
        match inner.rfind("\" (Raw: \"") {
            Some(i) => inner[..i].to_string(),
            None => inner.strip_suffix('"').unwrap_or(inner).to_string(),
        }
    } else {
        val.to_string()
    };
    Some((local, val))
}

/// Parses `aapt2 dump xmltree` text into its root element (`manifest`).
pub fn parse_xmltree(text: &str) -> Result<Element> {
    // Stack of (indent, element); a synthetic root at indent -1.
    let mut stack: Vec<(isize, Element)> = vec![(-1, Element::default())];
    for line in text.lines() {
        let indent = (line.len() - line.trim_start().len()) as isize;
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("E: ") {
            while stack.len() > 1 && stack.last().unwrap().0 >= indent {
                let (_, e) = stack.pop().unwrap();
                stack.last_mut().unwrap().1.children.push(e);
            }
            let name = rest.split(" (line=").next().unwrap_or(rest).to_string();
            stack.push((
                indent,
                Element {
                    name,
                    ..Default::default()
                },
            ));
        } else if let Some(rest) = t.strip_prefix("A: ") {
            while stack.len() > 1 && stack.last().unwrap().0 >= indent {
                let (_, e) = stack.pop().unwrap();
                stack.last_mut().unwrap().1.children.push(e);
            }
            if let Some(a) = parse_attr(rest) {
                stack.last_mut().unwrap().1.attrs.push(a);
            }
        }
    }
    while stack.len() > 1 {
        let (_, e) = stack.pop().unwrap();
        stack.last_mut().unwrap().1.children.push(e);
    }
    let root = stack.pop().unwrap().1;
    root.children
        .into_iter()
        .find(|e| e.name == "manifest")
        .context("no <manifest> in aapt2 xmltree output")
}

/// Desktop-relevant facts from the manifest.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ManifestInfo {
    pub package: String,
    /// `android:appCategory` (`ApplicationInfo.CATEGORY_*`).
    pub app_category: Option<i32>,
    pub is_game: bool,
    /// Custom URI schemes of exported, browsable VIEW filters (sorted, valid).
    pub schemes: Vec<String>,
    /// Hosts of exported, browsable `http(s)` VIEW filters (sorted, valid).
    pub web_hosts: Vec<String>,
}

pub fn manifest_info(root: &Element) -> ManifestInfo {
    let mut m = ManifestInfo {
        package: root.attr("package").unwrap_or_default().to_string(),
        ..Default::default()
    };
    let Some(app) = root.find("application").next() else {
        return m;
    };
    m.app_category = app.attr("appCategory").and_then(parse_int);
    m.is_game = app.attr("isGame") == Some("true");
    for comp in app
        .children
        .iter()
        .filter(|c| c.name == "activity" || c.name == "activity-alias")
    {
        if comp.attr("exported") == Some("false") {
            continue;
        }
        for f in comp.find("intent-filter") {
            let has = |el: &str, v: &str| f.find(el).any(|e| e.attr("name") == Some(v));
            if !has("action", "android.intent.action.VIEW")
                || !has("category", "android.intent.category.BROWSABLE")
            {
                continue;
            }
            let data: Vec<&Element> = f.find("data").collect();
            // An intent with only a URI doesn't match a filter that wants a type.
            if data.iter().any(|d| d.attr("mimeType").is_some()) {
                continue;
            }
            // <data> elements of one filter combine: every scheme x every host.
            let schemes: Vec<&str> = data.iter().filter_map(|d| d.attr("scheme")).collect();
            let hosts: Vec<&str> = data.iter().filter_map(|d| d.attr("host")).collect();
            for s in &schemes {
                if validate::custom_scheme(s) {
                    m.schemes.push(s.to_string());
                } else if *s == "http" || *s == "https" {
                    m.web_hosts.extend(
                        hosts
                            .iter()
                            .filter(|h| validate::web_host(h))
                            .map(|h| h.to_string()),
                    );
                }
            }
        }
    }
    m.schemes.sort();
    m.schemes.dedup();
    m.web_hosts.sort();
    m.web_hosts.dedup();
    m
}

fn parse_int(v: &str) -> Option<i32> {
    match v.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16).ok().map(|u| u as i32),
        None => v.parse().ok(),
    }
}

/// Finds the SDK's aapt2: `$LINUXAPK_AAPT2`, the newest
/// `$ANDROID_HOME/build-tools/*/aapt2` (default SDK `~/Android/Sdk`), or
/// `aapt2` on `PATH`.
pub fn find_aapt2() -> Result<PathBuf> {
    if let Some(p) = env::var_os("LINUXAPK_AAPT2") {
        return Ok(PathBuf::from(p));
    }
    let sdk = env::var_os("ANDROID_HOME")
        .or_else(|| env::var_os("ANDROID_SDK_ROOT"))
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join("Android/Sdk")));
    if let Some(sdk) = sdk
        && let Ok(rd) = std::fs::read_dir(sdk.join("build-tools"))
    {
        let mut v: Vec<(Vec<u32>, PathBuf)> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path().join("aapt2");
                let ver = e
                    .file_name()
                    .to_string_lossy()
                    .split('.')
                    .map(|s| s.parse().unwrap_or(0))
                    .collect();
                p.is_file().then_some((ver, p))
            })
            .collect();
        v.sort();
        if let Some((_, p)) = v.pop() {
            return Ok(p);
        }
    }
    if let Some(p) = proc::which("aapt2") {
        return Ok(p);
    }
    bail!("aapt2 not found: install the Android SDK build-tools or set LINUXAPK_AAPT2")
}

/// Runs aapt2 with a time limit, returning stdout.
fn run_limited(cmd: &mut Command, limit: Duration) -> Result<String> {
    let o = proc::run(cmd, Stdio::null(), limit)?;
    if !o.ok() {
        bail!(
            "{:?} failed: {}",
            cmd.get_program(),
            validate::truncate(o.stderr.trim(), 400)
        );
    }
    Ok(o.text())
}

/// Density of a resource configuration (`xxxhdpi`, `hdpi-v4`), if it has one
/// that a bitmap icon can be ranked by.
fn config_density(cfg: &str) -> Option<u32> {
    cfg.split('-').find_map(|q| match q {
        "ldpi" => Some(120),
        "mdpi" => Some(160),
        "tvdpi" => Some(213),
        "hdpi" => Some(240),
        "xhdpi" => Some(320),
        "xxhdpi" => Some(480),
        "xxxhdpi" => Some(640),
        _ => q.strip_suffix("dpi").and_then(|d| d.parse().ok()),
    })
}

/// The bitmap (PNG, WebP) variants of the resource whose files include
/// `path`, as (density, path inside the APK), from `aapt2 dump resources`.
/// For a launcher icon that is an adaptive (XML) icon on API 26+, these are
/// the legacy bitmaps older Android versions use.
pub fn bitmap_variants(resources: &str, path: &str) -> Vec<(u32, String)> {
    let mut block: Vec<(String, String)> = vec![];
    let mut found = false;
    let mut out = vec![];
    let mut flush = |block: &mut Vec<(String, String)>, found: &mut bool| {
        if *found {
            for (cfg, p) in block.iter() {
                if (p.ends_with(".png") || p.ends_with(".webp"))
                    && let Some(d) = config_density(cfg)
                {
                    out.push((d, p.clone()));
                }
            }
        }
        block.clear();
        *found = false;
    };
    for line in resources.lines() {
        let t = line.trim_start();
        if t.starts_with("resource ") || t.starts_with("type ") || t.starts_with("Package ") {
            flush(&mut block, &mut found);
            continue;
        }
        // `(xxxhdpi) (file) res/o-.png type=PNG`
        let Some(rest) = t.strip_prefix('(') else {
            continue;
        };
        let Some((cfg, rest)) = rest.split_once(") (file) ") else {
            continue;
        };
        let file = rest.split(" type=").next().unwrap_or(rest).to_string();
        if file == path {
            found = true;
        }
        block.push((cfg.to_string(), file));
    }
    flush(&mut block, &mut found);
    out.sort();
    out.dedup();
    out
}

pub struct Aapt2(PathBuf);

impl Aapt2 {
    pub fn find() -> Result<Self> {
        Ok(Self(find_aapt2()?))
    }

    pub fn badging(&self, apk: &Path) -> Result<Badging> {
        let out = run_limited(
            Command::new(&self.0)
                .args(["dump", "badging", "--include-meta-data"])
                .arg(apk),
            Duration::from_secs(120),
        )
        .with_context(|| format!("aapt2 dump badging {}", apk.display()))?;
        Ok(parse_badging(&out))
    }

    /// `aapt2 dump resources` (the resource table, as text).
    pub fn resources(&self, apk: &Path) -> Result<String> {
        run_limited(
            Command::new(&self.0).args(["dump", "resources"]).arg(apk),
            Duration::from_secs(120),
        )
        .with_context(|| format!("aapt2 dump resources {}", apk.display()))
    }

    pub fn manifest(&self, apk: &Path) -> Result<ManifestInfo> {
        let out = run_limited(
            Command::new(&self.0)
                .args(["dump", "xmltree", "--file", "AndroidManifest.xml"])
                .arg(apk),
            Duration::from_secs(120),
        )
        .with_context(|| format!("aapt2 dump xmltree {}", apk.display()))?;
        Ok(manifest_info(&parse_xmltree(&out)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn badging_lines() {
        let b = parse_badging(
            "package: name='com.example.app' versionCode='53' versionName='3.2 beta' platformBuildVersionName='14'\n\
             application-label:'It's \\\"X\\\" \\\\o/'\n\
             application-label-de:'Rechner'\n\
             application-label-pt-BR:'Calculadora'\n\
             application-icon-160:'res/mipmap-mdpi-v4/ic_launcher.png'\n\
             application-icon-640:'res/dT.xml'\n\
             native-code: 'arm64-v8a' 'x86_64'\n\
             uses-permission: name='android.permission.INTERNET'\n\
             uses-permission: name='android.permission.CAMERA' maxSdkVersion='40'\n\
             uses-permission: name='android.permission.GET_ACCOUNTS' maxSdkVersion='22'\n\
             uses-permission-sdk-23: name='android.permission.INTERNET'\n",
        );
        assert_eq!(
            b.permissions,
            ["android.permission.CAMERA", "android.permission.INTERNET"]
        );
        assert_eq!(b.package, "com.example.app");
        assert_eq!(b.version_code, "53");
        assert_eq!(b.version_name, "3.2 beta");
        assert_eq!(b.split, "");
        assert_eq!(b.label, "It's \"X\" \\o/");
        assert_eq!(
            b.labels,
            [
                ("de".into(), "Rechner".into()),
                ("pt-BR".into(), "Calculadora".into())
            ]
        );
        assert_eq!(b.icons[1], (640, "res/dT.xml".into()));
        assert_eq!(b.native_code, ["arm64-v8a", "x86_64"]);
        let s = parse_badging(
            "package: name='com.example.app' versionCode='1' split='config.x86_64'\n",
        );
        assert_eq!(s.split, "config.x86_64");
    }

    #[test]
    fn icon_bitmaps() {
        let t = "Package name=x id=7f\n  type mipmap id=0f entryCount=2\n\
                 \x20   resource 0x7f0f0000 mipmap/ic_launcher\n\
                 \x20     (mdpi) (file) res/9w.png type=PNG\n\
                 \x20     (xxxhdpi-v4) (file) res/o-.webp type=UNKNOWN\n\
                 \x20     (anydpi-v26) (file) res/BW.xml type=XML\n\
                 \x20   resource 0x7f0f0001 mipmap/ic_round\n\
                 \x20     (xxhdpi) (file) res/r.png type=PNG\n";
        assert_eq!(
            bitmap_variants(t, "res/BW.xml"),
            [(160, "res/9w.png".into()), (640, "res/o-.webp".into())]
        );
        assert!(bitmap_variants(t, "res/none.xml").is_empty());
    }

    #[test]
    fn xmltree_filters() {
        let text = r#"N: android=http://schemas.android.com/apk/res/android (line=2)
  E: manifest (line=2)
    A: package="com.example.app" (Raw: "com.example.app")
      E: application (line=10)
        A: http://schemas.android.com/apk/res/android:appCategory(0x01010545)=5
          E: activity (line=12)
            A: http://schemas.android.com/apk/res/android:name(0x01010003)="com.example.Main" (Raw: "com.example.Main")
              E: intent-filter (line=13)
                  E: action (line=14)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.action.VIEW" (Raw: "android.intent.action.VIEW")
                  E: category (line=15)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.category.BROWSABLE" (Raw: "android.intent.category.BROWSABLE")
                  E: data (line=16)
                    A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="https" (Raw: "https")
                  E: data (line=17)
                    A: http://schemas.android.com/apk/res/android:host(0x01010028)="*.example.org" (Raw: "*.example.org")
                  E: data (line=18)
                    A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="exmpl" (Raw: "exmpl")
                  E: data (line=19)
                    A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="bad;scheme" (Raw: "bad;scheme")
              E: intent-filter (line=20)
                  E: action (line=21)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.action.VIEW" (Raw: "android.intent.action.VIEW")
                  E: data (line=22)
                    A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="notbrowsable" (Raw: "notbrowsable")
          E: activity (line=30)
            A: http://schemas.android.com/apk/res/android:exported(0x01010010)=false
              E: intent-filter (line=31)
                  E: action (line=32)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.action.VIEW" (Raw: "android.intent.action.VIEW")
                  E: category (line=33)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.category.BROWSABLE" (Raw: "android.intent.category.BROWSABLE")
                  E: data (line=34)
                    A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="private" (Raw: "private")
          E: activity-alias (line=40)
              E: intent-filter (line=41)
                  E: action (line=42)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.action.VIEW" (Raw: "android.intent.action.VIEW")
                  E: category (line=43)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.category.BROWSABLE" (Raw: "android.intent.category.BROWSABLE")
                  E: data (line=44)
                    A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="content" (Raw: "content")
                    A: http://schemas.android.com/apk/res/android:mimeType(0x01010026)="text/plain" (Raw: "text/plain")
                  E: data (line=45)
                    A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="typed" (Raw: "typed")
          E: receiver (line=50)
              E: intent-filter (line=51)
                  E: action (line=52)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.action.VIEW" (Raw: "android.intent.action.VIEW")
                  E: category (line=53)
                    A: http://schemas.android.com/apk/res/android:name(0x01010003)="android.intent.category.BROWSABLE" (Raw: "android.intent.category.BROWSABLE")
                  E: data (line=54)
                    A: http://schemas.android.com/apk/res/android:scheme(0x01010027)="receiver" (Raw: "receiver")
"#;
        let m = manifest_info(&parse_xmltree(text).unwrap());
        assert_eq!(m.package, "com.example.app");
        assert_eq!(m.app_category, Some(5));
        assert!(!m.is_game);
        assert_eq!(m.schemes, ["exmpl"]);
        assert_eq!(m.web_hosts, ["*.example.org"]);
    }

    /// Corpus fixtures (not committed): skipped unless the corpus and aapt2
    /// are present. `LINUXAPK_CORPUS` overrides the corpus directory.
    fn corpus(name: &str) -> Option<(Aapt2, PathBuf)> {
        let dir = env::var_os("LINUXAPK_CORPUS")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var("HOME").unwrap_or_default())
                    .join("Projects/linux.apk-wt/compat/out/corpus")
            });
        let p = dir.join(name);
        let a = Aapt2::find().ok()?;
        if !p.is_file() {
            eprintln!("skipping: no corpus APK {}", p.display());
            return None;
        }
        Some((a, p))
    }

    #[test]
    fn corpus_wikipedia() {
        let Some((a, apk)) = corpus("org.wikipedia_50612.apk") else {
            return;
        };
        let b = a.badging(&apk).unwrap();
        assert_eq!(b.package, "org.wikipedia");
        assert_eq!(b.label, "Wikipedia");
        assert!(b.native_code.iter().any(|c| c == "x86_64"));
        let m = a.manifest(&apk).unwrap();
        assert_eq!(m.package, "org.wikipedia");
        assert_eq!(m.schemes, ["wikipedia"]);
        assert!(
            m.web_hosts.iter().any(|h| h == "*.wikipedia.org"),
            "{:?}",
            m.web_hosts
        );
    }

    #[test]
    fn corpus_opencalc() {
        let Some((a, apk)) = corpus("com.darkempire78.opencalculator_53.apk") else {
            return;
        };
        let b = a.badging(&apk).unwrap();
        assert_eq!(b.package, "com.darkempire78.opencalculator");
        assert_eq!(b.label, "Calculator");
        assert!(b.labels.contains(&("de".into(), "Rechner".into())));
        let m = a.manifest(&apk).unwrap();
        assert!(m.schemes.is_empty());
        assert!(m.web_hosts.is_empty());
    }

    #[test]
    fn corpus_thunderbird_schemes() {
        let Some((a, apk)) = corpus("net.thunderbird.android_33.apk") else {
            return;
        };
        let m = a.manifest(&apk).unwrap();
        assert!(m.schemes.iter().all(|s| validate::custom_scheme(s)));
        assert!(
            !m.schemes
                .iter()
                .any(|s| s == "http" || s == "https" || s == "content" || s == "file")
        );
        assert!(m.schemes.iter().any(|s| s == "mailto"), "{:?}", m.schemes);
    }

    #[test]
    fn corpus_game_category() {
        let Some((a, apk)) = corpus("com.shatteredpixel.shatteredpixeldungeon_922.apk") else {
            return;
        };
        let m = a.manifest(&apk).unwrap();
        assert!(m.is_game || m.app_category == Some(0), "{m:?}");
    }
}
