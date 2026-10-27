//! Plain-language names for Android permissions on the install page.

/// A permission as the install page shows it: a title, and whether it gives
/// access to something personal (listed first, with a warning icon).
pub struct Shown {
    pub title: String,
    pub sensitive: bool,
}

/// Describes `name` (`android.permission.CAMERA`). Unknown permissions keep
/// their constant, made readable (`READ_SYNC_SETTINGS` -> "Read sync
/// settings"); an app's own (`com.example.app.C2D_MESSAGE`) is `None`, since
/// it means nothing to the user.
pub fn describe(name: &str) -> Option<Shown> {
    let short = name
        .strip_prefix("android.permission.")
        .or_else(|| name.strip_prefix("com.android.alarm.permission."))
        .or_else(|| name.strip_prefix("com.android.launcher.permission."))?;
    let (title, sensitive) = match short {
        "INTERNET" => ("Full network access", false),
        "ACCESS_NETWORK_STATE" | "ACCESS_WIFI_STATE" => ("View network connections", false),
        "CAMERA" => ("Camera", true),
        "RECORD_AUDIO" => ("Microphone", true),
        "ACCESS_FINE_LOCATION" => ("Precise location", true),
        "ACCESS_COARSE_LOCATION" => ("Approximate location", true),
        "ACCESS_BACKGROUND_LOCATION" => ("Location in the background", true),
        "READ_CONTACTS" | "WRITE_CONTACTS" | "GET_ACCOUNTS" => ("Contacts and accounts", true),
        "READ_CALENDAR" | "WRITE_CALENDAR" => ("Calendar", true),
        "READ_EXTERNAL_STORAGE" | "WRITE_EXTERNAL_STORAGE" => ("Shared storage", true),
        "MANAGE_EXTERNAL_STORAGE" => ("All files in shared storage", true),
        "READ_MEDIA_IMAGES" | "READ_MEDIA_VIDEO" => ("Photos and videos", true),
        "READ_MEDIA_AUDIO" => ("Music and audio", true),
        "READ_PHONE_STATE" | "READ_PHONE_NUMBERS" => ("Phone status and identity", true),
        "CALL_PHONE" => ("Make phone calls", true),
        "READ_SMS" | "SEND_SMS" | "RECEIVE_SMS" => ("Text messages", true),
        "BODY_SENSORS" | "ACTIVITY_RECOGNITION" => ("Body sensors and activity", true),
        "BLUETOOTH" | "BLUETOOTH_ADMIN" | "BLUETOOTH_CONNECT" | "BLUETOOTH_SCAN" => {
            ("Bluetooth", false)
        }
        "NEARBY_WIFI_DEVICES" => ("Nearby Wi-Fi devices", false),
        "POST_NOTIFICATIONS" => ("Show notifications", false),
        "VIBRATE" => ("Vibration", false),
        "WAKE_LOCK" => ("Keep the device awake", false),
        "RECEIVE_BOOT_COMPLETED" => ("Run at startup", false),
        "FOREGROUND_SERVICE" => ("Run in the background", false),
        "SYSTEM_ALERT_WINDOW" => ("Draw over other apps", false),
        "REQUEST_INSTALL_PACKAGES" => ("Install other apps", true),
        "QUERY_ALL_PACKAGES" => ("See all installed apps", false),
        "USE_BIOMETRIC" | "USE_FINGERPRINT" => ("Biometric sign-in", false),
        "NFC" => ("NFC", false),
        "SCHEDULE_EXACT_ALARM" | "USE_EXACT_ALARM" | "SET_ALARM" => ("Alarms", false),
        s if s.starts_with("FOREGROUND_SERVICE_") => return None,
        s => return Some(readable(s)),
    };
    Some(Shown {
        title: title.into(),
        sensitive,
    })
}

fn readable(constant: &str) -> Shown {
    let words = constant.replace('_', " ").to_lowercase();
    let mut c = words.chars();
    let title = match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    };
    Shown {
        title,
        sensitive: false,
    }
}

/// The permissions to show, deduplicated by title, personal ones first.
pub fn shown(names: &[String]) -> Vec<Shown> {
    let mut v: Vec<Shown> = vec![];
    for n in names {
        if let Some(s) = describe(n)
            && !v.iter().any(|x| x.title == s.title)
        {
            v.push(s);
        }
    }
    v.sort_by_key(|s| (!s.sensitive, s.title.clone()));
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        let v = shown(&[
            "android.permission.INTERNET".into(),
            "android.permission.CAMERA".into(),
            "android.permission.READ_EXTERNAL_STORAGE".into(),
            "android.permission.WRITE_EXTERNAL_STORAGE".into(),
            "android.permission.READ_SYNC_SETTINGS".into(),
            "android.permission.FOREGROUND_SERVICE_DATA_SYNC".into(),
            "com.example.app.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION".into(),
        ]);
        let t: Vec<&str> = v.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(
            t,
            [
                "Camera",
                "Shared storage",
                "Full network access",
                "Read sync settings"
            ]
        );
    }
}
