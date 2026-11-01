//! Bundle operations inside an OpenHarmony session through `bm` and `aa`, the
//! tools that `hdc shell` offers. They run as root over exec with hdcd's
//! permissions (ohos/README.md). Both tools exit 0 on failure, so results come
//! from their output.

use std::fs::File;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

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
}

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
}

/// Parses `bm dump -n <bundle>`, which prints `<bundle>:` and then the JSON.
pub fn parse_dump(out: &str) -> Option<Bundle> {
    let d: Dump = serde_json::from_str(&out[out.find('{')?..]).ok()?;
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
    Some(Bundle {
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
