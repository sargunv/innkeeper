use std::time::Duration;

use smithay_client_toolkit::reexports::csd_frame::{
    CursorIcon, FrameAction, ResizeEdge, WindowManagerCapabilities, WindowState,
};

use crate::{
    buttons::ButtonKind,
    theme::{self, HEADER_SIZE, RESIZE_HANDLE_CORNER_SIZE},
};

/// Time to register the next click as a double click.
///
/// The value is the same as the default in gtk4.
const DOUBLE_CLICK_DURATION: Duration = Duration::from_millis(400);

/// The state of the mouse input inside the decorations frame.
#[derive(Debug, Default)]
pub(crate) struct MouseState {
    button_pressed: Option<ButtonKind>,
    visual_pressed: Location,

    /// The raw decoration surface and its local coordinates, before hit testing.
    position: Option<(Location, f64, f64)>,

    /// The instant of the last click.
    last_normal_click: Option<Duration>,
}

impl MouseState {
    /// Resolve the raw surface position against the current frame geometry.
    pub fn location(
        &self,
        buttons: &crate::buttons::Buttons,
        header_offset: i32,
        header_width: u32,
        side_height: u32,
        hide_border: bool,
    ) -> Location {
        let Some((location, x, raw_y)) = self.position else {
            return Location::None;
        };
        let y = if location == Location::Head {
            raw_y - f64::from(header_offset)
        } else {
            raw_y
        };
        let edge_size = theme::edge_size(hide_border);

        let left_corner_x = edge_size + RESIZE_HANDLE_CORNER_SIZE;
        let right_corner_x = (header_width + edge_size).saturating_sub(RESIZE_HANDLE_CORNER_SIZE);
        let top_corner_y = RESIZE_HANDLE_CORNER_SIZE;
        let bottom_corner_y = side_height.saturating_sub(RESIZE_HANDLE_CORNER_SIZE);
        match location {
            Location::Head | Location::Button(_) => buttons.find_button(x, y),
            Location::Top | Location::TopLeft | Location::TopRight => {
                if x <= f64::from(left_corner_x) {
                    Location::TopLeft
                } else if x >= f64::from(right_corner_x) {
                    Location::TopRight
                } else {
                    Location::Top
                }
            }
            Location::Bottom | Location::BottomLeft | Location::BottomRight => {
                if x <= f64::from(left_corner_x) {
                    Location::BottomLeft
                } else if x >= f64::from(right_corner_x) {
                    Location::BottomRight
                } else {
                    Location::Bottom
                }
            }
            Location::Left => {
                if y <= f64::from(top_corner_y) {
                    Location::TopLeft
                } else if y >= f64::from(bottom_corner_y) {
                    Location::BottomLeft
                } else {
                    Location::Left
                }
            }
            Location::Right => {
                if y <= f64::from(top_corner_y) {
                    Location::TopRight
                } else if y >= f64::from(bottom_corner_y) {
                    Location::BottomRight
                } else {
                    Location::Right
                }
            }
            other => other,
        }
    }

    pub fn set_pressed(
        &mut self,
        location: Location,
        click: smithay_client_toolkit::reexports::csd_frame::FrameClick,
        pressed: bool,
    ) {
        if matches!(
            click,
            smithay_client_toolkit::reexports::csd_frame::FrameClick::Normal
        ) {
            self.visual_pressed = if pressed { location } else { Location::None };
        }
    }

    pub fn pressed_location(&self) -> Location {
        self.visual_pressed
    }

    /// Activate custom controls only after a press and release on the same button.
    pub fn button_click(&mut self, location: Location, pressed: bool) -> Option<crate::Action> {
        let button = match location {
            Location::Button(kind @ (ButtonKind::Back | ButtonKind::Fullscreen)) => Some(kind),
            _ => None,
        };
        if pressed {
            self.button_pressed = button;
            return None;
        }
        match (self.button_pressed.take(), button) {
            (Some(ButtonKind::Back), Some(ButtonKind::Back)) => Some(crate::Action::Back),
            (Some(ButtonKind::Fullscreen), Some(ButtonKind::Fullscreen)) => {
                Some(crate::Action::ToggleFullscreen)
            }
            _ => None,
        }
    }

