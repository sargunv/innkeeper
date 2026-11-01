//! Turns `innkeeper-core` errors into messages for the user. A message has a
//! heading that says what failed, an optional action to take, and the full
//! error text as details.

/// What the user was doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Read,
    Install,
    Open,
    Uninstall,
    Start,
    Stop,
    AppSettings,
    Preferences,
    Profiles,
    /// Downloading and unpacking the image.
    Prepare,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Humane {
    pub heading: String,
    /// An action the user can take, if there is one.
    pub body: Option<String>,
    /// The full error chain, for a "Details" expander.
    pub details: String,
}

/// `name` is the app's name, or for [`Op::Start`], [`Op::Stop`] and
/// [`Op::Prepare`] the system's.
pub fn humane(op: Op, name: Option<&str>, err: &anyhow::Error) -> Humane {
    let details = format!("{err:#}");
    let (heading, body) = classify(op, name, err.downcast_ref::<innkeeper_core::ErrorKind>());
    Humane {
        heading,
        body: body.map(str::to_string),
        details,
    }
}

fn classify(
    op: Op,
    name: Option<&str>,
    kind: Option<&innkeeper_core::ErrorKind>,
) -> (String, Option<&'static str>) {
    use innkeeper_core::ErrorKind::*;
    let app = name.map_or_else(|| "the App".to_string(), cap);
    // Apps install and open in Android profiles, so app errors name Android.
    let system = match op {
        Op::Start | Op::Stop | Op::Prepare => name.unwrap_or("Android"),
        _ => "Android",
    };
    let h = |h: &str| h.to_string();
    let not_built = (h("Not Built for This Computer"), None);
    match kind {
        Some(NotAnApk) => (h("Not a Valid APK"), Some("Try downloading it again.")),
        Some(UnsupportedBundle) => (h("Unsupported Bundle"), None),
        Some(WrongAbi) => not_built,
        Some(SessionStartFailed) => (format!("{system} Couldn’t Start"), None),
        Some(ImageNotPrepared) => (
            format!("Couldn’t Download {system}"),
            Some("Check the network connection and free disk space."),
        ),
        Some(NoLauncherActivity) => (format!("{} Has No Main Screen", cap(&app)), None),
        Some(Timeout) => (format!("{system} Didn’t Respond"), Some("Try again.")),
        Some(InstallFailed(code)) => match code.as_str() {
            "INSTALL_FAILED_UPDATE_INCOMPATIBLE" => (
                h("Signed by a Different Developer"),
                Some("Uninstall the installed copy first."),
            ),
            "INSTALL_FAILED_VERSION_DOWNGRADE" => (
                h("A Newer Version Is Installed"),
                Some("Uninstall it first."),
            ),
            "INSTALL_FAILED_OLDER_SDK" => (h("Requires a Newer Android"), None),
            "INSTALL_FAILED_INSUFFICIENT_STORAGE" => {
                (h("Not Enough Disk Space"), Some("Free up disk space."))
            }
            "INSTALL_FAILED_NO_MATCHING_ABIS" | "INSTALL_FAILED_CPU_ABI_INCOMPATIBLE" => not_built,
            _ => (format!("Couldn’t Install {app}"), None),
        },
        None => (
            match op {
                Op::Preferences => h("Couldn’t Save Settings"),
                Op::Profiles => h("Couldn’t Change Profiles"),
                Op::Read => h("Couldn’t Read the File"),
                Op::Install => format!("Couldn’t Install {app}"),
                Op::Open => format!("Couldn’t Open {app}"),
                Op::Uninstall => format!("Couldn’t Uninstall {app}"),
                Op::AppSettings => format!("Couldn’t Change {app}’s Settings"),
                Op::Start => format!("Couldn’t Start {system}"),
                Op::Stop => format!("Couldn’t Stop {system}"),
                Op::Prepare => format!("Couldn’t Download {system}"),
            },
            None,
        ),
    }
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
    use innkeeper_core::ErrorKind;

    #[test]
    fn typed_context_selects_copy_independent_of_diagnostics() {
        for (kind, heading) in [
            (ErrorKind::NotAnApk, "Not a Valid APK"),
            (ErrorKind::UnsupportedBundle, "Unsupported Bundle"),
            (ErrorKind::WrongAbi, "Not Built for This Computer"),
            (ErrorKind::SessionStartFailed, "Android Couldn’t Start"),
            (ErrorKind::ImageNotPrepared, "Couldn’t Download Android"),
            (
                ErrorKind::NoLauncherActivity,
                "Calculator Has No Main Screen",
            ),
            (ErrorKind::Timeout, "Android Didn’t Respond"),
            (
                ErrorKind::InstallFailed("INSTALL_FAILED_UPDATE_INCOMPATIBLE".into()),
                "Signed by a Different Developer",
            ),
            (
                ErrorKind::InstallFailed("INSTALL_FAILED_VERSION_DOWNGRADE".into()),
                "A Newer Version Is Installed",
            ),
            (
                ErrorKind::InstallFailed("INSTALL_FAILED_OLDER_SDK".into()),
                "Requires a Newer Android",
            ),
            (
                ErrorKind::InstallFailed("INSTALL_FAILED_INSUFFICIENT_STORAGE".into()),
                "Not Enough Disk Space",
            ),
            (
                ErrorKind::InstallFailed("INSTALL_PARSE_FAILED_NO_CERTIFICATES".into()),
                "Couldn’t Install Calculator",
            ),
        ] {
            let e = anyhow::anyhow!("arbitrary diagnostic")
                .context(kind)
                .context("outer operation");
            assert_eq!(humane(Op::Install, Some("Calculator"), &e).heading, heading);
        }
        let e = anyhow::anyhow!("INSTALL_FAILED_OLDER_SDK");
        let h = humane(Op::Open, Some("Calculator"), &e);
        assert_eq!(h.heading, "Couldn’t Open Calculator");
        assert_eq!(h.body, None);
        assert_eq!(h.details, "INSTALL_FAILED_OLDER_SDK");
    }

    #[test]
    fn session_errors_name_the_profiles_system() {
        let failed = anyhow::anyhow!("x").context(ErrorKind::SessionStartFailed);
        assert_eq!(
            humane(Op::Start, Some("OpenHarmony"), &failed).heading,
            "OpenHarmony Couldn’t Start"
        );
        // Installing starts Android, whatever the app is called.
        assert_eq!(
            humane(Op::Install, Some("Calculator"), &failed).heading,
            "Android Couldn’t Start"
        );
        let e = anyhow::anyhow!("x");
        assert_eq!(
            humane(Op::Stop, Some("OpenHarmony"), &e).heading,
            "Couldn’t Stop OpenHarmony"
        );
    }
}
