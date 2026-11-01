//! Graphics tablets (`zwp_tablet_v2`) as input lines ([`super::proto`]). The
//! lines report a pen or eraser in proximity of a window's content, with its
//! position, pressure, distance, tilt and buttons, and one `tablet_frame` per
//! tool frame. The system app injects them as Android stylus events
//! (`docs/input.md`).
//!
//! Only one tool is tracked at a time. A second tool that comes into
//! proximity while another is in is ignored until it leaves and comes back.
//! Pads (buttons, rings, strips) are bound only because the protocol creates
//! them, and are not forwarded.

use std::collections::HashMap;

use sctk::dispatch2::Dispatch2;
use sctk::reexports::client::backend::{ObjectData, ObjectId};
use sctk::reexports::client::globals::GlobalList;
use sctk::reexports::client::protocol::wl_seat::WlSeat;
use sctk::reexports::client::{Connection, Proxy, QueueHandle};
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use wayland_protocols::wp::tablet::zv2::client::{
    zwp_tablet_manager_v2::ZwpTabletManagerV2,
    zwp_tablet_pad_group_v2::{self, ZwpTabletPadGroupV2},
    zwp_tablet_pad_ring_v2::ZwpTabletPadRingV2,
    zwp_tablet_pad_strip_v2::ZwpTabletPadStripV2,
    zwp_tablet_pad_v2::{self, ZwpTabletPadV2},
    zwp_tablet_seat_v2::{self, ZwpTabletSeatV2},
    zwp_tablet_tool_v2::{self, ZwpTabletToolV2},
    zwp_tablet_v2::{self, ZwpTabletV2},
};

use super::Hub;
use super::seat::Cursor;
use crate::display::wl::State;

#[derive(Default)]
pub(crate) struct Tablets {
    manager: Option<ZwpTabletManagerV2>,
    seat: Option<ZwpTabletSeatV2>,
    /// Tool -> its type (a `BTN_TOOL_*` code) and cursor-shape device.
    tools: HashMap<ObjectId, (u32, Option<WpCursorShapeDeviceV1>)>,
    /// The tool in proximity of a window's content, and that window.
    focus: Option<(ObjectId, u32)>,
    /// The tool whose events go out until its next frame. This includes the
    /// frame after its proximity_out.
    framing: Option<ObjectId>,
    last_time: u32,
}

/// User data of every tablet object.
pub(crate) struct T;

impl Tablets {
    pub(crate) fn bind(globals: &GlobalList, qh: &QueueHandle<State>) -> Tablets {
        Tablets {
            // Version 2 only adds pad dials and the bus type.
            manager: globals.bind(qh, 1..=1, T).ok(),
            ..Default::default()
        }
    }

    pub(crate) fn available(&self) -> bool {
        self.manager.is_some()
    }

    pub(crate) fn attach(&mut self, seat: &WlSeat, qh: &QueueHandle<State>) {
        if self.seat.is_none()
            && let Some(m) = &self.manager
        {
            self.seat = Some(m.get_tablet_seat(seat, qh, T));
        }
    }

    fn forwarding(&self, tool: &ObjectId) -> Option<u32> {
        match &self.focus {
            Some((t, w)) if t == tool => Some(*w),
            _ if self.framing.as_ref() == Some(tool) => Some(u32::MAX),
            _ => None,
        }
    }

    /// Ends proximity for a tool over window `id`, which is gone.
    pub(crate) fn forget(&mut self, id: u32, hub: &Hub) {
        if self.focus.as_ref().is_some_and(|(_, w)| *w == id) {
            self.focus = None;
            self.framing = None;
            hub.input("tablet_proximity_out");
            hub.input(&format!("tablet_frame {}", self.last_time));
        }
    }
}

impl Dispatch2<ZwpTabletManagerV2, State> for T {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpTabletManagerV2,
        _: <ZwpTabletManagerV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch2<ZwpTabletSeatV2, State> for T {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpTabletSeatV2,
        _: zwp_tablet_seat_v2::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        // Tools are registered at their first event (`type`).
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<State>) -> std::sync::Arc<dyn ObjectData> {
        match opcode {
            zwp_tablet_seat_v2::EVT_TABLET_ADDED_OPCODE => qh.make_data::<ZwpTabletV2, _>(T),
            zwp_tablet_seat_v2::EVT_TOOL_ADDED_OPCODE => qh.make_data::<ZwpTabletToolV2, _>(T),
            zwp_tablet_seat_v2::EVT_PAD_ADDED_OPCODE => qh.make_data::<ZwpTabletPadV2, _>(T),
            _ => panic!("zwp_tablet_seat_v2: unknown child opcode {opcode}"),
        }
    }
}

