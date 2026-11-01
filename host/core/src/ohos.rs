//! Bundle operations inside an OpenHarmony session through `bm` and `aa`, the
//! tools that `hdc shell` offers. They run as root over exec with hdcd's
//! permissions (ohos/README.md). Both tools exit 0 on failure, so results come
//! from their output.

use std::collections::BTreeSet;
use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::app_settings::Permission;
use crate::launcher::Session;
use crate::{ErrorKind, validate};

/// Installs one HAP. The file streams through stdin to a staging path, and
/// `bm` installs it from there for the foreground user.
pub fn install(s: &Session, hap: &Path) -> Result<()> {
    let f = File::open(hap).with_context(|| format!("open {}", hap.display()))?;
    let staging = format!("/data/local/tmp/innkeeper-{}.hap", std::process::id());
    // `$1` is the staging path, and the script is a constant.
    let o = s.exec_with(
        600,
        &[
            "sh",
            "-c",
            "mkdir -p \"${1%/*}\" && cat > \"$1\"",
            "sh",
            &staging,
        ],
        f.into(),
    )?;
    if !o.ok() {
        bail!("copy to {staging} failed: {}", o.stderr.trim());
    }
    let o = s.exec(600, &["bm", "install", "-p", &staging]);
    let _ = s.exec(30, &["rm", "-f", &staging]);
    let o = o?;
    let text = o.text();
    if text.contains("install bundle successfully") {
        return Ok(());
    }
    let e = anyhow::anyhow!("{}", errors(&format!("{text}\n{}", o.stderr)));
    Err(match failure_code(&text) {
        Some(code) => e.context(ErrorKind::InstallFailed(code)),
        None => e,
    })
}

/// `bm`'s numeric failure code (`code:9568344`).
pub fn failure_code(text: &str) -> Option<String> {
    let rest = text.split("code:").nth(1)?;
    let code: String = rest
        .trim_start()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (!code.is_empty()).then_some(code)
}

/// What `bm dump -n` reports about one bundle.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Bundle {
    pub name: String,
    /// Whether the bundle came with the image.
    pub preinstalled: bool,
    /// The ability that the home screen launches, as (module, ability).
    pub launcher: Option<(String, String)>,
    /// The URI schemes, in lower case, that the bundle's exported abilities
    /// open from a browser ([`BROWSABLE`]).
    pub link_schemes: BTreeSet<String>,
}

