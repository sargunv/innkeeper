//! APK metadata, read from the APK itself: its binary `AndroidManifest.xml`
//! ([`xml`]), the `resources.arsc` values the manifest refers to ([`table`]),
//! and `lib/<abi>/` for native code. No Android SDK or session is needed.
//! Everything read is untrusted: front ends validate it before use.

mod chunk;
mod locale;
mod table;
mod xml;

use std::collections::BTreeSet;
use std::fs::File;
use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::validate;
use chunk::Value;
use locale::Locale;
use table::Table;
use xml::Element;

/// The oldest Android API level a session runs (Android 16; the image is 17).
pub const SESSION_MIN_SDK: u32 = 36;

/// Read caps for the two files we parse (the largest corpus table is 3 MiB).
const MAX_MANIFEST: u64 = 16 << 20;
const MAX_TABLE: u64 = 256 << 20;

/// How many references a value may go through (`@string/a` -> `@string/b`).
const MAX_REFS: usize = 8;

/// What an install needs from one APK.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ApkInfo {
    pub package: String,
    pub version_code: String,
    pub version_name: String,
    /// `config.x86_64` for split APKs; empty for the base.
    pub split: String,
    /// Default-locale label.
    pub label: String,
    /// (locale tag, label) for the APK's locales where the label differs
    /// from the default, e.g. ("pt-BR", "Calculadora"), sorted.
    pub labels: Vec<(String, String)>,
    /// Every variant of the launcher icon as (density, path inside the APK):
    /// 160 for mdpi (or no density), 0xfffe for anydpi (adaptive or vector
    /// XML), 0xffff for nodpi.
    pub icons: Vec<(u32, String)>,
    /// ABIs with libraries under `lib/`, sorted.
    pub native_code: Vec<String>,
    /// `uses-permission` names that apply to a session's SDK (sorted,
    /// deduplicated, as declared).
    pub permissions: Vec<String>,
    /// `android:appCategory` (`ApplicationInfo.CATEGORY_*`).
    pub app_category: Option<i32>,
    pub is_game: bool,
    /// Custom URI schemes of exported, browsable VIEW filters (sorted, valid).
    pub schemes: Vec<String>,
    /// Hosts of exported, browsable `http(s)` VIEW filters (sorted, valid).
    pub web_hosts: Vec<String>,
}

/// Reads an APK's metadata.
pub fn read(apk: &Path) -> Result<ApkInfo> {
    let f = File::open(apk).with_context(|| format!("open {}", apk.display()))?;
    let mut zip = zip::ZipArchive::new(f).with_context(|| format!("read zip {}", apk.display()))?;
    let files: Vec<String> = zip
        .file_names()
        .filter_map(|n| n.ok().map(|n| n.into_owned()))
        .collect();
    let mut entry = |name: &str, cap: u64| -> Result<Option<Vec<u8>>> {
        let e = match zip.by_name(name) {
            Ok(e) => e,
            Err(zip::result::ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("read {name}")),
        };
        let mut buf = vec![];
        e.take(cap + 1)
            .read_to_end(&mut buf)
            .with_context(|| format!("read {name}"))?;
        if buf.len() as u64 > cap {
            bail!("{name} is larger than {} MiB", cap >> 20);
        }
        Ok(Some(buf))
    };
    let manifest = entry("AndroidManifest.xml", MAX_MANIFEST)?
        .with_context(|| format!("{} has no AndroidManifest.xml", apk.display()))?;
    // Split APKs may have no resources.
    let table = entry("resources.arsc", MAX_TABLE)?;
    parse(&manifest, table.as_deref(), &files)
        .with_context(|| format!("read the manifest of {}", apk.display()))
}

/// [`read`] on an APK's parts: the manifest, the resource table and the
/// names of its files.
pub fn parse(manifest: &[u8], table: Option<&[u8]>, files: &[String]) -> Result<ApkInfo> {
    let root = xml::parse(manifest)?;
    if root.name != "manifest" {
        bail!(
            "the manifest's root element is <{}>",
            validate::truncate(&root.name, 40)
        );
    }
    // A broken table only costs labels and icons.
    let res = Res(table.and_then(|t| Table::parse(t).ok()));
    let text = |e: &Element, a: &str| e.attr(a).and_then(|v| res.text(v)).unwrap_or_default();
    let mut info = ApkInfo {
        package: text(&root, "package"),
        version_code: text(&root, "versionCode"),
        version_name: text(&root, "versionName"),
        split: text(&root, "split"),
        native_code: native_code(files),
        ..Default::default()
    };
    for p in root.children.iter().filter(|c| {
        matches!(
            c.name.as_str(),
            "uses-permission" | "uses-permission-sdk-23" | "uses-permission-sdk-m"
        )
    }) {
        let name = text(p, "name");
        // Requested only up to an old SDK (`maxSdkVersion`): doesn't apply
        // in the session, which runs a current Android.
        let max: Option<u32> = p
            .attr("maxSdkVersion")
            .and_then(|v| res.text(v)?.parse().ok());
        if !name.is_empty() && max.is_none_or(|m| m >= SESSION_MIN_SDK) {
            info.permissions.push(name);
        }
    }
    info.permissions.sort();
    info.permissions.dedup();
    if let Some(app) = root.find("application").next() {
        if let Some(v) = app.attr("label") {
            (info.label, info.labels) = res.localized(v);
        }
        if let Some(v) = app.attr("icon") {
            info.icons = res.files(v, 0);
        }
        info.app_category = match app.attr("appCategory").map(|v| res.value(v)) {
            Some(Value::Int(n)) => Some(n as i32),
            _ => None,
        };
        info.is_game = app.attr("isGame").map(|v| res.value(v)) == Some(Value::Bool(true));
        links(app, &res, &mut info);
    }
    Ok(info)
}

