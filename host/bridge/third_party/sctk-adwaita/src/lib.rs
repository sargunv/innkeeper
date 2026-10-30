//! Vendored from sctk-adwaita 0.12.0 (https://github.com/PolyMeilex/sctk-adwaita).
//! Upstream commit: 1bddc2264a71ae3897903ec8a7d7b13b834ff1b1.
//! Copyright Bartłomiej Maryńczak and contributors; MIT license in ../LICENSE.
//!
//! Linux.apk changes for upstream sync: buttons.rs adds and draws Back and Fullscreen;
//! pointer.rs tracks completed custom button clicks; on_frame_click returns them alongside
//! SCTK window actions. Header metrics, symbolic controls, states and shadows match
//! Adwaita; fullscreen reveal animates the header without altering its title.
//! The bridge supplies live portal appearance and button-layout settings.
//! ab_glyph_renderer.rs uses the system font without the bundled binary fallback;
//! only the default ab_glyph renderer is kept. No fonts or snapshot fixtures are
//! vendored. Upstream rendering snapshot tests are omitted; our button behaviour
//! tests remain. Clippy uses its current spelling of clamp for border sizes.
//!
#![deny(
    clippy::print_stderr,
    clippy::print_stdout,
    clippy::dbg_macro,
    clippy::exit,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::string_slice
)]

use std::error::Error;
use std::mem;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tiny_skia::{
    Color, FillRule, Mask, Path, PathBuilder, Pixmap, PixmapMut, PixmapPaint, Point, Rect,
    Transform,
};

use smithay_client_toolkit::reexports::client::backend::ObjectId;
use smithay_client_toolkit::reexports::client::protocol::wl_shm;
use smithay_client_toolkit::reexports::client::protocol::wl_subsurface::WlSubsurface;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Dispatch, Proxy, QueueHandle};
use smithay_client_toolkit::reexports::csd_frame::{
    CursorIcon, DecorationsFrame, FrameAction, FrameClick, WindowManagerCapabilities, WindowState,
};

use smithay_client_toolkit::compositor::{CompositorState, Region, SurfaceData};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shm::{slot::SlotPool, Shm};
use smithay_client_toolkit::subcompositor::SubcompositorState;
use smithay_client_toolkit::subcompositor::SubsurfaceData;

mod buttons;
mod parts;
mod pointer;
mod shadow;
pub mod theme;
mod title;
mod wl_typed;

use crate::parts::{LayoutConfig, PartId};
use crate::theme::{ColorMap, ColorTheme, CORNER_RADIUS, HEADER_SIZE};

use buttons::Buttons;
use parts::DecorationParts;
use pointer::{Location, MouseState};
use shadow::Shadow;
use title::TitleText;
use wl_typed::WlTyped;

/// XXX this is not result, so `must_use` when needed.
type SkiaResult = Option<()>;

/// A header action, including Linux.apk's Android Back button.
#[derive(Debug)]
pub enum Action {
    Back,
    ToggleFullscreen,
    Window(FrameAction),
}

/// A simple set of decorations
#[derive(Debug)]
pub struct AdwaitaFrame<State> {
    /// The base surface used to create the window.
    base_surface: WlTyped<WlSurface, SurfaceData<()>>,

    compositor: Arc<CompositorState>,

    /// Subcompositor to create/drop subsurfaces ondemand.
    subcompositor: Arc<SubcompositorState>,

    /// Queue handle to perform object creation.
    queue_handle: QueueHandle<State>,

    /// The drawable decorations, `None` when hidden.
    decorations: Option<DecorationParts>,

    /// Memory pool to allocate the buffers for the decorations.
    pool: SlotPool,

    /// Whether the frame should be redrawn.
    dirty: bool,

    /// Whether the drawing should be synced with the main surface.
    should_sync: bool,

    /// Scale factor used for the surface.
    scale_factor: u32,

    /// Whether the frame is resizable.
    resizable: bool,

    buttons: Buttons,
    state: WindowState,
    wm_capabilities: WindowManagerCapabilities,
    mouse: MouseState,
    theme: ColorTheme,
    title: Option<String>,
    title_text: Option<TitleText>,
    shadow: Shadow,

