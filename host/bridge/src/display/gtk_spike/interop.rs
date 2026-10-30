//! Borrow GDK's Wayland display, content toplevel and pointer without replacing
//! their listeners. Only our own extension objects receive Rust listeners.
use std::sync::Arc;

use crate::input::Hub;
use adw::prelude::*;
use anyhow::{Context, Result};
use gtk::{gdk, glib::translate::ToGlibPtr};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    backend::{Backend, ObjectId},
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_pointer::WlPointer, wl_registry, wl_surface::WlSurface},
};
use wayland_protocols::wp::{
    pointer_constraints::zv1::client::{
        zwp_locked_pointer_v1::{self, ZwpLockedPointerV1},
        zwp_pointer_constraints_v1::{Lifetime, ZwpPointerConstraintsV1},
    },
    presentation_time::client::{
        wp_presentation::{self, WpPresentation},
        wp_presentation_feedback::{self, WpPresentationFeedback},
    },
    relative_pointer::zv1::client::{
        zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1,
        zwp_relative_pointer_v1::{self, ZwpRelativePointerV1},
    },
};

// GTK exports these in libgtk-4 on Wayland builds. Using the C interop avoids
// adding a second GTK/GDK Rust binding stack or owning GDK's wl_pointer listener.
#[link(name = "gtk-4")]
unsafe extern "C" {
    fn gdk_wayland_display_get_wl_display(
        display: *mut gdk::ffi::GdkDisplay,
    ) -> *mut std::ffi::c_void;
    fn gdk_wayland_surface_get_wl_surface(
        surface: *mut gdk::ffi::GdkSurface,
    ) -> *mut std::ffi::c_void;
    fn gdk_wayland_toplevel_set_application_id(
        toplevel: *mut gdk::ffi::GdkToplevel,
        application_id: *const std::ffi::c_char,
    );
    fn gdk_wayland_device_get_wl_pointer(device: *mut gdk::ffi::GdkDevice)
    -> *mut std::ffi::c_void;
}

