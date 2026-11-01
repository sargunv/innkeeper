//! Phone mode, used when the profile has a phone [`Display`]. It shows
//! Android's display 0, with SystemUI and the image's launcher, in one desktop
//! window the size of a device preset. Windowed profiles get a window per app
//! task instead (docs/display.md, "Phone mode"). The composer, window manager
//! and init read these properties.

use std::path::Path;

use anyhow::Result;

use crate::profile::{Display, Settings};

/// Image files that the overlay blanks for windowed profiles but phone mode
/// keeps. This is the launcher. A windowed profile's home is the system app's
/// empty activity, and the desktop launches apps.
pub const KEEPS: &[&str] = &["system_ext/priv-app/Launcher3QuickStep"];

/// Returns the display properties for a boot with `settings`.
pub fn props(settings: &Settings) -> String {
    let Some((w, h, dpi)) = settings.display.screen() else {
        return "vendor.innkeeper.mode=windowed\nvendor.innkeeper.nav=gestural\n".into();
    };
    // A large screen has a taskbar, which crash-loops in three-button mode
    // (android/overlay). A phone gets buttons that a mouse can click.
    let nav = if settings.display.large() {
        "gestural"
    } else {
        "threebutton"
    };
    format!(
        "vendor.innkeeper.mode=phone\nvendor.innkeeper.hwc.primary={w}x{h}\n\
         ro.sf.lcd_density={dpi}\nvendor.innkeeper.nav={nav}\n"
    )
}

/// The display of the profile with data directory `data`.
pub fn display(data: &Path) -> Result<Display> {
    Ok(Settings::load(data)?.display)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn props_follow_the_display() {
        let mut s = Settings::default();
        assert_eq!(
            props(&s),
            "vendor.innkeeper.mode=windowed\nvendor.innkeeper.nav=gestural\n"
        );
        s.display = Display::MediumPhone;
        let p = props(&s);
        assert!(p.contains("vendor.innkeeper.hwc.primary=1080x2400\n"));
        assert!(p.contains("ro.sf.lcd_density=420\n"));
        assert!(p.contains("vendor.innkeeper.nav=threebutton\n"));
        s.display = Display::MediumTablet;
        let p = props(&s);
        assert!(p.contains("vendor.innkeeper.hwc.primary=2560x1600\n"));
        assert!(p.contains("vendor.innkeeper.nav=gestural\n"));
    }
}