    /// Draw decorations but without the titlebar
    hide_titlebar: bool,
    hide_border: bool,
    fullscreen_revealed: bool,
    reveal_started: Option<Instant>,
    fullscreen_label: Option<TitleText>,
    button_layout: Option<String>,

    width: NonZeroU32,
    height: NonZeroU32,
}

impl<State> AdwaitaFrame<State>
where
    State: Dispatch<WlSurface, SurfaceData<()>> + Dispatch<WlSubsurface, SubsurfaceData> + 'static,
{
    pub fn new(
        base_surface: &impl WaylandSurface,
        shm: &Shm,
        compositor: Arc<CompositorState>,
        subcompositor: Arc<SubcompositorState>,
        queue_handle: QueueHandle<State>,
        frame_config: FrameConfig,
    ) -> Result<Self, Box<dyn Error>> {
        let base_surface = WlTyped::wrap::<State>(base_surface.wl_surface().clone());

        let pool = SlotPool::new(1, shm)?;

        let decorations = Some(DecorationParts::new(
            &base_surface,
            &subcompositor,
            &queue_handle,
        ));

        let theme = frame_config.theme;

        Ok(AdwaitaFrame {
            base_surface,
            decorations,
            pool,
            compositor,
            subcompositor,
            queue_handle,
            dirty: true,
            scale_factor: 1,
            should_sync: true,
            title: None,
            title_text: TitleText::new(theme.active.font_color),
            theme,
            buttons: Buttons::new(frame_config.button_layout.clone()),
            mouse: Default::default(),
            state: WindowState::empty(),
            wm_capabilities: WindowManagerCapabilities::all(),
            resizable: true,
            shadow: Shadow::default(),
            hide_titlebar: frame_config.hide_titlebar,
            hide_border: frame_config.hide_border,
            fullscreen_revealed: false,
            reveal_started: None,
            fullscreen_label: TitleText::new(theme::Color::BLACK),
            button_layout: frame_config.button_layout,
            width: NonZeroU32::MIN,
            height: NonZeroU32::MIN,
        })
    }

    /// Tick only while a revealed header is sliding; the caller sleeps when idle.
    pub fn animate(&mut self) {
        if let Some(started) = self.reveal_started {
            if started.elapsed() >= REVEAL_DURATION {
                self.reveal_started = None;
            }
            self.dirty = true;
            self.draw();
        }
    }

    pub fn is_animating(&self) -> bool {
        self.reveal_started.is_some()
    }

    /// Handle the app button before delegating normal window actions to SCTK.
    pub fn on_frame_click(
        &mut self,
        timestamp: Duration,
        click: FrameClick,
        pressed: bool,
    ) -> Option<Action> {
        if self.is_hidden() {
            return None;
        }
        self.dirty = true;
        let location = self.pointer_location(self.header_offset());
        self.mouse.set_pressed(location, click, pressed);
        self.redraw_inner();
        if matches!(click, FrameClick::Normal) {
            if let Some(action) = self.mouse.button_click(location, pressed) {
                return Some(action);
            }
        }
        if self.state.contains(WindowState::FULLSCREEN) && location == Location::Head {
            return None;
        }
        self.on_click(timestamp, click, pressed).map(Action::Window)
    }

    /// Reset a touch press without dismissing revealed controls: after release they
    /// remain available for a second tap. Content touches/cancel use click_point_left.
    pub fn reset_touch_point(&mut self) {
        self.mouse.left();
    }

    /// Update the current frame config.
    pub fn set_config(&mut self, config: FrameConfig) {
        self.theme = config.theme;
        if self.button_layout != config.button_layout {
            self.button_layout = config.button_layout.clone();
            self.buttons = Buttons::new(config.button_layout);
            self.buttons.update_wm_capabilities(self.wm_capabilities);
            self.buttons
                .set_fullscreen(self.state.contains(WindowState::FULLSCREEN));
            self.buttons.arrange(
                self.width.get(),
                get_margin_h_lp(&self.state, self.hide_border),
            );
            self.mouse.left();
        }
        self.dirty = true;

        if self.hide_titlebar != config.hide_titlebar || self.hide_border != config.hide_border {
            self.hide_titlebar = config.hide_titlebar;
            self.hide_border = config.hide_border;

            let layout_config = self.layout_config();
            if let Some(decorations) = self.decorations.as_mut() {
                decorations.relayout(layout_config);
            }
        }
    }

    fn header_offset(&self) -> i32 {
        self.reveal_started
            .map(|t| reveal_offset(t.elapsed()))
            .unwrap_or(0)
    }

    fn pointer_location(&self, header_offset: i32) -> Location {
        let Some(decorations) = self.decorations.as_ref() else {
            return Location::None;
        };
        self.mouse.location(
            &self.buttons,
            header_offset,
            decorations.header().surface_rect.width,
            decorations.side_height(),
            self.hide_border,
        )
    }

    fn layout_config(&self) -> LayoutConfig {
        LayoutConfig {
            width: self.width.get(),
            height: self.height.get(),
            hide_titlebar: self.hide_titlebar,
            hide_border: self.hide_border || self.state.contains(WindowState::FULLSCREEN),
            hide_edges: self
                .state
                .intersects(WindowState::MAXIMIZED | WindowState::FULLSCREEN),
            fullscreen: self.state.contains(WindowState::FULLSCREEN),
            revealed: self.fullscreen_revealed,
        }
    }

    fn redraw_inner(&mut self) -> Option<bool> {
        let layout_config = self.layout_config();
        let reveal_offset = self.header_offset();
        let mouse_location = self.pointer_location(reveal_offset);
        let decorations = self.decorations.as_mut()?;

        // Reset the dirty bit.
        self.dirty = false;
        let should_sync = mem::take(&mut self.should_sync);

        decorations.show();

        if layout_config.hide_titlebar {
            decorations.hide_titlebar();
        }
        if layout_config.hide_edges {
            // Don't draw the borders and shadows.
            decorations.hide_edges();
        }

        let colors = self
            .theme
            .for_state(self.state.contains(WindowState::ACTIVATED));

        decorations.relayout(layout_config);

        if let Some(title_text) = self.title_text.as_mut() {
            let title = self.title.as_deref().unwrap_or_default();
            title_text.update_title(title);
            title_text.update_scale(self.scale_factor);
            title_text.update_color(colors.font_color);
        }

        if let Some(label) = self.fullscreen_label.as_mut() {
            label.update_title("Leave fullscreen");
            label.update_scale(self.scale_factor);
            label.update_color(colors.button_icon);
        }
        // Draw the borders.
        for (idx, part) in decorations.parts().filter(|(_, part)| !part.hide) {
            let scale = self.scale_factor;

            let mut rect = part.surface_rect;
            rect.width *= scale;
            rect.height *= scale;

            let (buffer, canvas) = match self.pool.create_buffer(
                rect.width as i32,
                rect.height as i32,
                rect.width as i32 * 4,
                wl_shm::Format::Argb8888,
            ) {
                Ok((buffer, canvas)) => (buffer, canvas),
                Err(_) => continue,
            };

            // Create the pixmap and fill with transparent color.
            let mut pixmap = PixmapMut::from_bytes(canvas, rect.width, rect.height)?;

            // Fill everything with transparent background, since we draw rounded corners and
            // do invisible borders to enlarge the input zone.
            pixmap.fill(Color::TRANSPARENT);

            // In fullscreen the transparent header keeps only its top-edge input strip.
            if !layout_config.fullscreen || layout_config.revealed {
                let mut draw = |target: &mut PixmapMut<'_>| {
                    draw_part(
                        idx,
                        rect,
                        target,
                        self.title_text.as_ref().map(|t| t.pixmap()).unwrap_or(None),
                        scale,
                        self.resizable,
                        &self.state,
                        colors,
                        &self.buttons,
                        mouse_location,
                        self.mouse.pressed_location(),
                        self.fullscreen_label.as_ref().and_then(|t| t.pixmap()),
                        layout_config.hide_border,
                        self.hide_titlebar,
                        &mut self.shadow,
                    )
                };
                if layout_config.fullscreen {
                    let mut overlay = Pixmap::new(rect.width, rect.height)?;
                    draw(&mut overlay.as_mut());
                    pixmap.draw_pixmap(
                        0,
                        reveal_offset * scale as i32,
                        overlay.as_ref(),
                        &PixmapPaint::default(),
                        Transform::identity(),
                        None,
                    );
                } else {
                    draw(&mut pixmap);
                }
            }

            // Debug fill all subsurfaces with solid colors
            if false {
                match idx {
                    PartId::Top => {
                        pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
                    }
                    PartId::Left => {
                        pixmap.fill(Color::from_rgba8(0, 0, 255, 255));
                    }
                    PartId::Right => {
                        pixmap.fill(Color::from_rgba8(0, 0, 255, 255));
                    }
                    PartId::Bottom => {
                        pixmap.fill(Color::from_rgba8(255, 0, 0, 255));
                    }
                    PartId::Header => {
                        pixmap.fill(Color::from_rgba8(0, 255, 255, 255));
                    }
                };
            }

            if should_sync {
                part.subsurface.set_sync();
            } else {
                part.subsurface.set_desync();
            }

            part.surface.set_buffer_scale(scale as i32);

            part.subsurface.set_position(rect.x, rect.y);
            buffer.attach_to(&part.surface).ok()?;

            if part.surface.version() >= 4 {
                part.surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            } else {
                part.surface.damage(0, 0, i32::MAX, i32::MAX);
            }

            if let Some(input_rect) = part.input_rect {
                let input_region = Region::new(&*self.compositor).ok()?;
                input_region.add(
                    input_rect.x,
                    input_rect.y,
                    input_rect.width as i32,
                    input_rect.height as i32,
                );

                part.surface
                    .set_input_region(Some(input_region.wl_region()));
            } else {
                part.surface.set_input_region(None);
            }

            part.surface.commit();
        }

        Some(should_sync)
    }
}

