use smithay_client_toolkit::reexports::csd_frame::{WindowManagerCapabilities, WindowState};
use tiny_skia::{FillRule, PathBuilder, Pixmap, PixmapMut, PixmapPaint, Rect, Stroke, Transform};

use crate::{theme::ColorMap, Location, SkiaResult};

/// The size of the button on the header bar in logical points.
const BUTTON_SIZE: f32 = 34.;
const BUTTON_MARGIN: f32 = 6.;
const BUTTON_SPACING: f32 = 0.;
const BUTTON_TOP: f32 = (crate::theme::HEADER_SIZE as f32 - BUTTON_SIZE) / 2.;

#[derive(Debug)]
pub(crate) struct Buttons {
    // Sorted by order vec of buttons for the left and right sides
    buttons_left: Vec<Button>,
    buttons_right: Vec<Button>,
    layout_config: Option<String>,
}

type ButtonLayout = (Vec<Button>, Vec<Button>);

impl Default for Buttons {
    fn default() -> Self {
        let (buttons_left, buttons_right) = Buttons::get_default_buttons_layout();

        Self {
            buttons_left,
            buttons_right,
            layout_config: None,
        }
    }
}

impl Buttons {
    pub fn new(layout_config: Option<String>) -> Self {
        match Buttons::parse_button_layout(layout_config.clone()) {
            Some((buttons_left, buttons_right)) => Self {
                buttons_left,
                buttons_right,
                layout_config,
            },
            _ => Self::default(),
        }
    }

    pub fn set_fullscreen(&mut self, fullscreen: bool) {
        for button in self
            .buttons_left
            .iter_mut()
            .chain(self.buttons_right.iter_mut())
        {
            button.width = if fullscreen && button.kind == ButtonKind::Fullscreen {
                164.
            } else {
                BUTTON_SIZE
            };
        }
    }

    /// Rearrange the buttons with the new width.
    pub fn arrange(&mut self, width: u32, margin_h: f32) {
        let mut left_x = BUTTON_MARGIN + margin_h;
        let mut right_x = width as f32 - BUTTON_MARGIN;

        for button in &mut self.buttons_left {
            button.offset = left_x;

            // Add the button size plus spacing
            left_x += button.width + BUTTON_SPACING;
        }

        for button in &mut self.buttons_right {
            // Subtract the button size.
            right_x -= button.width;

            // Update it
            button.offset = right_x;

            // Subtract spacing for the next button.
            right_x -= BUTTON_SPACING;
        }
        for button in &mut self.buttons_right {
            button.visible = button.x() >= margin_h && button.end_x() <= width as f32;
        }
        let right_start = self.right_buttons_start_x().unwrap_or(width as f32);
        for button in &mut self.buttons_left {
            button.visible =
                button.x() >= margin_h && button.end_x() <= right_start - BUTTON_SPACING;
        }
    }

    /// Find the coordinate of the button.
    pub fn find_button(&self, x: f64, y: f64) -> Location {
        let x = x as f32;
        let y = y as f32;
        let buttons = self.buttons_left.iter().chain(self.buttons_right.iter());

        for button in buttons {
            if button.visible && button.contains(x, y) {
                return Location::Button(button.kind);
            }
        }

        Location::Head
    }

    pub fn update_wm_capabilities(&mut self, wm_capabilites: WindowManagerCapabilities) {
        let supports_maximize = wm_capabilites.contains(WindowManagerCapabilities::MAXIMIZE);
        let supports_minimize = wm_capabilites.contains(WindowManagerCapabilities::MINIMIZE);

        self.update_buttons(supports_maximize, supports_minimize);
    }

    pub fn update_buttons(&mut self, supports_maximize: bool, supports_minimize: bool) {
        let is_supported = |button: &Button| match button.kind {
            ButtonKind::Back | ButtonKind::Fullscreen | ButtonKind::Close => true,
            ButtonKind::Maximize => supports_maximize,
            ButtonKind::Minimize => supports_minimize,
        };

        let (buttons_left, buttons_right) =
            Buttons::parse_button_layout(self.layout_config.clone())
                .unwrap_or_else(Buttons::get_default_buttons_layout);

        self.buttons_left = buttons_left.into_iter().filter(is_supported).collect();
        self.buttons_right = buttons_right.into_iter().filter(is_supported).collect();
    }

