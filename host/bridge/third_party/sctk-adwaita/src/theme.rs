pub use tiny_skia::Color;
use tiny_skia::{Paint, Shader};

// https://gitlab.gnome.org/GNOME/gtk/-/blob/1bf88f1d81043fd99740e2f91e56ade7ede7303b/gtk/gtkwindow.c#L165
pub(crate) const RESIZE_HANDLE_SIZE: u32 = 12;
// https://gitlab.gnome.org/GNOME/gtk/-/blob/1bf88f1d81043fd99740e2f91e56ade7ede7303b/gtk/gtkwindow.c#L166
pub(crate) const RESIZE_HANDLE_CORNER_SIZE: u32 = 24;
pub(crate) const HEADER_SIZE: u32 = 47;
pub(crate) const CORNER_RADIUS: u32 = 12;

pub(crate) fn header_height(hide_header: bool) -> u32 {
    if hide_header {
        0
    } else {
        HEADER_SIZE
    }
}

/// Edge is a border + shadows
pub(crate) fn edge_size(hide_border: bool) -> u32 {
    crate::shadow::SHADOW_SIZE + border_size(hide_border)
}

/// Just border without shadows
pub(crate) fn border_size(hide_border: bool) -> u32 {
    if hide_border {
        0
    } else {
        1
    }
}

/// The color theme to use with the decorations frame.
#[derive(Debug, Clone)]
pub struct ColorTheme {
    pub active: ColorMap,
    pub inactive: ColorMap,
}

impl ColorTheme {
    /// Predefined light variant, which aims to replecate Adwaita theme.
    pub fn light() -> Self {
        Self::palette(0xeb, 0xfa, 0x2e)
    }

    /// Adwaita's dark headerbar and backdrop colours (stored in shm BGR order).
    pub fn dark() -> Self {
        Self::palette(0x30, 0x24, 0xff)
    }

    fn palette(header: u8, backdrop: u8, foreground: u8) -> Self {
        let map = |bg| ColorMap {
            headerbar: Color::from_rgba8(bg, bg, bg, 255),
            button_idle: Color::from_rgba8(foreground, foreground, foreground, 20),
            button_hover: Color::from_rgba8(foreground, foreground, foreground, 38),
            button_pressed: Color::from_rgba8(foreground, foreground, foreground, 64),
            button_icon: Color::from_rgba8(foreground, foreground, foreground, 255),
            border_color: Color::from_rgba8(0, 0, 0, 35),
            font_color: Color::from_rgba8(foreground, foreground, foreground, 255),
        };
        Self {
            active: map(header),
            inactive: map(backdrop),
        }
    }

    pub(crate) fn for_state(&self, active: bool) -> &ColorMap {
        if active {
            &self.active
        } else {
            &self.inactive
        }
    }
}

impl Default for ColorTheme {
    fn default() -> Self {
        Self::light()
    }
}

/// The color map for various decorcation parts.
#[derive(Debug, Clone)]
pub struct ColorMap {
    pub headerbar: Color,
    pub button_idle: Color,
    pub button_hover: Color,
    pub button_pressed: Color,
    pub button_icon: Color,
    pub border_color: Color,
    pub font_color: Color,
}

impl ColorMap {
    pub(crate) fn headerbar_paint(&self) -> Paint<'_> {
        Paint {
            shader: Shader::SolidColor(self.headerbar),
            anti_alias: true,
            ..Default::default()
        }
    }

    pub(crate) fn button_idle_paint(&self) -> Paint<'_> {
        Paint {
            shader: Shader::SolidColor(self.button_idle),
            anti_alias: true,
            ..Default::default()
        }
    }

    pub(crate) fn button_hover_paint(&self) -> Paint<'_> {
        Paint {
            shader: Shader::SolidColor(self.button_hover),
            anti_alias: true,
            ..Default::default()
        }
    }

    pub(crate) fn button_pressed_paint(&self) -> Paint<'_> {
        Paint {
            shader: Shader::SolidColor(self.button_pressed),
            anti_alias: true,
            ..Default::default()
        }
    }

    pub(crate) fn button_icon_paint(&self) -> Paint<'_> {
        Paint {
            shader: Shader::SolidColor(self.button_icon),
            ..Default::default()
        }
    }

    pub(crate) fn border_paint(&self) -> Paint<'_> {
        Paint {
            shader: Shader::SolidColor(self.border_color),
            ..Default::default()
        }
    }
}
