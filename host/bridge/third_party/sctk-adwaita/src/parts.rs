use smithay_client_toolkit::reexports::client::{
    backend::ObjectId,
    protocol::{wl_subsurface::WlSubsurface, wl_surface::WlSurface},
    Dispatch, Proxy, QueueHandle,
};

use smithay_client_toolkit::{
    compositor::SurfaceData,
    subcompositor::{SubcompositorState, SubsurfaceData},
};

use crate::theme::{self, RESIZE_HANDLE_SIZE};
use crate::{pointer::Location, wl_typed::WlTyped};

// Order is important. The lower the number, the earlier the part gets drawn.
// Because the header can overlap other parts, we draw it last.
#[repr(u8)]
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum PartId {
    Top = 0,
    Left = 1,
    Right = 2,
    Bottom = 3,
    Header = 4,
}

impl PartId {
    const TOP: usize = Self::Top as usize;
    const LEFT: usize = Self::Left as usize;
    const RIGHT: usize = Self::Right as usize;
    const BOTTOM: usize = Self::Bottom as usize;
    const HEADER: usize = Self::Header as usize;
    pub const COUNT: usize = 5;

    pub fn from_usize(v: usize) -> Self {
        match v {
            Self::TOP => PartId::Top,
            Self::LEFT => PartId::Left,
            Self::RIGHT => PartId::Right,
            Self::BOTTOM => PartId::Bottom,
            Self::HEADER => PartId::Header,
            _ => unreachable!(),
        }
    }
}

/// The decoration's 'parts'.
#[derive(Debug)]
pub struct DecorationParts {
    parts: [Part; PartId::COUNT],
    config: LayoutConfig,
}

impl DecorationParts {
    pub fn new<State>(
        base_surface: &WlTyped<WlSurface, SurfaceData<()>>,
        subcompositor: &SubcompositorState,
        queue_handle: &QueueHandle<State>,
    ) -> Self
    where
        State:
            Dispatch<WlSurface, SurfaceData<()>> + Dispatch<WlSubsurface, SubsurfaceData> + 'static,
    {
        let parts = [
            Part::new(base_surface, subcompositor, queue_handle),
            Part::new(base_surface, subcompositor, queue_handle),
            Part::new(base_surface, subcompositor, queue_handle),
            Part::new(base_surface, subcompositor, queue_handle),
            Part::new(base_surface, subcompositor, queue_handle),
        ];

        Self {
            parts,
            config: LayoutConfig::default(),
        }
    }

    pub fn parts(&self) -> impl Iterator<Item = (PartId, &Part)> {
        self.parts
            .iter()
            .enumerate()
            .map(|(id, part)| (PartId::from_usize(id), part))
    }

    fn parts_mut(&mut self) -> impl Iterator<Item = (PartId, &mut Part)> {
        self.parts
            .iter_mut()
            .enumerate()
            .map(|(id, part)| (PartId::from_usize(id), part))
    }

    /// Edge is a border + shadow
    fn edges_mut(&mut self) -> impl Iterator<Item = &mut Part> {
        self.parts_mut()
            .filter(|(idx, _)| *idx != PartId::Header)
            .map(|(_, p)| p)
    }

    pub fn header(&self) -> &Part {
        &self.parts[PartId::HEADER]
    }

    pub fn header_mut(&mut self) -> &mut Part {
        &mut self.parts[PartId::HEADER]
    }

    pub fn show(&mut self) {
        for part in self.parts.iter_mut() {
            part.hide = false;
        }
    }

    /// Edge is a border + shadow
    pub fn hide_edges(&mut self) {
        for part in self.edges_mut() {
            part.hide = true;
            part.surface.attach(None, 0, 0);
            part.surface.commit();
        }
    }

    pub fn hide_titlebar(&mut self) {
        let part = self.header_mut();
        part.hide = true;
        part.surface.attach(None, 0, 0);
        part.surface.commit();
    }

