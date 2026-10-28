//! Turns `linuxapk-core` errors into messages for people: a heading, a
//! sentence or two, and the original text as details.

/// What the user was doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read,
    Install,
    Open,
    Uninstall,
    Session,
    /// Downloading and unpacking the Android image.
    Prepare,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Humane {
    pub heading: String,
    pub body: String,
    /// The full error chain, for a "Details" expander.
    pub details: String,
}

/// `name` is the app's name where one is known.
pub fn humane(op: Op, name: Option<&str>, err: &anyhow::Error) -> Humane {
    let details = format!("{err:#}");
    let (heading, body) = classify(op, name, &details);
    Humane {
        heading,
        body,
        details,
    }
}

fn classify(op: Op, name: Option<&str>, d: &str) -> (String, String) {
    let app = name.unwrap_or("the app");
    let has = |s: &str| d.contains(s);
    let pair = |h: &str, b: &str| (h.to_string(), b.to_string());
    if has("aapt2 not found") {
        return pair(
            "Android Build Tools Missing",
            "Linux.apk reads APK files with aapt2 from the Android SDK build tools. Install them, or set LINUXAPK_AAPT2 to aapt2’s path.",
        );
    }
    if has("is not an APK or APK bundle") {
        return pair(
            "Not an Android App",
            "This file isn’t an APK or an APK bundle (.apks, .xapk or .apkm).",
        );
    }
    if has("encrypted") || has("contains no APKs") {
        return pair(
            "Unsupported Bundle",
            "This bundle contains no installable APKs. Encrypted .apkm files aren’t supported.",
        );
    }
    if has("only has native code for") || has("has no native code for this session") {
        return (
            "Not Built for This Computer".into(),
            format!(
                "{} only contains code for phone processors (ARM). This computer can’t run it yet.",
                cap(app)
            ),
        );
    }
    if has("INSTALL_FAILED_UPDATE_INCOMPATIBLE") {
        return (
            "A Different Version Is Installed".into(),
            format!(
                "The installed copy of {app} was signed by someone else. Uninstall it first to install this one."
            ),
        );
    }
    if has("INSTALL_FAILED_VERSION_DOWNGRADE") {
        return (
            "A Newer Version Is Installed".into(),
            format!("This is an older version of {app} than the one already installed."),
        );
    }
    if has("INSTALL_FAILED_OLDER_SDK") {
        return pair(
            "Needs a Newer Android",
            "This app requires a newer version of Android than Linux.apk runs.",
        );
    }
    if has("INSTALL_FAILED_INSUFFICIENT_STORAGE") {
        return pair(
            "Not Enough Space",
            "There isn’t enough free disk space to install this app.",
        );
    }
    if has("INSTALL_PARSE_FAILED") || has("INSTALL_FAILED_INVALID_APK") || has("read zip") {
        return pair(
            "Damaged APK",
            "This file couldn’t be read as an Android app. It may be incomplete or corrupt; try downloading it again.",
        );
    }
    if has("expected one base APK") || has("bundle mixes packages") {
        return pair(
            "Unsupported Bundle",
            "This bundle doesn’t contain exactly one app.",
        );
    }
    if has("linuxapk-session not found") {
        return pair(
            "Linux.apk Isn’t Set Up",
            "This copy of Linux.apk is incomplete: the Android session launcher isn’t installed next to it. In a checkout, run `mise run build` and start out/stage/bin/linuxapk-manager.",
        );
    }
    if has("session start failed") || has("stopped while booting") || has("did not finish booting")
    {
        return pair(
            "Android Couldn’t Start",
            "The Android session didn’t start. Its log (linuxapk-session log) says why.",
        );
    }
    if has("has no launcher activity") {
        return (
            format!("{} Can’t Be Opened", cap(app)),
            "It has no main screen. It may be a keyboard, a widget or a background service, which run inside other apps."
                .into(),
        );
    }
    if has("timed out") {
        return pair(
            "Android Didn’t Respond",
            "The Android session took too long to answer. Try again in a moment.",
        );
    }
    if has("`linuxapk` not found") {
        return pair(
            "The linuxapk Command Is Missing",
            "App menu entries run apps with the linuxapk command. Install it next to linuxapk-manager or on your PATH.",
        );
    }
    let heading = match op {
        Op::Read => "Couldn’t Read the File".to_string(),
        Op::Install => format!("Couldn’t Install {}", cap(app)),
        Op::Open => format!("Couldn’t Open {}", cap(app)),
        Op::Uninstall => format!("Couldn’t Uninstall {}", cap(app)),
        Op::Session => "Couldn’t Control the Android Session".into(),
        Op::Prepare => "Couldn’t Download Android".into(),
    };
    let first = d.split(": ").last().unwrap_or(d).trim();
    let body = if first.is_empty() {
        "Something went wrong.".to_string()
    } else {
        let s = linuxapk_core::validate::truncate(first, 200);
        let s = cap(&s);
        if s.ends_with('.') { s } else { s + "." }
    };
    (heading, body)
}

fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(op: Op, e: &str) -> Humane {
        humane(op, Some("Calculator"), &anyhow::anyhow!(e.to_string()))
    }

    #[test]
    fn known() {
        assert_eq!(
            h(
                Op::Install,
                "org.x only has native code for arm64-v8a (this session runs x86_64)"
            )
            .heading,
            "Not Built for This Computer"
        );
        assert_eq!(
            h(
                Op::Read,
                "/a/b.txt is not an APK or APK bundle (not a zip archive)"
            )
            .heading,
            "Not an Android App"
        );
        assert_eq!(
            h(
                Op::Install,
                "Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: x]"
            )
            .heading,
            "A Different Version Is Installed"
        );
        assert_eq!(
            h(
                Op::Open,
                "org.x has no launcher activity (is it installed?)"
            )
            .heading,
            "Calculator Can’t Be Opened"
        );
    }

    #[test]
    fn fallback() {
        let e = anyhow::anyhow!("boom happened").context("uninstall org.x");
        let m = humane(Op::Uninstall, Some("Calculator"), &e);
        assert_eq!(m.heading, "Couldn’t Uninstall Calculator");
        assert_eq!(m.body, "Boom happened.");
        assert_eq!(m.details, "uninstall org.x: boom happened");
    }
}