    /// The normal click on decorations frame was made.
    pub fn click(
        &mut self,
        location: Location,
        timestamp: Duration,
        pressed: bool,
        resizable: bool,
        state: &WindowState,
        wm_capabilities: &WindowManagerCapabilities,
    ) -> Option<FrameAction> {
        let maximized = state.contains(WindowState::MAXIMIZED);
        let action = match location {
            Location::Top if resizable => FrameAction::Resize(ResizeEdge::Top),
            Location::TopLeft if resizable => FrameAction::Resize(ResizeEdge::TopLeft),
            Location::Left if resizable => FrameAction::Resize(ResizeEdge::Left),
            Location::BottomLeft if resizable => FrameAction::Resize(ResizeEdge::BottomLeft),
            Location::Bottom if resizable => FrameAction::Resize(ResizeEdge::Bottom),
            Location::BottomRight if resizable => FrameAction::Resize(ResizeEdge::BottomRight),
            Location::Right if resizable => FrameAction::Resize(ResizeEdge::Right),
            Location::TopRight if resizable => FrameAction::Resize(ResizeEdge::TopRight),
            Location::Button(ButtonKind::Close) if !pressed => FrameAction::Close,
            Location::Button(ButtonKind::Maximize) if !pressed && !maximized => {
                FrameAction::Maximize
            }
            Location::Button(ButtonKind::Maximize) if !pressed && maximized => {
                FrameAction::UnMaximize
            }
            Location::Button(ButtonKind::Minimize) if !pressed => FrameAction::Minimize,
            Location::Head
                if pressed && wm_capabilities.contains(WindowManagerCapabilities::MAXIMIZE) =>
            {
                match self.last_normal_click.replace(timestamp) {
                    Some(last) if timestamp.saturating_sub(last) < DOUBLE_CLICK_DURATION => {
                        if maximized {
                            FrameAction::UnMaximize
                        } else {
                            FrameAction::Maximize
                        }
                    }
                    _ => FrameAction::Move,
                }
            }
            Location::Head if pressed => FrameAction::Move,
            _ => return None,
        };

        Some(action)
    }

    /// Alternative click on decorations frame was made.
    pub fn alternate_click(
        &mut self,
        location: Location,
        header_offset: i32,
        pressed: bool,
        wm_capabilities: &WindowManagerCapabilities,
        hide_border: bool,
    ) -> Option<FrameAction> {
        // Invalidate the normal click.
        self.last_normal_click = None;

        let edge_size = theme::edge_size(hide_border);

        let (_, x, y) = self.position?;
        match location {
            Location::Head | Location::Button(_)
                if pressed && wm_capabilities.contains(WindowManagerCapabilities::WINDOW_MENU) =>
            {
                Some(FrameAction::ShowMenu(
                    // XXX this could be one 1pt off when the frame is not maximized, but it's not
                    // like it really matters in the end.
                    x as i32 - edge_size as i32,
                    // We must offset it by header size for precise position.
                    y as i32 - header_offset - HEADER_SIZE as i32,
                ))
            }
            _ => None,
        }
    }

    /// The mouse moved inside the decorations frame.
    pub fn moved(&mut self, surface: Location, x: f64, y: f64) {
        self.position = Some((surface, x, y));
    }

    /// The mouse left the decorations frame.
    pub fn left(&mut self) {
        self.position = None;
        self.button_pressed = None;
        self.visual_pressed = Location::None;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Default)]
pub enum Location {
    #[default]
    None,
    Head,
    Top,
    TopRight,
    Right,
    BottomRight,
    Bottom,
    BottomLeft,
    Left,
    TopLeft,
    Button(ButtonKind),
}

