//! Plain-language names for Android permissions on the install and app pages.

/// A permission as the install page shows it.
pub struct Shown {
    pub title: String,
    /// The permission gives access to personal data. These permissions list
    /// first and get a warning icon.
    pub sensitive: bool,
}

/// Describes `name`, such as `android.permission.CAMERA`. An unknown platform
/// permission gets its constant in readable form, so `READ_SYNC_SETTINGS`
/// becomes "Read sync settings". An app's own permission, such as
/// `com.example.app.C2D_MESSAGE`, returns `None` because its name means nothing
/// to the user.
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

/// Returns the permissions to show, deduplicated by title, with personal ones
/// first.
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

/// Returns a title and subtitle for each runtime permission in `names`, in
/// order, for the app page. The subtitle is the permission's constant when two
/// permissions share a title, and empty otherwise. For example,
/// `READ_CONTACTS` and `WRITE_CONTACTS` are both "Contacts and accounts".
pub fn titles<'a>(names: impl Iterator<Item = &'a str>) -> Vec<(String, String)> {
    let rows: Vec<(String, &str)> = names
        .map(|n| (describe(n).map_or_else(|| n.to_string(), |s| s.title), n))
        .collect();
    rows.iter()
        .map(|(title, name)| {
            let shared = rows.iter().filter(|(t, _)| t == title).count() > 1;
            let constant = name.rsplit('.').next().unwrap_or(name);
            (
                title.clone(),
                if shared {
                    constant.into()
                } else {
                    String::new()
                },
            )
        })
        .collect()
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

    #[test]
    fn colliding_titles_show_their_constant() {
        let t = titles(
            [
                "android.permission.CAMERA",
                "android.permission.READ_CONTACTS",
                "android.permission.WRITE_CONTACTS",
                "com.example.app.OWN",
            ]
            .into_iter(),
        );
        assert_eq!(
            t,
            [
                ("Camera".into(), String::new()),
                ("Contacts and accounts".into(), "READ_CONTACTS".into()),
                ("Contacts and accounts".into(), "WRITE_CONTACTS".into()),
                ("com.example.app.OWN".into(), String::new()),
            ]
        );
    }
}