/// The ABIs of `lib/<abi>/<file>` entries.
fn native_code(files: &[String]) -> Vec<String> {
    let abis: BTreeSet<String> = files
        .iter()
        .filter_map(|f| {
            let mut it = f.strip_prefix("lib/")?.split('/');
            let abi = it.next()?;
            let ok = !abi.is_empty()
                && abi
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
            (ok && it.next().is_some_and(|f| !f.is_empty())).then(|| abi.to_string())
        })
        .collect();
    abis.into_iter().collect()
}

/// Schemes and web hosts from exported activities' browsable VIEW filters.
fn links(app: &Element, res: &Res, info: &mut ApkInfo) {
    let text = |e: &Element, a: &str| e.attr(a).and_then(|v| res.text(v));
    for comp in app
        .children
        .iter()
        .filter(|c| c.name == "activity" || c.name == "activity-alias")
    {
        if comp.attr("exported").map(|v| res.value(v)) == Some(Value::Bool(false)) {
            continue;
        }
        for f in comp.find("intent-filter") {
            let has = |el: &str, v: &str| f.find(el).any(|e| text(e, "name").as_deref() == Some(v));
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
            let schemes: Vec<String> = data.iter().filter_map(|d| text(d, "scheme")).collect();
            let hosts: Vec<String> = data.iter().filter_map(|d| text(d, "host")).collect();
            for s in &schemes {
                if validate::custom_scheme(s) {
                    info.schemes.push(s.clone());
                } else if s == "http" || s == "https" {
                    info.web_hosts
                        .extend(hosts.iter().filter(|h| validate::web_host(h)).cloned());
                }
            }
        }
    }
    info.schemes.sort();
    info.schemes.dedup();
    info.web_hosts.sort();
    info.web_hosts.dedup();
}

/// Resolves manifest values through the resource table, if there is one.
struct Res<'a>(Option<Table<'a>>);

impl Res<'_> {
    /// The value of resource `id` that Android would pick for `locale`.
    /// Values for other qualifiers (night mode, screen sizes) are ignored;
    /// of several SDK levels the highest wins, as on a current Android.
    fn pick(&self, id: u32, locale: &Locale) -> Option<Value> {
        let t = self.0.as_ref()?;
        t.values(id)
            .into_iter()
            .filter(|(c, _)| !c.other)
            .filter_map(|(c, v)| Some(((c.locale.score(locale)?, c.sdk), v)))
            .max_by_key(|(k, _)| *k)
            .map(|(_, v)| v)
    }

    /// `v` in `locale`, with references followed.
    fn resolve(&self, v: &Value, locale: &Locale) -> Value {
        let mut v = v.clone();
        for _ in 0..MAX_REFS {
            match v {
                Value::Ref(id) => match self.pick(id, locale) {
                    Some(n) => v = n,
                    None => return Value::Other,
                },
                _ => return v,
            }
        }
        Value::Other
    }

    /// `v` in the default configuration.
    fn value(&self, v: &Value) -> Value {
        self.resolve(v, &Locale::default())
    }

    /// `v` as text in the default configuration (integers in decimal).
    fn text(&self, v: &Value) -> Option<String> {
        match self.value(v) {
            Value::Str(s) => Some(s),
            Value::Int(n) => Some(n.to_string()),
            Value::Bool(b) => Some(b.to_string()),
            _ => None,
        }
    }

    /// The default text of `v`, and its text in each locale of the APK where
    /// that differs (as aapt2 lists it: `pt-BR` is listed if any resource
    /// has a `pt-BR` value, with the label Android shows there).
    fn localized(&self, v: &Value) -> (String, Vec<(String, String)>) {
        let text = |loc: &Locale| match self.resolve(v, loc) {
            Value::Str(s) => s,
            _ => String::new(),
        };
        let default = text(&Locale::default());
        let labels = self
            .0
            .iter()
            .flat_map(|t| t.locales())
            .filter(|l| !l.is_default())
            .map(|l| (l.tag(), text(l)))
            .filter(|(_, t)| !t.is_empty() && *t != default)
            .collect();
        (default, labels)
    }

    /// The files (paths inside the APK) of every density of `v`, sorted.
    fn files(&self, v: &Value, depth: usize) -> Vec<(u32, String)> {
        let mut out = vec![];
        match v {
            Value::Str(p) => out.push((160, p.clone())),
            Value::Ref(id) if depth < MAX_REFS => {
                for (c, n) in self.0.iter().flat_map(|t| t.values(*id)) {
                    if c.other || !c.locale.is_default() {
                        continue;
                    }
                    match n {
                        Value::Str(p) => out.push((
                            if c.density == 0 {
                                160
                            } else {
                                c.density as u32
                            },
                            p,
                        )),
                        r @ Value::Ref(_) => out.extend(self.files(&r, depth + 1)),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
        out.sort();
        out.dedup();
        out
    }
}

#[cfg(test)]
mod tests;