impl Location {
    pub fn cursor(self, resizable: bool) -> CursorIcon {
        match self {
            _ if !resizable => CursorIcon::Default,
            Location::Top => CursorIcon::NResize,
            Location::TopRight => CursorIcon::NeResize,
            Location::Right => CursorIcon::EResize,
            Location::BottomRight => CursorIcon::SeResize,
            Location::Bottom => CursorIcon::SResize,
            Location::BottomLeft => CursorIcon::SwResize,
            Location::Left => CursorIcon::WResize,
            Location::TopLeft => CursorIcon::NwResize,
            _ => CursorIcon::Default,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn visual_press_tracks_button_and_clears_on_release_or_leave() {
        use smithay_client_toolkit::reexports::csd_frame::FrameClick;
        let mut mouse = MouseState::default();
        let location = Location::Button(ButtonKind::Close);
        mouse.set_pressed(location, FrameClick::Normal, true);
        assert_eq!(
            mouse.pressed_location(),
            Location::Button(ButtonKind::Close)
        );
        mouse.set_pressed(location, FrameClick::Normal, false);
        assert_eq!(mouse.pressed_location(), Location::None);
        mouse.set_pressed(location, FrameClick::Normal, true);
        mouse.left();
        assert_eq!(mouse.pressed_location(), Location::None);
    }

    #[test]
    fn normal_taps_activate_controls_and_title_press_starts_move() {
        let mut mouse = MouseState::default();
        let state = WindowState::empty();
        let caps = WindowManagerCapabilities::all();
        for button in [
            ButtonKind::Close,
            ButtonKind::Minimize,
            ButtonKind::Maximize,
        ] {
            let location = Location::Button(button);
            assert!(mouse
                .click(location, Duration::ZERO, true, true, &state, &caps)
                .is_none());
            assert!(matches!(
                (
                    button,
                    mouse.click(location, Duration::ZERO, false, true, &state, &caps)
                ),
                (ButtonKind::Close, Some(FrameAction::Close))
                    | (ButtonKind::Minimize, Some(FrameAction::Minimize))
                    | (ButtonKind::Maximize, Some(FrameAction::Maximize))
            ));
            mouse.left();
        }
        let location = Location::Button(ButtonKind::Maximize);
        assert!(matches!(
            mouse.click(
                location,
                Duration::ZERO,
                false,
                true,
                &WindowState::MAXIMIZED,
                &caps
            ),
            Some(FrameAction::UnMaximize),
        ));
        let location = Location::Head;
        assert!(matches!(
            mouse.click(location, Duration::ZERO, true, true, &state, &caps),
            Some(FrameAction::Move)
        ));
        mouse.left();
        let location = mouse.location(&crate::buttons::Buttons::new(None), 0, 600, 800, false);
        assert!(mouse
            .click(location, Duration::ZERO, false, true, &state, &caps)
            .is_none());
    }

    #[test]
    fn custom_controls_require_a_click_and_do_not_move_or_resize() {
        for kind in [ButtonKind::Back, ButtonKind::Fullscreen] {
            let mut mouse = MouseState::default();
            let location = Location::Button(kind);
            assert!(mouse.button_click(location, false).is_none());
            assert!(mouse.button_click(location, true).is_none());
            assert!(mouse
                .click(
                    location,
                    Duration::ZERO,
                    true,
                    true,
                    &WindowState::empty(),
                    &WindowManagerCapabilities::all()
                )
                .is_none());
            assert!(matches!(
                (kind, mouse.button_click(location, false)),
                (ButtonKind::Back, Some(crate::Action::Back))
                    | (
                        ButtonKind::Fullscreen,
                        Some(crate::Action::ToggleFullscreen)
                    )
            ));
            assert!(mouse.button_click(location, false).is_none());
            mouse.button_click(location, true);
            mouse.left();
            let location = Location::Button(kind);
            assert!(mouse.button_click(location, false).is_none());
            let location = Location::Head;
            mouse.button_click(location, true);
            let location = Location::Button(kind);
            assert!(mouse.button_click(location, false).is_none());
            mouse.moved(Location::Head, 19., 17.);
            assert!(matches!(
                mouse.alternate_click(location, 0, true, &WindowManagerCapabilities::all(), false),
                Some(FrameAction::ShowMenu(_, _))
            ));
        }
    }
}
