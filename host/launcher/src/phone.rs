//! Phone mode ([`Mode::Phone`]): Android's own display 0, with SystemUI and
//! the image's launcher, in one desktop window the size of a device preset,
//! instead of a window per app task (docs/display.md, "Phone mode"). The
//! composer, window manager and init read these properties.

use std::path::Path;

use anyhow::Result;

use crate::profile::{Mode, Settings};

/// What the overlay blanks for windowed profiles that a phone keeps from the
/// image: the launcher (windowed profiles' home is the system app's empty
/// activity; the desktop launches apps).
pub const KEEPS: &[&str] = &["system_ext/priv-app/Launcher3QuickStep"];

/// The properties a boot of `settings` gets.
pub fn props(settings: &Settings) -> String {
    match settings.mode {
        Mode::Windowed => "vendor.linuxapk.mode=windowed\nvendor.linuxapk.nav=gestural\n".into(),
        Mode::Phone => {
            let (w, h, dpi) = settings.size.screen();
            // A large screen has a taskbar, which crash-loops in three-button
            // mode (android/overlay); a phone gets buttons a mouse can click.
            let nav = if settings.size.large() {
                "gestural"
            } else {
                "threebutton"
            };
            format!(
                "vendor.linuxapk.mode=phone\nvendor.linuxapk.hwc.primary={w}x{h}\n\
                 ro.sf.lcd_density={dpi}\nvendor.linuxapk.nav={nav}\n"
            )
        }
    }
}

/// The mode of the profile with data directory `data`.
pub fn mode(data: &Path) -> Result<Mode> {
    Ok(Settings::load(data)?.mode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::Size;

    #[test]
    fn props_follow_mode_and_size() {
        let mut s = Settings::default();
        assert_eq!(
            props(&s),
            "vendor.linuxapk.mode=windowed\nvendor.linuxapk.nav=gestural\n"
        );
        s.mode = Mode::Phone;
        let p = props(&s);
        assert!(p.contains("vendor.linuxapk.hwc.primary=1080x2400\n"));
        assert!(p.contains("ro.sf.lcd_density=420\n"));
        assert!(p.contains("vendor.linuxapk.nav=threebutton\n"));
        s.size = Size::MediumTablet;
        let p = props(&s);
        assert!(p.contains("vendor.linuxapk.hwc.primary=2560x1600\n"));
        assert!(p.contains("vendor.linuxapk.nav=gestural\n"));
    }
}
