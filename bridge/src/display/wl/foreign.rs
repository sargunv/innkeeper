//! `zxdg_exporter_v2` (xdg-foreign): each window's toplevel is exported, so
//! portals can parent their dialogs to it (`wayland:<handle>`). Handles are
//! published in [`crate::display::foreign_handle`] by window id.

use std::collections::HashMap;
use std::sync::Mutex;

use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::protocol::wl_surface;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use wayland_protocols::xdg::foreign::zv2::client::{
    zxdg_exported_v2::{self, ZxdgExportedV2},
    zxdg_exporter_v2::ZxdgExporterV2,
};

use super::State;

/// Exported handles by window id (all display connections; window ids are
/// Android display ids, unique in a session).
pub(crate) static HANDLES: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);

pub(super) fn export(
    exporter: Option<&ZxdgExporterV2>,
    surface: &wl_surface::WlSurface,
    window: u32,
    qh: &QueueHandle<State>,
) -> Option<ZxdgExportedV2> {
    Some(exporter?.export_toplevel(surface, qh, Exported(window)))
}

pub(super) fn forget(window: u32, exported: Option<ZxdgExportedV2>) {
    if let Some(e) = exported {
        e.destroy();
    }
    if let Some(m) = HANDLES.lock().unwrap().as_mut() {
        m.remove(&window);
    }
}

pub(super) struct Exported(u32);

impl Dispatch2<ZxdgExportedV2, State> for Exported {
    fn event(
        &self,
        _: &mut State,
        _: &ZxdgExportedV2,
        e: zxdg_exported_v2::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let zxdg_exported_v2::Event::Handle { handle } = e {
            HANDLES
                .lock()
                .unwrap()
                .get_or_insert_with(HashMap::new)
                .insert(self.0, handle);
        }
    }
}

impl Dispatch2<ZxdgExporterV2, State> for super::Unit {
    fn event(
        &self,
        _: &mut State,
        _: &ZxdgExporterV2,
        _: <ZxdgExporterV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
