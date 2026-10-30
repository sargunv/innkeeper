//! Adwaita header titles use the interface font with bold weight.
use crate::title::font_preference::FontPreference;

pub(crate) fn titlebar_font() -> FontPreference {
    FontPreference {
        name: "Adwaita Sans,sans-serif".into(),
        style: Some("Bold".into()),
        pt_size: 11.,
    }
}
