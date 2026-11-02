//! Phone mode, used when the profile has the device [`Display`]. It shows
//! Android's display 0, with SystemUI and the image's launcher, in one desktop
//! window, and display 0 resizes with that window. Windowed profiles get a
//! window per app task instead (docs/display.md, "Phone mode"). The composer,
//! window manager and init read these properties.

use std::path::Path;

use anyhow::Result;

use crate::profile::{Display, Settings};

/// Image files that the overlay blanks for windowed profiles but phone mode
/// keeps. This is the launcher. A windowed profile's home is the system app's
/// empty activity, and the desktop launches apps.
pub const KEEPS: &[&str] = &["system_ext/priv-app/Launcher3QuickStep"];

/// Returns the display properties for a boot with `settings`. Display 0 boots
/// as a phone, with buttons that a mouse can click. The window manager then
/// gives it its window's size, and the navigation that suits that size.
pub fn props(settings: &Settings) -> String {
    let Some((w, h, dpi)) = settings.display.screen() else {
        return "vendor.innkeeper.mode=windowed\nvendor.innkeeper.nav=gestural\n".into();
    };
    format!(
        "vendor.innkeeper.mode=phone\nvendor.innkeeper.hwc.primary={w}x{h}\n\
         ro.sf.lcd_density={dpi}\nvendor.innkeeper.nav=threebutton\n"
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
        s.display = Display::Device;
        let p = props(&s);
        assert!(p.contains("vendor.innkeeper.mode=phone\n"));
        assert!(p.contains("vendor.innkeeper.hwc.primary=1080x2400\n"));
        assert!(p.contains("ro.sf.lcd_density=420\n"));
        assert!(p.contains("vendor.innkeeper.nav=threebutton\n"));
    }
}
