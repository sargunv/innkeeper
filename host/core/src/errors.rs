//! Stable failure categories for front ends. The diagnostic text stays in the
//! anyhow chain. Callers downcast to [`ErrorKind`] instead of parsing that
//! text.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    NotAnApk,
    NotAHap,
    UnsupportedBundle,
    /// The package is for the other system than the profile's.
    WrongSystem,
    WrongAbi,
    SessionStartFailed,
    ImageNotPrepared,
    NoLauncherActivity,
    Timeout,
    InstallFailed(String),
}

impl std::fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAnApk => f.write_str("not a readable Android app"),
            Self::NotAHap => f.write_str("not a readable OpenHarmony app"),
            Self::UnsupportedBundle => f.write_str("unsupported APK bundle"),
            Self::WrongSystem => f.write_str("app for another system"),
            Self::WrongAbi => f.write_str("incompatible processor architecture"),
            Self::SessionStartFailed => f.write_str("Android could not start"),
            Self::ImageNotPrepared => f.write_str("Android image is not prepared"),
            Self::NoLauncherActivity => f.write_str("app has no launcher activity"),
            Self::Timeout => f.write_str("Android did not respond in time"),
            Self::InstallFailed(code) => write!(f, "Android refused installation ({code})"),
        }
    }
}

/// Extracts PackageManager's symbolic failure code from its diagnostic text.
pub fn install_failure(text: &str) -> Option<ErrorKind> {
    text.split(|c: char| !c.is_ascii_uppercase() && !c.is_ascii_digit() && c != '_')
        .find(|s| {
            ["INSTALL_FAILED_", "INSTALL_PARSE_FAILED_"]
                .iter()
                .any(|p| s.starts_with(p) && s.len() > p.len())
        })
        .map(|s| ErrorKind::InstallFailed(s.to_string()))
}

/// Attaches PackageManager's failure code as context and shortens its
/// diagnostic. Other failures keep their original chain, including any Timeout
/// context.
pub(crate) fn classify_install_error(err: anyhow::Error) -> anyhow::Error {
    let text = format!("{err:#}");
    match install_failure(&text) {
        Some(kind) => anyhow::anyhow!(crate::validate::truncate(text.trim(), 600)).context(kind),
        None => err,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_manager_codes() {
        for code in [
            "INSTALL_FAILED_UPDATE_INCOMPATIBLE",
            "INSTALL_FAILED_VERSION_DOWNGRADE",
            "INSTALL_PARSE_FAILED_NO_CERTIFICATES",
            "INSTALL_FAILED_FUTURE_CODE_2",
        ] {
            assert_eq!(
                install_failure(&format!("Failure [{code}: long explanation]")),
                Some(ErrorKind::InstallFailed(code.into()))
            );
        }
        for text in [
            "Success",
            "INSTALL_FAILED_",
            "NOT_INSTALL_FAILED_INVALID_APK",
        ] {
            assert_eq!(install_failure(text), None);
        }
        let long = format!(
            "{} Failure [INSTALL_FAILED_INVALID_APK: detail]",
            "prefix ".repeat(100)
        );
        let err = classify_install_error(anyhow::anyhow!(long));
        assert_eq!(
            err.downcast_ref::<ErrorKind>(),
            Some(&ErrorKind::InstallFailed(
                "INSTALL_FAILED_INVALID_APK".into()
            ))
        );
        assert!(err.root_cause().to_string().len() < 650);

        let err = anyhow::anyhow!("diagnostic changed")
            .context(ErrorKind::WrongAbi)
            .context("install");
        assert_eq!(err.downcast_ref::<ErrorKind>(), Some(&ErrorKind::WrongAbi));
    }
}