pub(super) struct Interop {
    conn: Connection,
    queue: EventQueue<Protocol>,
    state: Protocol,
    constraints: Option<ZwpPointerConstraintsV1>,
    manager: Option<ZwpRelativePointerManagerV1>,
    relative: Option<ZwpRelativePointerV1>,
    presentation: Option<WpPresentation>,
    surface: WlSurface,
    pointer: Option<WlPointer>,
    requested: bool,
    blocked: bool,
    focused: bool,
    keyboard: bool,
    // Keep borrowed proxies alive until the extension objects are destroyed.
    _display: gdk::Display,
    _surface: gdk::Surface,
    _device: Option<gdk::Device>,
}
struct Protocol {
    lock: Option<ZwpLockedPointerV1>,
    active: bool,
    unlocked: bool,
    hub: Arc<Hub>,
    window: u32,
}
impl Interop {
    pub(super) fn new(
        window: &adw::ApplicationWindow,
        hub: Arc<Hub>,
        id: u32,
        app_id: &str,
    ) -> Result<Self> {
        let display = gtk::prelude::WidgetExt::display(window);
        anyhow::ensure!(
            display.type_().name() == "GdkWaylandDisplay",
            "GTK spike requires Wayland"
        );
        let surface = window.surface().context("unrealized GTK window")?;
        let app_id = std::ffi::CString::new(app_id)?;
        unsafe {
            gdk_wayland_toplevel_set_application_id(surface.as_ptr().cast(), app_id.as_ptr());
        }
        let pointer = display.default_seat().and_then(|s| s.pointer());
        // All calls and proxy use are on GDK's owning thread. The foreign backend
        // never closes GDK's display and never changes a borrowed proxy listener.
        let conn = unsafe {
            Connection::from_backend(Backend::from_foreign_display(
                gdk_wayland_display_get_wl_display(display.to_glib_none().0).cast(),
            ))
        };
        let raw_surface = unsafe { gdk_wayland_surface_get_wl_surface(surface.to_glib_none().0) };
        let raw_pointer = pointer.as_ref().map_or(std::ptr::null_mut(), |p| unsafe {
            gdk_wayland_device_get_wl_pointer(p.to_glib_none().0)
        });
        anyhow::ensure!(
            !raw_surface.is_null(),
            "GDK Wayland handles unavailable: surface={raw_surface:p} pointer={raw_pointer:p}"
        );
        let wl_surface = WlSurface::from_id(&conn, unsafe {
            ObjectId::from_ptr(WlSurface::interface(), raw_surface.cast())?
        })?;
        let wl_pointer = if raw_pointer.is_null() {
            None
        } else {
            Some(WlPointer::from_id(&conn, unsafe {
                ObjectId::from_ptr(WlPointer::interface(), raw_pointer.cast())?
            })?)
        };
        let (globals, queue) = registry_queue_init::<Protocol>(&conn)?;
        let qh = queue.handle();
        let constraints: Option<ZwpPointerConstraintsV1> = globals.bind(&qh, 1..=1, ()).ok();
        let manager: Option<ZwpRelativePointerManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
        let relative = manager
            .as_ref()
            .zip(wl_pointer.as_ref())
            .map(|(m, p)| m.get_relative_pointer(p, &qh, ()));
        let presentation = globals.bind(&qh, 1..=1, ()).ok();
        crate::bench::trace(format_args!(
            "capabilities presentation={}",
            presentation.is_some() as u8
        ));
        crate::info!(
            "GTK spike: pointer constraints={} relative pointer={}",
            constraints.is_some(),
            manager.is_some()
        );
        Ok(Self {
            conn,
            queue,
            state: Protocol {
                lock: None,
                active: false,
                unlocked: false,
                hub,
                window: id,
            },
            constraints,
            manager,
            relative,
            presentation,
            surface: wl_surface,
            pointer: wl_pointer,
            requested: false,
            blocked: false,
            focused: false,
            keyboard: window.is_active(),
            _display: display,
            _surface: surface,
            _device: pointer,
        })
    }
    pub(super) fn request(&mut self, enabled: bool) {
        crate::info!(
            "GTK spike: capture requested={enabled} pointer_focus={} keyboard_focus={} blocked={}",
            self.focused,
            self.keyboard,
            self.blocked
        );
        self.requested = enabled;
        if !enabled {
            self.blocked = false;
            self.destroy_lock();
        }
        self.sync();
    }
    pub(super) fn focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.cancel();
        }
        self.sync();
    }
    fn sync(&mut self) {
        if self.requested
            && !self.blocked
            && self.focused
            && self.keyboard
            && self.state.lock.is_none()
            && let (Some(c), Some(_), Some(pointer)) =
                (&self.constraints, &self.relative, &self.pointer)
        {
            crate::info!(
                "GTK spike: requesting pointer lock window {}",
                self.state.window
            );
            self.state.lock = Some(c.lock_pointer(
                &self.surface,
                pointer,
                None,
                Lifetime::Oneshot,
                &self.queue.handle(),
                (),
            ));
            let _ = self.conn.flush();
        }
    }
    pub(super) fn keyboard_focus(&mut self, active: bool) {
        self.keyboard = active;
        if !active {
            self.cancel();
        }
        self.sync();
    }
    pub(super) fn active(&self) -> bool {
        self.state.active
    }
    fn destroy_lock(&mut self) {
        if let Some(lock) = self.state.lock.take() {
            lock.destroy();
        }
        self.state.active = false;
        let _ = self.conn.flush();
    }
    pub(super) fn cancel(&mut self) {
        self.blocked |= self.requested;
        self.destroy_lock();
    }
    pub(super) fn dispatch(&mut self) {
        if self.pointer.is_none()
            && let Some(device) = self._display.default_seat().and_then(|s| s.pointer())
        {
            let raw = unsafe { gdk_wayland_device_get_wl_pointer(device.to_glib_none().0) };
            if !raw.is_null() {
                let pointer = unsafe { ObjectId::from_ptr(WlPointer::interface(), raw.cast()) }
                    .ok()
                    .and_then(|id| WlPointer::from_id(&self.conn, id).ok());
                if let Some(pointer) = pointer {
                    self.relative = self
                        .manager
                        .as_ref()
                        .map(|m| m.get_relative_pointer(&pointer, &self.queue.handle(), ()));
                    self.pointer = Some(pointer);
                    self._device = Some(device);
                    self.sync();
                }
            }
        }
        // GDK reads the shared display. Drain only our extension queue; never
        // dispatch/read GDK's own events through a competing Wayland event loop.
        if let Err(e) = self.queue.dispatch_pending(&mut self.state) {
            crate::warn!("GTK extensions: {e}");
        }
        if self.state.unlocked {
            self.state.unlocked = false;
            self.cancel();
        }
    }
    pub(super) fn feedback(&self, stamp: u64) {
        if let Some(p) = &self.presentation {
            p.feedback(&self.surface, &self.queue.handle(), stamp);
        }
        let _ = self.conn.flush();
    }
}
impl Drop for Interop {
    fn drop(&mut self) {
        self.destroy_lock();
        if let Some(r) = self.relative.take() {
            r.destroy();
        }
        if let Some(m) = self.manager.take() {
            m.destroy();
        }
        if let Some(c) = self.constraints.take() {
            c.destroy();
        }
        if let Some(p) = self.presentation.take() {
            p.destroy();
        }
        let _ = self.conn.flush();
    }
}
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Protocol {
    fn event(
        _: &mut Self,
        _: &wl_registry::WlRegistry,
        _: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
impl Dispatch<ZwpLockedPointerV1, ()> for Protocol {
    fn event(
        s: &mut Self,
        lock: &ZwpLockedPointerV1,
        event: zwp_locked_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if s.lock.as_ref() != Some(lock) {
            return;
        }
        match event {
            zwp_locked_pointer_v1::Event::Locked => {
                s.active = true;
                crate::info!("GTK spike: pointer capture locked window {}", s.window);
            }
            zwp_locked_pointer_v1::Event::Unlocked => {
                s.unlocked = true;
                s.active = false;
            }
            _ => {}
        }
    }
}
impl Dispatch<ZwpRelativePointerV1, ()> for Protocol {
    fn event(
        s: &mut Self,
        _: &ZwpRelativePointerV1,
        event: zwp_relative_pointer_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if s.active
            && let zwp_relative_pointer_v1::Event::RelativeMotion {
                dx_unaccel,
                dy_unaccel,
                ..
            } = event
        {
            s.hub.input(&format!(
                "pointer_relative {} {dx_unaccel} {dy_unaccel}",
                s.window
            ));
        }
    }
}
impl Dispatch<WpPresentation, ()> for Protocol {
    fn event(
        _: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            crate::bench::trace(format_args!("clock id={clk_id}"));
        }
    }
}
impl Dispatch<WpPresentationFeedback, u64> for Protocol {
    fn event(
        s: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        stamp: &u64,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                flags,
                ..
            } => {
                let ns = ((u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo)) * 1_000_000_000
                    + u64::from(tv_nsec);
                crate::bench::trace(format_args!(
                    "present frame={stamp} window={} ns={ns} refresh_ns={refresh} flags={flags:?} backend=gtk-parent",
                    s.window
                ));
            }
            wp_presentation_feedback::Event::Discarded => {
                crate::bench::trace(format_args!("discard frame={stamp} window={}", s.window))
            }
            _ => {}
        }
    }
}
wayland_client::delegate_noop!(Protocol: ignore ZwpPointerConstraintsV1);
wayland_client::delegate_noop!(Protocol: ignore ZwpRelativePointerManagerV1);