/// The action and entity of a want that opens a link, as a browser sends
/// it. Like Android's `BROWSABLE` category, the entity limits the start to
/// abilities that accept links from a browser.
const VIEW: &str = "ohos.want.action.viewData";
const BROWSABLE: &str = "entity.system.browsable";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Dump {
    name: String,
    #[serde(default)]
    is_pre_install_app: bool,
    #[serde(default)]
    entry_module_name: String,
    #[serde(default)]
    hap_module_infos: Vec<DumpModule>,
    #[serde(default)]
    version_name: String,
    #[serde(default)]
    version_code: u64,
    #[serde(default)]
    req_permissions: Vec<String>,
    /// One per requested permission for the current user: 0 is granted and
    /// -1 denied.
    #[serde(default)]
    req_permission_states: Vec<i32>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DumpModule {
    #[serde(default)]
    ability_infos: Vec<DumpAbility>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DumpAbility {
    name: String,
    #[serde(default)]
    module_name: String,
    #[serde(default)]
    visible: bool,
    #[serde(default)]
    skills: Vec<DumpSkill>,
}

#[derive(Deserialize)]
struct DumpSkill {
    #[serde(default)]
    entities: Vec<String>,
    #[serde(default)]
    actions: Vec<String>,
    #[serde(default)]
    uris: Vec<DumpUri>,
}

#[derive(Deserialize)]
struct DumpUri {
    #[serde(default)]
    scheme: String,
}

fn dump_json(out: &str) -> Option<Dump> {
    serde_json::from_str(&out[out.find('{')?..]).ok()
}

/// Parses `bm dump -n <bundle>`, which prints `<bundle>:` and then the JSON.
pub fn parse_dump(out: &str) -> Option<Bundle> {
    let d = dump_json(out)?;
    // The home screen shows the exported abilities whose skills have the home
    // entity and action. The entry module's ability comes first.
    let mut launchers: Vec<&DumpAbility> = d
        .hap_module_infos
        .iter()
        .flat_map(|m| &m.ability_infos)
        .filter(|a| {
            a.visible
                && a.skills.iter().any(|s| {
                    s.entities.iter().any(|e| e == "entity.system.home")
                        && s.actions.iter().any(|a| a == "action.system.home")
                })
        })
        .collect();
    launchers.sort_by_key(|a| a.module_name != d.entry_module_name);
    let link_schemes = d
        .hap_module_infos
        .iter()
        .flat_map(|m| &m.ability_infos)
        .filter(|a| a.visible)
        .flat_map(|a| &a.skills)
        .filter(|s| {
            s.actions.iter().any(|a| a == VIEW) && s.entities.iter().any(|e| e == BROWSABLE)
        })
        .flat_map(|s| &s.uris)
        .filter(|u| !u.scheme.is_empty())
        .map(|u| u.scheme.to_ascii_lowercase())
        .collect();
    Some(Bundle {
        link_schemes,
        launcher: launchers
            .first()
            .map(|a| (a.module_name.clone(), a.name.clone())),
        preinstalled: d.is_pre_install_app,
        name: d.name,
    })
}

/// The bundle named `name`, or None if it isn't installed.
pub fn bundle(s: &Session, name: &str) -> Result<Option<Bundle>> {
    let out = s.exec_ok(60, &["bm", "dump", "-n", name])?;
    Ok(parse_dump(&out).filter(|b| b.name == name))
}

/// What `app info` shows about a bundle.
#[derive(Debug, Default, Clone, Serialize)]
pub struct BundleInfo {
    pub version_name: String,
    pub version_code: u64,
    /// The bundle's code.
    pub app_bytes: u64,
    /// The user's files, preferences and databases, including the cache.
    pub data_bytes: u64,
    pub cache_bytes: u64,
    /// The requested permissions in the order the bundle lists them. A
    /// `user_grant` permission is editable in Settings, and a
    /// `system_grant` permission isn't.
    pub permissions: Vec<Permission>,
    /// Whether the app may run in the background. It is None until Innkeeper
    /// System's first activity report.
    pub background: Option<bool>,
}

/// OH's first user, the one an Innkeeper profile runs.
const USER: &str = "100";

/// Reads bundle `name`'s version, sizes and permissions. BMS has no tool
/// that prints a bundle's storage stats, so `du` measures the bundle's
/// directories. Grant modes come from the access token service's
/// permission definitions.
pub fn info(s: &Session, name: &str) -> Result<BundleInfo> {
    let out = s.exec_ok(60, &["bm", "dump", "-n", name])?;
    let Some(d) = dump_json(&out).filter(|d| d.name == name) else {
        bail!("{name} isn't installed");
    };
    let mut info = BundleInfo {
        version_name: d.version_name,
        version_code: d.version_code,
        ..Default::default()
    };
    let du = s.exec(
        60,
        &[
            "sh",
            "-c",
            // `$1` is the user and `$2` the bundle. A missing directory makes
            // du print an error and nothing else. A du counts each inode once, so the cache
            // directories, which sit inside the data directories, get a du
            // of their own.
            "cd /data/app && \
             du -sk el1/bundle/public/\"$2\" el[12]/$1/base/\"$2\" el[12]/$1/database/\"$2\" 2>/dev/null; \
             du -sk el[12]/$1/base/\"$2\"/cache el[12]/$1/base/\"$2\"/haps/*/cache 2>/dev/null",
            "sh",
            USER,
            name,
        ],
    )?;
    (info.app_bytes, info.data_bytes, info.cache_bytes) = parse_du(&du.text());
    let names: Vec<&str> = d
        .req_permissions
        .iter()
        .map(String::as_str)
        .filter(|p| permission_name(p))
        .collect();
    let defs = if names.is_empty() {
        String::new()
    } else {
        s.exec_ok(
            60,
            &[
                &["sh", "-c", "for p; do atm dump -d -p \"$p\"; done", "sh"],
                &names[..],
            ]
            .concat(),
        )?
    };
    let user_grant = parse_user_grant(&defs);
    info.permissions = d
        .req_permissions
        .iter()
        .zip(&d.req_permission_states)
        .filter(|(p, _)| permission_name(p))
        .map(|(p, state)| Permission {
            name: p.clone(),
            granted: *state == 0,
            editable: user_grant.contains(p),
        })
        .collect();
    Ok(info)
}

/// Whether `p` looks like a permission name, such as
/// `ohos.permission.CAMERA`.
fn permission_name(p: &str) -> bool {
    !p.is_empty()
        && p.len() <= 256
        && p.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._".contains(&c))
}

/// Sums `du -sk` output over the code directory, the data directories and
/// the cache directories, in bytes. A cache directory is also counted in
/// the data directory that holds it.
fn parse_du(out: &str) -> (u64, u64, u64) {
    let (mut app, mut data, mut cache) = (0, 0, 0);
    for line in out.lines() {
        let Some((kib, path)) = line.split_once('\t') else {
            continue;
        };
        let Ok(kib) = kib.trim().parse::<u64>() else {
            continue;
        };
        let bytes = kib.saturating_mul(1024);
        if path.starts_with("el1/bundle/") {
            app += bytes;
        } else if path.ends_with("/cache") {
            cache += bytes;
        } else {
            data += bytes;
        }
    }
    (app, data, cache)
}

/// The `USER_GRANT` permissions among `atm dump -d -p` outputs, which are
/// JSON objects one after another.
fn parse_user_grant(out: &str) -> Vec<String> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Def {
        permission_name: String,
        grant_mode: String,
    }
    serde_json::Deserializer::from_str(out)
        .into_iter::<Def>()
        .map_while(Result::ok)
        .filter(|d| d.grant_mode == "USER_GRANT")
        .map(|d| d.permission_name)
        .collect()
}

/// Lists the bundles that the user installed, with their labels, sorted by
/// name. The shell loop runs every `bm dump` in one exec.
pub fn user_bundles(s: &Session) -> Result<Vec<(String, String)>> {
    let out = s.exec_ok(60, &["bm", "dump", "-a", "-l"])?;
    let labels: Vec<Label> = serde_json::from_str(&out[out.find('[').unwrap_or(0)..])
        .with_context(|| format!("bm dump -a -l: {}", validate::truncate(out.trim(), 200)))?;
    let user = s.exec_ok(
        120,
        &[
            "sh",
            "-c",
            "for b in \"$@\"; do if bm dump -n \"$b\" | grep -qE '\"isPreInstallApp\": *false'; then echo \"$b\"; fi; done",
            "sh",
        ]
        .into_iter()
        .chain(
            labels
                .iter()
                .map(|l| l.bundle_name.as_str())
                .filter(|b| validate::package(b).is_ok()),
        )
        .collect::<Vec<_>>(),
    )?;
    let user: Vec<&str> = user.lines().map(str::trim).collect();
    let mut v: Vec<(String, String)> = labels
        .into_iter()
        .filter(|l| user.contains(&l.bundle_name.as_str()))
        .map(|l| (l.bundle_name, l.label))
        .collect();
    v.sort();
    Ok(v)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Label {
    bundle_name: String,
    #[serde(default)]
    label: String,
}

/// Uninstalls a bundle, keeping its data with `keep_data`.
pub fn uninstall(s: &Session, name: &str, keep_data: bool) -> Result<()> {
    let mut argv = vec!["bm", "uninstall", "-n", name];
    if keep_data {
        argv.push("-k");
    }
    expect(s, &argv, "uninstall bundle successfully")
}

/// Starts the bundle's launcher ability, as the home screen does.
pub fn start(s: &Session, name: &str) -> Result<String> {
    let Some((module, ability)) = bundle(s, name)?.and_then(|b| b.launcher) else {
        return Err(
            anyhow::anyhow!("{name} isn't installed or has no launcher ability")
                .context(ErrorKind::NoLauncherActivity),
        );
    };
    let argv = ["aa", "start", "-b", name, "-m", &module, "-a", &ability];
    match expect(s, &argv, "start ability successfully") {
        Ok(()) => Ok(format!("started {name}/{ability}")),
        // AMS blocks third-party apps until the first unlock, and it reports
        // that as an internal error.
        Err(e) if screen_locked(s) => {
            Err(e.context("OpenHarmony's screen is locked. Unlock it, then try again"))
        }
        Err(e) => Err(e),
    }
}

/// Opens `uri`, which [`validate::launch_uri`] checked, in bundle `name`, as
/// a browser opens a link. AMS picks the bundle's ability whose skills match
/// the URI.
pub fn open_uri(s: &Session, name: &str, uri: &str) -> Result<String> {
    let scheme = uri
        .split_once(':')
        .map_or("", |(s, _)| s)
        .to_ascii_lowercase();
    let Some(b) = bundle(s, name)? else {
        bail!("{name} isn't installed");
    };
    if !b.link_schemes.contains(&scheme) {
        bail!("{name} doesn't open {scheme}: links");
    }
    let argv = [
        "aa", "start", "-b", name, "-A", VIEW, "-e", BROWSABLE, "-U", uri,
    ];
    expect(s, &argv, "start ability successfully")?;
    Ok(format!("opened {} in {name}", validate::truncate(uri, 80)))
}

/// Whether SceneBoard shows its lock screen.
fn screen_locked(s: &Session) -> bool {
    s.exec(30, &["hidumper", "-s", "ScreenlockService", "-a", "-all"])
        .is_ok_and(|o| parse_screen_locked(&o.text()))
}

/// Reads `screenLocked` from the screen lock service's dump.
fn parse_screen_locked(dump: &str) -> bool {
    dump.lines().any(|l| {
        let mut w = l.split_whitespace().skip_while(|w| *w == "*");
        w.next() == Some("screenLocked") && w.next() == Some("true")
    })
}

/// Stops every process of the bundle.
pub fn force_stop(s: &Session, name: &str) -> Result<()> {
    expect(
        s,
        &["aa", "force-stop", name],
        "force stop process successfully",
    )
}

/// Deletes the bundle's cache (`cache`) or all of its data.
pub fn clean(s: &Session, name: &str, cache: bool) -> Result<()> {
    let what = if cache { "-c" } else { "-d" };
    let o = s.exec(60, &["bm", "clean", "-n", name, what])?;
    let text = o.text();
    if text.contains("successfully") && !text.contains("error") {
        return Ok(());
    }
    bail!(
        "bm clean {name}: {}",
        errors(&format!("{text}\n{}", o.stderr))
    );
}

/// Runs `argv` and fails with its errors unless the output contains `ok`.
fn expect(s: &Session, argv: &[&str], ok: &str) -> Result<()> {
    let o = s.exec(120, argv)?;
    let text = o.text();
    if text.contains(ok) {
        return Ok(());
    }
    bail!(
        "{}: {}",
        argv[..2].join(" "),
        errors(&format!("{text}\n{}", o.stderr))
    );
}

/// The error lines of `bm` and `aa` output, which also print causes and
/// suggestions, joined into one line.
pub fn errors(out: &str) -> String {
    let lines: Vec<&str> = out
        .lines()
        .map(str::trim)
        .filter(|l| {
            l.starts_with("error:") || l.starts_with("code:") || l.starts_with("Error Code:")
        })
        .collect();
    let text = if lines.is_empty() {
        out.trim().to_string()
    } else {
        lines.join(" ")
    };
    validate::truncate(&text, 400).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn du_sizes() {
        let out = "1284\tel1/bundle/public/dev.example.app\n\
                   0\tel1/100/base/dev.example.app\n\
                   40\tel2/100/base/dev.example.app\n\
                   2072\tel2/100/database/dev.example.app\n\
                   12\tel2/100/base/dev.example.app/cache\n\
                   4\tel2/100/base/dev.example.app/haps/entry/cache\n\
                   du: odd line\n\
                   x\tel2/100/base/y\n";
        assert_eq!(parse_du(out), (1284 * 1024, 2112 * 1024, 16 * 1024));
        assert_eq!(parse_du(""), (0, 0, 0));
    }

    #[test]
    fn user_grant_permissions() {
        let out = r#"{
  "permissionName": "ohos.permission.CAMERA",
  "grantMode": "USER_GRANT",
  "availableLevel": "NORMAL"
}
{
  "permissionName": "ohos.permission.INTERNET",
  "grantMode": "SYSTEM_GRANT"
}
"#;
        assert_eq!(parse_user_grant(out), ["ohos.permission.CAMERA"]);
        assert!(parse_user_grant("").is_empty());
        assert!(parse_user_grant("Invalid param").is_empty());
    }

    #[test]
    fn permission_names() {
        assert!(permission_name("ohos.permission.CAMERA"));
        assert!(!permission_name(""));
        assert!(!permission_name("a b"));
        assert!(!permission_name("-x"));
    }

    #[test]
    fn dump_versions_and_permission_states() {
        let out = "dev.example.app:\n{\"name\": \"dev.example.app\", \"versionName\": \"1.0.2\", \
                   \"versionCode\": 1000002, \"reqPermissions\": [\"ohos.permission.CAMERA\", \
                   \"ohos.permission.INTERNET\"], \"reqPermissionStates\": [-1, 0]}";
        let d = dump_json(out).unwrap();
        assert_eq!(
            (d.version_name.as_str(), d.version_code),
            ("1.0.2", 1000002)
        );
        assert_eq!(d.req_permissions.len(), 2);
        assert_eq!(d.req_permission_states, [-1, 0]);
        let bare = dump_json("x:\n{\"name\": \"x\"}").unwrap();
        assert_eq!((bare.version_code, bare.req_permissions.len()), (0, 0));
    }

    #[test]
    fn link_schemes_need_view_browsable_and_an_exported_ability() {
        let abilities = r#"
            {"name": "Links", "visible": true, "skills": [
                {"actions": ["ohos.want.action.viewData"], "entities": ["entity.system.browsable"],
                 "uris": [{"scheme": "HTTPS", "host": "example.com"}, {"scheme": ""}]},
                {"actions": ["ohos.want.action.viewData"], "entities": ["entity.system.home"],
                 "uris": [{"scheme": "sms"}]},
                {"actions": ["ohos.want.action.sendData"], "entities": ["entity.system.browsable"],
                 "uris": [{"scheme": "mailto"}]}]},
            {"name": "Hidden", "visible": false, "skills": [
                {"actions": ["ohos.want.action.viewData"], "entities": ["entity.system.browsable"],
                 "uris": [{"scheme": "secret"}]}]}"#;
        let b = parse_dump(&dump(abilities, "entry", false)).unwrap();
        assert_eq!(b.link_schemes, BTreeSet::from(["https".to_string()]));
    }

    #[test]
    fn screen_lock_dump() {
        let dump = " Screenlock system state\\tValue\\t\\tDescription\n \
                    * deviceLocked  \t\tfalse\t\t100\n \
                    * screenLocked  \t\ttrue\t\t100\n";
        assert!(parse_screen_locked(dump));
        assert!(!parse_screen_locked(&dump.replace("\t\ttrue", "\t\tfalse")));
        assert!(!parse_screen_locked("Option  Description"));
    }

    #[test]
    fn error_lines() {
        let aa = "error: failed to start ability.\nError Code:10108101  Error Message:An internal error.\n\
                  Error cause: Kernel common errors\n  Try the following:\n  > Try restarting the device\n";
        assert_eq!(
            errors(aa),
            "error: failed to start ability. Error Code:10108101  Error Message:An internal error."
        );
        let bm = "error: failed to install bundle.\ncode:9568344\nerror: install parse profile prop check error.\n";
        assert_eq!(
            errors(bm),
            "error: failed to install bundle. code:9568344 error: install parse profile prop check error."
        );
        assert_eq!(errors(" odd output \n"), "odd output");
    }

    #[test]
    fn bm_failure_codes() {
        let out = "error: failed to install bundle.\ncode:9568344\nerror: install parse profile prop check error.\n";
        assert_eq!(failure_code(out).as_deref(), Some("9568344"));
        assert_eq!(failure_code("install bundle successfully."), None);
        assert_eq!(failure_code("code: x"), None);
    }

    fn dump(abilities: &str, entry: &str, preinstalled: bool) -> String {
        format!(
            "dev.example.app:\n{{\"name\": \"dev.example.app\", \"isPreInstallApp\": {preinstalled}, \
             \"entryModuleName\": \"{entry}\", \"hapModuleInfos\": [{{\"abilityInfos\": [{abilities}]}}]}}\n"
        )
    }

    const HOME: &str =
        r#""skills": [{"entities": ["entity.system.home"], "actions": ["action.system.home"]}]"#;

    #[test]
    fn launcher_ability_follows_the_home_skill() {
        // The calculator's mainElement is com.ohos.calculator.CalculatorAbility,
        // but its ability is named CalculatorAbility.
        let abilities = format!(
            r#"{{"name": "Service", "moduleName": "entry", "visible": true}},
               {{"name": "Feature", "moduleName": "feature", "visible": true, {HOME}}},
               {{"name": "Hidden", "moduleName": "entry", "visible": false, {HOME}}},
               {{"name": "CalculatorAbility", "moduleName": "entry", "visible": true, {HOME}}}"#
        );
        let b = parse_dump(&dump(&abilities, "entry", false)).unwrap();
        assert_eq!(
            b,
            Bundle {
                name: "dev.example.app".into(),
                preinstalled: false,
                launcher: Some(("entry".into(), "CalculatorAbility".into())),
                link_schemes: BTreeSet::new(),
            }
        );
        let b = parse_dump(&dump(&abilities, "other", true)).unwrap();
        assert!(b.preinstalled);
        assert_eq!(b.launcher, Some(("feature".into(), "Feature".into())));
        let none = parse_dump(&dump(r#"{"name": "A", "visible": true}"#, "entry", false));
        assert_eq!(none.unwrap().launcher, None);
        assert_eq!(
            parse_dump("error: failed to get information and the parameters may be wrong."),
            None
        );
    }
}