impl<State> DecorationsFrame for AdwaitaFrame<State>
where
    State: Dispatch<WlSurface, SurfaceData<()>> + Dispatch<WlSubsurface, SubsurfaceData> + 'static,
{
    fn update_state(&mut self, state: WindowState) {
        let difference = self.state.symmetric_difference(state);
        self.state = state;
        if difference.contains(WindowState::FULLSCREEN) {
            self.fullscreen_revealed = false;
            self.reveal_started = None;
            self.mouse.left();
            self.buttons
                .set_fullscreen(state.contains(WindowState::FULLSCREEN));
        }
        self.dirty |= difference.intersects(
            WindowState::ACTIVATED
                | WindowState::FULLSCREEN
                | WindowState::MAXIMIZED
                | WindowState::TILED,
        );
    }

    fn update_wm_capabilities(&mut self, wm_capabilities: WindowManagerCapabilities) {
        self.dirty |= self.wm_capabilities != wm_capabilities;
        self.wm_capabilities = wm_capabilities;
        self.buttons.update_wm_capabilities(wm_capabilities);
        self.buttons
            .set_fullscreen(self.state.contains(WindowState::FULLSCREEN));
    }

    fn set_hidden(&mut self, hidden: bool) {
        if hidden {
            self.dirty = false;
            let _ = self.pool.resize(1);
            self.decorations = None;
        } else if self.decorations.is_none() {
            self.decorations = Some(DecorationParts::new(
                &self.base_surface,
                &self.subcompositor,
                &self.queue_handle,
            ));
            self.dirty = true;
            self.should_sync = true;
        }
    }

    fn set_resizable(&mut self, resizable: bool) {
        self.dirty |= self.resizable != resizable;
        self.resizable = resizable;
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) {
        self.width = width;
        self.height = height;

        let layout_config = self.layout_config();

        let Some(decorations) = self.decorations.as_mut() else {
            log::error!("trying to resize the hidden frame.");
            return;
        };

        decorations.relayout(layout_config);
        self.buttons
            .arrange(width.get(), get_margin_h_lp(&self.state, self.hide_border));
        self.dirty = true;
        self.should_sync = true;
    }

    fn draw(&mut self) -> bool {
        self.redraw_inner().unwrap_or(true)
    }

    fn subtract_borders(
        &self,
        width: NonZeroU32,
        height: NonZeroU32,
    ) -> (Option<NonZeroU32>, Option<NonZeroU32>) {
        if self.decorations.is_none()
            || self.state.contains(WindowState::FULLSCREEN)
            || self.hide_titlebar
        {
            (Some(width), Some(height))
        } else {
            (
                Some(width),
                NonZeroU32::new(height.get().saturating_sub(HEADER_SIZE)),
            )
        }
    }

    fn add_borders(&self, width: u32, height: u32) -> (u32, u32) {
        if self.decorations.is_none()
            || self.state.contains(WindowState::FULLSCREEN)
            || self.hide_titlebar
        {
            (width, height)
        } else {
            (width, height + HEADER_SIZE)
        }
    }

    fn location(&self) -> (i32, i32) {
        if self.decorations.is_none()
            || self.state.contains(WindowState::FULLSCREEN)
            || self.hide_titlebar
        {
            (0, 0)
        } else {
            (0, -(HEADER_SIZE as i32))
        }
    }

    fn set_title(&mut self, title: impl Into<String>) {
        let new_title = title.into();
        if let Some(title_text) = self.title_text.as_mut() {
            title_text.update_title(new_title.clone());
        }

        self.title = Some(new_title);
        self.dirty = true;
    }

    fn on_click(
        &mut self,
        timestamp: Duration,
        click: FrameClick,
        pressed: bool,
    ) -> Option<FrameAction> {
        let offset = self.header_offset();
        let location = self.pointer_location(offset);
        match click {
            FrameClick::Normal => self.mouse.click(
                location,
                timestamp,
                pressed,
                self.resizable,
                &self.state,
                &self.wm_capabilities,
            ),
            FrameClick::Alternate => self.mouse.alternate_click(
                location,
                offset,
                pressed,
                &self.wm_capabilities,
                self.hide_border,
            ),
            _ => None,
        }
    }

    fn set_scaling_factor(&mut self, scale_factor: f64) {
        // NOTE: Clamp it just in case to some ok-ish range.
        self.scale_factor = scale_factor.clamp(0.1, 64.).ceil() as u32;
        self.dirty = true;
        self.should_sync = true;
    }

    fn click_point_moved(
        &mut self,
        _timestamp: Duration,
        surface: &ObjectId,
        x: f64,
        y: f64,
    ) -> Option<CursorIcon> {
        let decorations = self.decorations.as_ref()?;
        let location = decorations.find_surface(surface);
        if location == Location::None {
            return None;
        }

        if self.state.contains(WindowState::FULLSCREEN) {
            if !parts::fullscreen_header_contains(self.width.get(), x, y, self.fullscreen_revealed)
            {
                self.click_point_left();
                return Some(CursorIcon::Default);
            }
            if !self.fullscreen_revealed {
                self.fullscreen_revealed = true;
                self.reveal_started = Some(Instant::now());
                self.dirty = true;
            }
        }
        let offset = self.header_offset();
        let old_location = self.pointer_location(offset);
        self.mouse.moved(location, x, y);
        let location = self.pointer_location(offset);
        let new_cursor = location.cursor(self.resizable);

        // Set dirty if we moved the cursor between the buttons.
        self.dirty |= (matches!(old_location, Location::Button(_))
            || matches!(location, Location::Button(_)))
            && old_location != location;

        if self.dirty {
            let _ = self.redraw_inner();
        }
        Some(new_cursor)
    }

    fn click_point_left(&mut self) {
        self.dirty |= matches!(
            self.pointer_location(self.header_offset()),
            Location::Button(_)
        );
        self.mouse.left();
        if self.fullscreen_revealed {
            self.fullscreen_revealed = false;
            self.reveal_started = None;
            self.dirty = true;
        }
        if self.dirty {
            let _ = self.redraw_inner();
        }
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn is_hidden(&self) -> bool {
        self.decorations.is_none()
    }
}

/// The configuration for the [`AdwaitaFrame`] frame.
#[derive(Debug, Clone)]
pub struct FrameConfig {
    pub theme: ColorTheme,
    /// Draw decorations but without the titlebar
    pub hide_titlebar: bool,
    /// Draw decorations but without the window border
    pub hide_border: bool,
    pub button_layout: Option<String>,
}

impl FrameConfig {
    /// Create the new configuration with the given `theme`.
    pub fn new(theme: ColorTheme) -> Self {
        Self {
            theme,
            hide_titlebar: false,
            hide_border: false,
            button_layout: None,
        }
    }

    /// This is equivalent of calling `FrameConfig::new(ColorTheme::light())`.
    ///
    /// For details see [`ColorTheme::light`].
    pub fn light() -> Self {
        Self::new(ColorTheme::light())
    }

    /// This is equivalent of calling `FrameConfig::new(ColorTheme::dark())`.
    ///
    /// For details see [`ColorTheme::dark`].
    pub fn dark() -> Self {
        Self::new(ColorTheme::dark())
    }

    /// Draw decorations but without the titlebar
    pub fn hide_titlebar(mut self, hide: bool) -> Self {
        self.hide_titlebar = hide;
        self
    }

    /// Draw decorations but without the window border
    pub fn hide_border(mut self, hide: bool) -> Self {
        self.hide_border = hide;
        self
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_part(
    part_id: PartId,
    rect: parts::Rect,
    pixmap: &mut PixmapMut,
    text_pixmap: Option<&Pixmap>,
    scale: u32,
    resizable: bool,
    state: &WindowState,
    colors: &ColorMap,
    buttons: &Buttons,
    mouse: Location,
    pressed: Location,
    fullscreen_label: Option<&Pixmap>,
    hide_border: bool,
    hide_titlebar: bool,
    shadow: &mut Shadow,
) {
    if !state.intersects(WindowState::TILED | WindowState::FULLSCREEN) {
        shadow.draw(
            pixmap,
            scale,
            state.contains(WindowState::ACTIVATED),
            part_id,
            hide_border,
        );
    }

    let border_paint = colors.border_paint();

    let border_size = theme::border_size(hide_border);
    let border_size = border_size * scale;

    // XXX we do all the math using integral types and then convert to f32 in the
    // end to ensure that result is finite.
    let border_rect = match part_id {
        PartId::Header => {
            return draw_headerbar(
                pixmap,
                text_pixmap,
                scale as f32,
                resizable,
                state,
                colors,
                buttons,
                mouse,
                pressed,
                fullscreen_label,
                hide_border,
            );
        }
        PartId::Left => {
            let x = (rect.x.unsigned_abs() * scale) - border_size;
            let y = rect.y.unsigned_abs() * scale;
            Rect::from_xywh(
                x as f32,
                y as f32,
                border_size as f32,
                (rect.height - y) as f32,
            )
        }
        PartId::Right => {
            let y = rect.y.unsigned_abs() * scale;
            Rect::from_xywh(0., y as f32, border_size as f32, (rect.height - y) as f32)
        }
        // We draw small visible border only bellow the window surface, no need to
        // handle `TOP`.
        PartId::Bottom => {
            let x = (rect.x.unsigned_abs() * scale) - border_size;
            Rect::from_xywh(
                x as f32,
                0.,
                (rect.width - 2 * x) as f32,
                border_size as f32,
            )
        }
        // Unless titlebar is disabled
        PartId::Top if hide_titlebar => {
            let x = rect.x.unsigned_abs() * scale;
            let x = x.saturating_sub(border_size);

            let y = rect.y.unsigned_abs() * scale;
            let y = y.saturating_sub(border_size);

            Rect::from_xywh(
                x as f32,
                y as f32,
                (rect.width - 2 * x) as f32,
                border_size as f32,
            )
        }
        PartId::Top => None,
    };

    // Fill the visible border, if present.
    if let Some(border_rect) = border_rect {
        pixmap.fill_rect(border_rect, &border_paint, Transform::identity(), None);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_headerbar(
    pixmap: &mut PixmapMut,
    text_pixmap: Option<&Pixmap>,
    scale: f32,
    resizable: bool,
    state: &WindowState,
    colors: &ColorMap,
    buttons: &Buttons,
    mouse: Location,
    pressed: Location,
    fullscreen_label: Option<&Pixmap>,
    hider_border: bool,
) {
    let _ = draw_headerbar_bg(pixmap, scale, colors, state);

    // Horizontal margin.
    let margin_h = get_margin_h_lp(state, hider_border) * scale;

    let canvas_w = pixmap.width() as f32;
    let canvas_h = HEADER_SIZE as f32 * scale;

    let header_w = canvas_w - margin_h * 2.0;
    let header_h = canvas_h;

    if let Some(text_pixmap) = text_pixmap {
        const TEXT_OFFSET: f32 = 10.;
        let offset_x = TEXT_OFFSET * scale;

        let text_w = text_pixmap.width() as f32;
        let text_h = text_pixmap.height() as f32;

        let x = margin_h + header_w / 2. - text_w / 2.;
        let y = header_h / 2. - text_h / 2.;

        let left_buttons_end_x = buttons.left_buttons_end_x().unwrap_or(0.0) * scale;
        let right_buttons_start_x =
            buttons.right_buttons_start_x().unwrap_or(header_w / scale) * scale;

        {
            // We have enough space to center text
            let (x, y, text_canvas_start_x) = if (x + text_w < right_buttons_start_x - offset_x)
                && (x > left_buttons_end_x + offset_x)
            {
                let text_canvas_start_x = x;

                (x, y, text_canvas_start_x)
            } else {
                let x = left_buttons_end_x + offset_x;
                let text_canvas_start_x = left_buttons_end_x + offset_x;

                (x, y, text_canvas_start_x)
            };

            let text_canvas_end_x = right_buttons_start_x - x - offset_x;
            // Ensure that text start within the bounds.
            let x = x.max(margin_h + offset_x);

            if let Some(clip) =
                Rect::from_xywh(text_canvas_start_x, 0., text_canvas_end_x, canvas_h)
            {
                if let Some(mut mask) = Mask::new(pixmap.width(), pixmap.height()) {
                    mask.fill_path(
                        &PathBuilder::from_rect(clip),
                        FillRule::Winding,
                        false,
                        Transform::identity(),
                    );
                    pixmap.draw_pixmap(
                        x.round() as i32,
                        y as i32,
                        text_pixmap.as_ref(),
                        &PixmapPaint::default(),
                        Transform::identity(),
                        Some(&mask),
                    );
                } else {
                    log::error!(
                        "Invalid mask width and height: w: {}, h: {}",
                        canvas_w as u32,
                        canvas_h as u32
                    );
                }
            }
        }
    }

    // Draw the buttons.
    buttons.draw(
        scale,
        colors,
        mouse,
        pressed,
        fullscreen_label,
        pixmap,
        resizable,
        state,
    );
}

#[must_use]
fn draw_headerbar_bg(
    pixmap: &mut PixmapMut,
    scale: f32,
    colors: &ColorMap,
    state: &WindowState,
) -> SkiaResult {
    let w = pixmap.width() as f32;
    let h = HEADER_SIZE as f32 * scale;

    let radius = if state
        .intersects(WindowState::MAXIMIZED | WindowState::TILED | WindowState::FULLSCREEN)
    {
        0.
    } else {
        CORNER_RADIUS as f32 * scale
    };

    let bg = rounded_headerbar_shape(0., 0., w, h, radius)?;

    pixmap.fill_path(
        &bg,
        &colors.headerbar_paint(),
        FillRule::Winding,
        Transform::identity(),
        None,
    );

    if state.contains(WindowState::FULLSCREEN) {
        for row in 0..(parts::REVEAL_SHADOW as f32 * scale) as u32 {
            let alpha = (0.24 * (-0.45 * row as f32 / scale).exp() * 255.) as u8;
            let mut paint = tiny_skia::Paint::default();
            paint.set_color_rgba8(0, 0, 0, alpha);
            pixmap.fill_rect(
                Rect::from_xywh(0., h + row as f32, w, 1.)?,
                &paint,
                Transform::identity(),
                None,
            );
        }
    }

    Some(())
}

fn rounded_headerbar_shape(x: f32, y: f32, width: f32, height: f32, radius: f32) -> Option<Path> {
    // https://stackoverflow.com/a/27863181
    let cubic_bezier_circle = 0.552_284_8 * radius;

    let mut pb = PathBuilder::new();
    let mut cursor = Point::from_xy(x, y);

    // !!!
    // This code is heavily "inspired" by https://gitlab.com/snakedye/snui/
    // So technically it should be licensed under MPL-2.0, sorry about that 🥺 👉👈
    // !!!

    // Positioning the cursor
    cursor.y += radius;
    pb.move_to(cursor.x, cursor.y);

    // Drawing the outline
    let next = Point::from_xy(cursor.x + radius, cursor.y - radius);
    pb.cubic_to(
        cursor.x,
        cursor.y - cubic_bezier_circle,
        next.x - cubic_bezier_circle,
        next.y,
        next.x,
        next.y,
    );
    cursor = next;
    pb.line_to(
        {
            cursor.x = x + width - radius;
            cursor.x
        },
        cursor.y,
    );
    let next = Point::from_xy(cursor.x + radius, cursor.y + radius);
    pb.cubic_to(
        cursor.x + cubic_bezier_circle,
        cursor.y,
        next.x,
        next.y - cubic_bezier_circle,
        next.x,
        next.y,
    );
    cursor = next;
    pb.line_to(cursor.x, {
        cursor.y = y + height;
        cursor.y
    });
    pb.line_to(
        {
            cursor.x = x;
            cursor.x
        },
        cursor.y,
    );

    pb.close();

    pb.finish()
}

// returns horizontal margin, logical points
fn get_margin_h_lp(state: &WindowState, hider_border: bool) -> f32 {
    if state.intersects(WindowState::MAXIMIZED | WindowState::TILED | WindowState::FULLSCREEN) {
        0.0
    } else {
        theme::border_size(hider_border) as f32
    }
}

const REVEAL_DURATION: Duration = Duration::from_millis(180);

fn reveal_offset(elapsed: Duration) -> i32 {
    let t = (elapsed.as_secs_f32() / REVEAL_DURATION.as_secs_f32()).clamp(0., 1.);
    (-(HEADER_SIZE as f32) * (1. - t).powi(3)).round() as i32
}

#[cfg(test)]
mod animation_tests {
    use super::*;
    #[test]
    fn stationary_pointer_clicks_control_after_fullscreen_slide() {
        let mut buttons = Buttons::new(None);
        buttons.set_fullscreen(true);
        buttons.arrange(600, 0.);
        let mut mouse = MouseState::default();
        let (x, y) = (23., 23.);
        let offset = reveal_offset(Duration::from_millis(40));
        mouse.moved(Location::Head, x, y);
        assert_eq!(
            mouse.location(&buttons, offset, 600, 800, false),
            Location::Head
        );

        // The animation completes with no further pointer motion.
        let offset = reveal_offset(REVEAL_DURATION);
        let location = mouse.location(&buttons, offset, 600, 800, false);
        assert_eq!(location, Location::Button(buttons::ButtonKind::Back));
        mouse.set_pressed(location, FrameClick::Normal, true);
        assert_eq!(mouse.pressed_location(), location);
        assert!(mouse.button_click(location, true).is_none());
        let location = mouse.location(&buttons, offset, 600, 800, false);
        assert!(matches!(
            mouse.button_click(location, false),
            Some(Action::Back)
        ));
    }

    #[test]
    fn fullscreen_header_keeps_title_pixels_with_its_shadow_extension() {
        for scale in [1u32, 2] {
            let mut header = Pixmap::new(600 * scale, (HEADER_SIZE + parts::REVEAL_SHADOW) * scale)
                .unwrap_or_else(|| unreachable!());
            let mut text = Pixmap::new(20 * scale, 8 * scale).unwrap_or_else(|| unreachable!());
            text.fill(Color::from_rgba8(255, 0, 255, 255));
            let mut buttons = Buttons::new(None);
            buttons.set_fullscreen(true);
            buttons.arrange(600, 0.);
            draw_headerbar(
                &mut header.as_mut(),
                Some(&text),
                scale as f32,
                true,
                &WindowState::FULLSCREEN,
                &ColorTheme::light().active,
                &buttons,
                Location::None,
                Location::None,
                None,
                true,
            );
            let title_pixel = header
                .pixel(300 * scale, HEADER_SIZE * scale / 2)
                .unwrap_or_else(|| unreachable!());
            assert_eq!(
                (title_pixel.red(), title_pixel.green(), title_pixel.blue()),
                (255, 0, 255)
            );
            assert!(header
                .pixel(300 * scale, (HEADER_SIZE + 1) * scale)
                .is_some_and(|p| p.alpha() > 0));
        }
    }

    #[test]
    fn reveal_slides_to_rest_without_overshooting() {
        assert_eq!(reveal_offset(Duration::ZERO), -(HEADER_SIZE as i32));
        let offsets: Vec<_> = (0..=240)
            .map(|ms| reveal_offset(Duration::from_millis(ms)))
            .collect();
        assert!(offsets
            .iter()
            .zip(offsets.iter().skip(1))
            .all(|(a, b)| a <= b));
        assert_eq!(reveal_offset(REVEAL_DURATION), 0);
        assert_eq!(reveal_offset(Duration::MAX), 0);
    }
}