impl Dispatch2<ZwpTabletToolV2, State> for T {
    fn event(
        &self,
        st: &mut State,
        tool: &ZwpTabletToolV2,
        e: zwp_tablet_tool_v2::Event,
        _: &Connection,
        qh: &QueueHandle<State>,
    ) {
        use zwp_tablet_tool_v2::Event as E;
        let id = tool.id();
        let tabs = &mut st.seat.tablets;
        match e {
            E::Type { tool_type } => {
                tabs.tools.entry(id).or_default().0 = tool_type.into();
            }
            E::Removed => {
                if let Some((_, Some(c))) = tabs.tools.remove(&id) {
                    c.destroy();
                }
                if let Some((_, w)) = tabs.focus.clone().filter(|(t, _)| *t == id) {
                    tabs.forget(w, &st.hub);
                }
                if tabs.framing.as_ref() == Some(&id) {
                    tabs.framing = None;
                }
                tool.destroy();
            }
            E::ProximityIn {
                serial, surface, ..
            } => {
                st.seat.last_serial = serial;
                let win = st.window_of(&surface);
                // Show the default arrow over any of our surfaces.
                if let Some(m) = &st.seat.cursor_shape {
                    let entry = st.seat.tablets.tools.entry(id.clone()).or_default();
                    entry
                        .1
                        .get_or_insert_with(|| m.get_tablet_tool_v2(tool, qh, Cursor))
                        .set_shape(serial, Shape::Default);
                }
                let tabs = &mut st.seat.tablets;
                if let (None, Some((w, false))) = (&tabs.focus, win) {
                    let ty = tabs.tools.get(&id).map_or(0, |t| t.0);
                    tabs.focus = Some((id.clone(), w));
                    tabs.framing = Some(id);
                    st.hub.input(&format!("tablet_proximity_in {ty} {w}"));
                }
            }
            E::ProximityOut => {
                if tabs.focus.as_ref().is_some_and(|(t, _)| *t == id) {
                    tabs.focus = None;
                    st.hub.input("tablet_proximity_out");
                }
            }
            E::Frame { time } => {
                if tabs.forwarding(&id).is_some() {
                    tabs.last_time = time;
                    tabs.framing = tabs.focus.as_ref().map(|f| f.0.clone());
                    st.hub.input(&format!("tablet_frame {time}"));
                }
            }
            e => {
                let Some(w) = tabs.forwarding(&id) else {
                    return;
                };
                let line = match e {
                    E::Down { serial } => {
                        st.seat.last_serial = serial;
                        "tablet_down".to_string()
                    }
                    E::Up => "tablet_up".to_string(),
                    E::Motion { x, y } => {
                        let (x, y) = st.to_display(w, x, y);
                        format!("tablet_motion {x:.2} {y:.2}")
                    }
                    E::Pressure { pressure } => format!("tablet_pressure {pressure}"),
                    E::Distance { distance } => format!("tablet_distance {distance}"),
                    E::Tilt { tilt_x, tilt_y } => format!("tablet_tilt {tilt_x:.2} {tilt_y:.2}"),
                    E::Button {
                        serial,
                        button,
                        state,
                    } => {
                        st.seat.last_serial = serial;
                        let s: u32 = state.into();
                        format!("tablet_button {button} {}", (s == 1) as u8)
                    }
                    _ => return,
                };
                st.hub.input(&line);
            }
        }
    }
}

// Tablets and pads are bound because the protocol creates them. Their events
// are ignored.

impl Dispatch2<ZwpTabletV2, State> for T {
    fn event(
        &self,
        _: &mut State,
        t: &ZwpTabletV2,
        e: zwp_tablet_v2::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let zwp_tablet_v2::Event::Removed = e {
            t.destroy();
        }
    }
}

impl Dispatch2<ZwpTabletPadV2, State> for T {
    fn event(
        &self,
        _: &mut State,
        p: &ZwpTabletPadV2,
        e: zwp_tablet_pad_v2::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let zwp_tablet_pad_v2::Event::Removed = e {
            p.destroy();
        }
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<State>) -> std::sync::Arc<dyn ObjectData> {
        match opcode {
            zwp_tablet_pad_v2::EVT_GROUP_OPCODE => qh.make_data::<ZwpTabletPadGroupV2, _>(T),
            _ => panic!("zwp_tablet_pad_v2: unknown child opcode {opcode}"),
        }
    }
}

impl Dispatch2<ZwpTabletPadGroupV2, State> for T {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpTabletPadGroupV2,
        _: zwp_tablet_pad_group_v2::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }

    fn event_created_child(opcode: u16, qh: &QueueHandle<State>) -> std::sync::Arc<dyn ObjectData> {
        match opcode {
            zwp_tablet_pad_group_v2::EVT_RING_OPCODE => qh.make_data::<ZwpTabletPadRingV2, _>(T),
            zwp_tablet_pad_group_v2::EVT_STRIP_OPCODE => qh.make_data::<ZwpTabletPadStripV2, _>(T),
            _ => panic!("zwp_tablet_pad_group_v2: unknown child opcode {opcode}"),
        }
    }
}

impl Dispatch2<ZwpTabletPadRingV2, State> for T {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpTabletPadRingV2,
        _: <ZwpTabletPadRingV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}

impl Dispatch2<ZwpTabletPadStripV2, State> for T {
    fn event(
        &self,
        _: &mut State,
        _: &ZwpTabletPadStripV2,
        _: <ZwpTabletPadStripV2 as Proxy>::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