    pub fn right_buttons_start_x(&self) -> Option<f32> {
        self.buttons_right
            .iter()
            .rev()
            .find(|b| b.visible)
            .map(|button| button.x())
    }

    pub fn left_buttons_end_x(&self) -> Option<f32> {
        self.buttons_left
            .iter()
            .rev()
            .find(|b| b.visible)
            .map(|button| button.end_x())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        scale: f32,
        colors: &ColorMap,
        mouse_location: Location,
        pressed: Location,
        fullscreen_label: Option<&Pixmap>,
        pixmap: &mut PixmapMut,
        resizable: bool,
        state: &WindowState,
    ) {
        for button in self
            .buttons_left
            .iter()
            .chain(self.buttons_right.iter())
            .filter(|b| b.visible)
        {
            button.draw(
                scale,
                colors,
                mouse_location,
                pressed,
                fullscreen_label,
                pixmap,
                resizable,
                state,
            );
        }
    }

    fn parse_button_layout(layout: Option<String>) -> Option<ButtonLayout> {
        let layout = layout?;
        let (left, right) = layout.split_once(':')?;
        if right.contains(':') {
            return None;
        }
        let mut seen = Vec::new();
        let mut parse = |side: &str| -> Option<Vec<Button>> {
            let mut buttons = Vec::new();
            for token in side.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                let kind = match token {
                    "close" => ButtonKind::Close,
                    "maximize" => ButtonKind::Maximize,
                    "minimize" => ButtonKind::Minimize,
                    // A desktop menu entry is not a window control.
                    "appmenu" | "menu" => continue,
                    _ => return None,
                };
                if !seen.contains(&kind) {
                    seen.push(kind);
                    buttons.push(Button::new(kind));
                }
            }
            Some(buttons)
        };
        let mut left = parse(left)?;
        let mut right = parse(right)?;
        // Back is always leading. Fullscreen joins the configured controls.
        if right.is_empty() && !left.is_empty() {
            left.push(Button::new(ButtonKind::Fullscreen));
        } else {
            right.insert(0, Button::new(ButtonKind::Fullscreen));
        }
        left.insert(0, Button::new(ButtonKind::Back));
        right.reverse();
        Some((left, right))
    }

    fn get_default_buttons_layout() -> ButtonLayout {
        (
            vec![Button::new(ButtonKind::Back)],
            vec![
                Button::new(ButtonKind::Close),
                Button::new(ButtonKind::Maximize),
                Button::new(ButtonKind::Minimize),
                Button::new(ButtonKind::Fullscreen),
            ],
        )
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Button {
    /// The button offset into the header bar canvas.
    offset: f32,
    /// The kind of the button.
    kind: ButtonKind,
    width: f32,
    visible: bool,
}

impl Button {
    pub fn new(kind: ButtonKind) -> Self {
        Self {
            offset: 0.,
            kind,
            width: BUTTON_SIZE,
            visible: true,
        }
    }

    pub fn radius(&self) -> f32 {
        BUTTON_SIZE / 2.0
    }

    pub fn x(&self) -> f32 {
        self.offset
    }

    pub fn center_x(&self) -> f32 {
        self.offset + self.radius()
    }

    pub fn center_y(&self) -> f32 {
        BUTTON_TOP + self.radius()
    }

    pub fn end_x(&self) -> f32 {
        self.offset + self.width
    }

    fn contains(&self, x: f32, y: f32) -> bool {
        x > self.offset
            && x < self.offset + self.width
            && y > BUTTON_TOP
            && y < BUTTON_TOP + BUTTON_SIZE
    }

    #[allow(clippy::too_many_arguments)]
    pub fn draw(
        &self,
        scale: f32,
        colors: &ColorMap,
        mouse_location: Location,
        pressed: Location,
        fullscreen_label: Option<&Pixmap>,
        pixmap: &mut PixmapMut,
        resizable: bool,
        state: &WindowState,
    ) -> SkiaResult {
        let button_bg = if mouse_location == Location::Button(self.kind)
            && (resizable || self.kind != ButtonKind::Maximize)
        {
            if pressed == mouse_location {
                colors.button_pressed_paint()
            } else {
                colors.button_hover_paint()
            }
        } else if self.kind == ButtonKind::Back {
            tiny_skia::Paint {
                shader: tiny_skia::Shader::SolidColor(tiny_skia::Color::TRANSPARENT),
                ..Default::default()
            }
        } else {
            colors.button_idle_paint()
        };

        // Convert to pixels.
        let x = self.center_x() * scale;
        let y = self.center_y() * scale;
        let radius = if self.kind == ButtonKind::Back || self.width > BUTTON_SIZE {
            self.radius()
        } else {
            12.
        } * scale;

        // Draw the button background.
        let circle = if self.width > BUTTON_SIZE {
            rounded_button(
                self.offset * scale,
                BUTTON_TOP * scale,
                self.width * scale,
                BUTTON_SIZE * scale,
                radius,
            )?
        } else {
            PathBuilder::from_circle(x, y, radius)?
        };
        pixmap.fill_path(
            &circle,
            &button_bg,
            FillRule::Winding,
            Transform::identity(),
            None,
        );

        let mut button_icon_paint = colors.button_icon_paint();
        // Do AA only for diagonal lines.
        button_icon_paint.anti_alias = matches!(self.kind, ButtonKind::Close | ButtonKind::Back);

        // Draw the icon.
        match self.kind {
            ButtonKind::Back => {
                let mut path = PathBuilder::new();
                path.move_to(x, y - 5.0 * scale);
                path.line_to(x - 5.0 * scale, y);
                path.line_to(x, y + 5.0 * scale);
                path.move_to(x - 5.0 * scale, y);
                path.line_to(x + 6.0 * scale, y);
                pixmap.stroke_path(
                    &path.finish()?,
                    &button_icon_paint,
                    &Stroke {
                        width: 1.5 * scale,
                        ..Default::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
            ButtonKind::Fullscreen => {
                if self.width > BUTTON_SIZE {
                    if let Some(label) = fullscreen_label {
                        pixmap.draw_pixmap(
                            (self.offset * scale + BUTTON_SIZE * scale) as i32,
                            (y - label.height() as f32 / 2.) as i32,
                            label.as_ref(),
                            &PixmapPaint::default(),
                            Transform::identity(),
                            None,
                        );
                    }
                }
                // Inward corners indicate Leave fullscreen; outward corners indicate Enter.
                let (corner, arm) = if state.contains(WindowState::FULLSCREEN) {
                    (2., 5.)
                } else {
                    (5., 2.)
                };
                let mut path = PathBuilder::new();
                for (dx, dy) in [(-1., -1.), (1., -1.), (-1., 1.), (1., 1.)] {
                    path.move_to(x + dx * arm * scale, y + dy * corner * scale);
                    path.line_to(x + dx * corner * scale, y + dy * corner * scale);
                    path.line_to(x + dx * corner * scale, y + dy * arm * scale);
                }
                pixmap.stroke_path(
                    &path.finish()?,
                    &button_icon_paint,
                    &Stroke {
                        width: scale,
                        ..Default::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
            ButtonKind::Close => {
                let x_icon = {
                    let size = 4.5 * scale;
                    let mut pb = PathBuilder::new();

                    {
                        let sx = x - size;
                        let sy = y - size;
                        let ex = x + size;
                        let ey = y + size;

                        pb.move_to(sx, sy);
                        pb.line_to(ex, ey);
                        pb.close();
                    }

                    {
                        let sx = x - size;
                        let sy = y + size;
                        let ex = x + size;
                        let ey = y - size;

                        pb.move_to(sx, sy);
                        pb.line_to(ex, ey);
                        pb.close();
                    }

                    pb.finish()?
                };

                pixmap.stroke_path(
                    &x_icon,
                    &button_icon_paint,
                    &Stroke {
                        width: 1.5 * scale,
                        ..Default::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
            ButtonKind::Maximize => {
                let path2 = {
                    let size = 10.0 * scale;
                    let hsize = size / 2.0;
                    let mut pb = PathBuilder::new();

                    let x = x - hsize;
                    let y = y - hsize;
                    if state.contains(WindowState::MAXIMIZED) {
                        let offset = 2.0 * scale;
                        if let Some(rect) =
                            Rect::from_xywh(x, y + offset, size - offset, size - offset)
                        {
                            pb.push_rect(rect);
                            pb.move_to(rect.left() + offset, rect.top() - offset);
                            pb.line_to(rect.right() + offset, rect.top() - offset);
                            pb.line_to(rect.right() + offset, rect.bottom() - offset + 0.5);
                        }
                    } else if let Some(rect) = Rect::from_xywh(x, y, size, size) {
                        pb.push_rect(rect);
                    }

                    pb.finish()?
                };

                pixmap.stroke_path(
                    &path2,
                    &button_icon_paint,
                    &Stroke {
                        width: 1.0 * scale,
                        ..Default::default()
                    },
                    Transform::identity(),
                    None,
                );
            }
            ButtonKind::Minimize => {
                let len = 10.0 * scale;
                let hlen = len / 2.0;
                pixmap.fill_rect(
                    Rect::from_xywh(x - hlen, y + hlen, len, scale)?,
                    &button_icon_paint,
                    Transform::identity(),
                    None,
                );
            }
        }

        Some(())
    }
}

fn rounded_button(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut p = PathBuilder::new();
    p.move_to(x + r, y);
    p.line_to(x + w - r, y);
    p.quad_to(x + w, y, x + w, y + r);
    p.line_to(x + w, y + h - r);
    p.quad_to(x + w, y + h, x + w - r, y + h);
    p.line_to(x + r, y + h);
    p.quad_to(x, y + h, x, y + h - r);
    p.line_to(x, y + r);
    p.quad_to(x, y, x + r, y);
    p.close();
    p.finish()
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ButtonKind {
    Back,
    Fullscreen,
    Close,
    Maximize,
    Minimize,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn kinds(buttons: &[Button]) -> Vec<ButtonKind> {
        buttons.iter().map(|b| b.kind).collect()
    }

    #[test]
    fn portal_layouts_preserve_sides_order_and_single_controls() {
        use ButtonKind::*;
        for (layout, left, right) in [
            (
                ":minimize,maximize,close",
                vec![Back],
                vec![Close, Maximize, Minimize, Fullscreen],
            ),
            (
                "close,minimize,maximize:",
                vec![Back, Close, Minimize, Maximize, Fullscreen],
                vec![],
            ),
            (
                "close:maximize",
                vec![Back, Close],
                vec![Maximize, Fullscreen],
            ),
            (
                "close,close:close,minimize",
                vec![Back, Close],
                vec![Minimize, Fullscreen],
            ),
            (" : ", vec![Back], vec![Fullscreen]),
            ("appmenu:close", vec![Back], vec![Close, Fullscreen]),
        ] {
            let b = Buttons::new(Some(layout.into()));
            assert_eq!(kinds(&b.buttons_left), left, "{layout}");
            assert_eq!(kinds(&b.buttons_right), right, "{layout}");
        }
        for layout in [
            None,
            Some("garbage".into()),
            Some("a:b:c".into()),
            Some(":unknown".into()),
        ] {
            let b = Buttons::new(layout);
            assert_eq!(
                kinds(&b.buttons_right),
                vec![Close, Maximize, Minimize, Fullscreen]
            );
        }
    }

    #[test]
    fn metrics_capabilities_and_fullscreen_label_share_hit_targets() {
        let mut b = Buttons::new(Some("close,minimize,maximize:".into()));
        b.update_wm_capabilities(WindowManagerCapabilities::empty());
        b.set_fullscreen(true);
        b.arrange(500, 1.);
        assert_eq!(
            kinds(&b.buttons_left),
            vec![ButtonKind::Back, ButtonKind::Close, ButtonKind::Fullscreen]
        );
        for button in &b.buttons_left {
            assert_eq!(
                b.find_button(button.center_x() as f64, 23.5),
                Location::Button(button.kind)
            );
            assert_eq!(b.find_button(button.center_x() as f64, 5.), Location::Head);
        }
        assert_eq!(b.buttons_left.last().map(|b| b.width), Some(164.));
        b.set_fullscreen(false);
        b.arrange(1, 0.);
        assert!(b.buttons_left.iter().all(|b| b.x().is_finite()));
    }
}