    pub fn relayout(&mut self, config: LayoutConfig) {
        if self.config == config {
            return;
        }
        self.config = config;

        let layout = PartLayout::calc(config);
        for (part, layout) in self.parts.iter_mut().zip(layout) {
            part.surface_rect = layout.surface_rect;
            part.input_rect = layout.input_rect;
        }
    }

    pub fn side_height(&self) -> u32 {
        self.parts[PartId::LEFT].surface_rect.height
    }

    pub fn find_surface(&self, surface: &ObjectId) -> Location {
        let found = self
            .parts()
            .find(|(_id, part)| &part.surface.id() == surface);

        let Some((id, _)) = found else {
            return Location::None;
        };

        match id {
            PartId::Header => Location::Head,
            PartId::Top => Location::Top,
            PartId::Bottom => Location::Bottom,
            PartId::Left => Location::Left,
            PartId::Right => Location::Right,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, Eq, PartialEq)]
pub struct LayoutConfig {
    pub width: u32,
    pub height: u32,
    pub hide_titlebar: bool,
    pub hide_border: bool,
    pub hide_edges: bool,
    pub fullscreen: bool,
    pub revealed: bool,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct PartLayout {
    /// Positioned relative to the main surface.
    pub surface_rect: Rect,
    /// Positioned relative to the local surface, aka. `surface_rect`.
    ///
    /// `None` if it fully covers `surface_rect`.
    pub input_rect: Option<Rect>,
}

impl PartLayout {
    pub fn calc(config: LayoutConfig) -> [Self; 5] {
        let LayoutConfig {
            width,
            height,
            hide_titlebar,
            hide_border,
            hide_edges,
            fullscreen,
            revealed,
        } = config;

        let mut parts = [Self::default(); 5];

        let header_height = theme::header_height(hide_titlebar);
        let height_with_header = height + header_height;

        let edge_size = theme::edge_size(hide_border);

        let width_with_edge = width + 2 * edge_size;
        let width_input_rect = width_with_edge - (edge_size * 2) + (RESIZE_HANDLE_SIZE * 2);

        parts[PartId::TOP].surface_rect = Rect {
            x: -(edge_size as i32),
            y: -(header_height as i32 + edge_size as i32),
            width: width_with_edge,
            height: edge_size,
        };
        parts[PartId::TOP].input_rect = Some(Rect {
            x: edge_size as i32 - RESIZE_HANDLE_SIZE as i32,
            y: edge_size as i32 - RESIZE_HANDLE_SIZE as i32,
            width: width_input_rect,
            height: RESIZE_HANDLE_SIZE,
        });

        parts[PartId::LEFT].surface_rect = Rect {
            x: -(edge_size as i32),
            y: -(header_height as i32),
            width: edge_size,
            height: height_with_header,
        };
        parts[PartId::LEFT].input_rect = Some(Rect {
            x: edge_size as i32 - RESIZE_HANDLE_SIZE as i32,
            y: 0,
            width: RESIZE_HANDLE_SIZE,
            height: height_with_header,
        });

        parts[PartId::RIGHT].surface_rect = Rect {
            x: width as i32,
            y: -(header_height as i32),
            width: edge_size,
            height: height_with_header,
        };
        parts[PartId::RIGHT].input_rect = Some(Rect {
            x: 0,
            y: 0,
            width: RESIZE_HANDLE_SIZE,
            height: height_with_header,
        });

        parts[PartId::BOTTOM].surface_rect = Rect {
            x: -(edge_size as i32),
            y: height as i32,
            width: width_with_edge,
            height: edge_size,
        };
        parts[PartId::BOTTOM].input_rect = Some(Rect {
            x: edge_size as i32 - RESIZE_HANDLE_SIZE as i32,
            y: 0,
            width: width_input_rect,
            height: RESIZE_HANDLE_SIZE,
        });

        parts[PartId::HEADER].surface_rect = Rect {
            x: 0,
            y: -(header_height as i32),
            width,
            height: header_height,
        };
        parts[PartId::HEADER].input_rect = None;

        let border_size = theme::border_size(hide_border);

        // XXX to perfectly align the visible borders we draw them with
        // the header, otherwise rounded corners won't look 'smooth' at the
        // start. To achieve that, we enlargen the width of the header by
        // 2 * `VISIBLE_BORDER_SIZE`, and move `x` by `VISIBLE_BORDER_SIZE`
        // to the left.
        if !hide_edges {
            parts[PartId::HEADER].surface_rect.width += 2 * border_size;
            parts[PartId::HEADER].surface_rect.x -= border_size as i32;
        }

        if fullscreen {
            parts[PartId::HEADER].surface_rect = Rect {
                x: 0,
                y: 0,
                width,
                height: theme::HEADER_SIZE + REVEAL_SHADOW,
            };
            parts[PartId::HEADER].input_rect = Some(Rect {
                x: 0,
                y: 0,
                width,
                height: fullscreen_header_height(revealed),
            });
        }

        parts
    }
}

/// A small transparent strip receives pointer/touch input while the header is hidden.
pub(crate) const REVEAL_SHADOW: u32 = 12;

const FULLSCREEN_REVEAL_HEIGHT: u32 = 4;

fn fullscreen_header_height(revealed: bool) -> u32 {
    if revealed {
        theme::HEADER_SIZE
    } else {
        FULLSCREEN_REVEAL_HEIGHT
    }
}

pub(crate) fn fullscreen_header_contains(width: u32, x: f64, y: f64, revealed: bool) -> bool {
    x >= 0. && x < f64::from(width) && y >= 0. && y < f64::from(fullscreen_header_height(revealed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fullscreen_reveal_zone_matches_the_header_input_region() {
        for revealed in [false, true] {
            let height = if revealed { theme::HEADER_SIZE } else { 4 };
            let layout = PartLayout::calc(LayoutConfig {
                width: 800,
                height: 600,
                fullscreen: true,
                revealed,
                ..Default::default()
            });
            let header = &layout[PartId::HEADER];
            assert_eq!((header.surface_rect.x, header.surface_rect.y), (0, 0));
            let Some(input) = header.input_rect else {
                unreachable!()
            };
            assert_eq!(
                (input.x, input.y, input.width, input.height),
                (0, 0, 800, height)
            );
            for (x, y, inside) in [
                (0., 0., true),
                (799.9, f64::from(height) - 0.1, true),
                (-0.1, 0., false),
                (800., 0., false),
                (0., -0.1, false),
                (0., f64::from(height), false),
            ] {
                assert_eq!(fullscreen_header_contains(800, x, y, revealed), inside);
            }
        }
        assert!(!fullscreen_header_contains(800, 400., 17., false));
        assert!(fullscreen_header_contains(800, 400., 17., true));
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug)]
pub struct Part {
    pub surface: WlTyped<WlSurface, SurfaceData<()>>,
    pub subsurface: WlTyped<WlSubsurface, SubsurfaceData>,

    /// Positioned relative to the main surface.
    pub surface_rect: Rect,
    /// Positioned relative to the local surface, aka. `surface_rect`.
    ///
    /// `None` if it fully covers `surface_rect`.
    pub input_rect: Option<Rect>,

    pub hide: bool,
}

impl Part {
    fn new<State>(
        parent: &WlTyped<WlSurface, SurfaceData<()>>,
        subcompositor: &SubcompositorState,
        queue_handle: &QueueHandle<State>,
    ) -> Part
    where
        State:
            Dispatch<WlSurface, SurfaceData<()>> + Dispatch<WlSubsurface, SubsurfaceData> + 'static,
    {
        let (subsurface, surface) =
            subcompositor.create_subsurface(parent.inner().clone(), queue_handle);

        let subsurface = WlTyped::wrap::<State>(subsurface);
        let surface = WlTyped::wrap::<State>(surface);

        // Sync with the parent surface.
        subsurface.set_sync();

        Part {
            surface,
            subsurface,
            surface_rect: Rect::default(),
            input_rect: None,
            hide: false,
        }
    }
}

impl Drop for Part {
    fn drop(&mut self) {
        self.subsurface.destroy();
        self.surface.destroy();
    }
}
