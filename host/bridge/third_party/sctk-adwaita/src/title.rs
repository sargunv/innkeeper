use tiny_skia::{Color, Pixmap};

mod ab_glyph_renderer;
mod config;
mod font_preference;

#[derive(Debug)]
pub struct TitleText {
    imp: ab_glyph_renderer::AbGlyphTitleText,
}

impl TitleText {
    pub fn new(color: Color) -> Option<Self> {
        Some(Self {
            imp: ab_glyph_renderer::AbGlyphTitleText::new(color)?,
        })
    }

    pub fn update_scale(&mut self, scale: u32) {
        self.imp.update_scale(scale)
    }

    pub fn update_title(&mut self, title: impl Into<String>) {
        self.imp.update_title(title)
    }

    pub fn update_color(&mut self, color: Color) {
        self.imp.update_color(color)
    }

    pub fn pixmap(&self) -> Option<&Pixmap> {
        self.imp.pixmap()
    }
}
