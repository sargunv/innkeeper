//! GNOME's focus-scoped clipboard. Objects and keyboard serials must belong
//! to this display connection, rather than the separate data-control connection.
use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use calloop::EventLoop;
use calloop::channel::{self, Event};
use sctk::dispatch2::Dispatch2;
use wayland_client::backend::ObjectId;
use wayland_client::globals::GlobalList;
use wayland_client::protocol::{
    wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_seat,
};
use wayland_client::{Connection, Proxy, QueueHandle};

use super::State;
use crate::system::Shared;
use crate::system::clipboard::{ClipboardHub, Offered, receive_offer, serve};

pub(crate) struct Clipboard {
    hub: Arc<ClipboardHub>,
    manager: wl_data_device_manager::WlDataDeviceManager,
    device: Option<wl_data_device::WlDataDevice>,
    offers: HashMap<ObjectId, wl_data_offer::WlDataOffer>,
    types: HashMap<ObjectId, Vec<String>>,
    selection: Option<ObjectId>,
    source: Option<(wl_data_source::WlDataSource, Arc<Offered>)>,
    pending: Option<Arc<Offered>>,
    serial: Option<u32>,
    enter_snapshot: bool,
    skip_snapshot: bool,
}

impl Clipboard {
    pub(super) fn new(
        globals: &GlobalList,
        qh: &QueueHandle<State>,
        shared: &Shared,
        ev: &EventLoop<State>,
    ) -> Result<Option<Self>> {
        // Gate before binding anything: a session without the hub neither reads
        // nor writes the desktop clipboard.
        let Some(hub) = shared.clipboard().cloned() else {
            return Ok(None);
        };
        let (tx, rx) = channel::channel();
        if !hub.attach(tx) {
            return Ok(None);
        }
        let manager = globals.bind(qh, 1..=3, Data)?;
        let qh = qh.clone();
        ev.handle()
            .insert_source(rx, move |event, _, st| {
                if let Event::Msg(offered) = event
                    && let Some(c) = &mut st.clipboard
                {
                    c.pending = Some(offered);
                    c.publish(&qh);
                }
            })
            .map_err(|e| anyhow::anyhow!("clipboard channel: {e}"))?;
        Ok(Some(Self {
            hub,
            manager,
            device: None,
            offers: HashMap::new(),
            types: HashMap::new(),
            selection: None,
            source: None,
            pending: None,
            serial: None,
            enter_snapshot: false,
            skip_snapshot: false,
        }))
    }

    pub(crate) fn seat(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<State>) {
        if self.device.is_none() {
            self.device = Some(self.manager.get_data_device(seat, qh, Data));
        }
    }

    pub(crate) fn focus(&mut self, serial: Option<u32>, qh: &QueueHandle<State>) {
        self.serial = serial;
        self.enter_snapshot = serial.is_some();
        self.skip_snapshot = false;
        self.publish(qh);
    }

    fn publish(&mut self, qh: &QueueHandle<State>) {
        let (Some(serial), Some(device)) = (self.serial, self.device.as_ref()) else {
            return;
        };
        let Some(offered) = self.pending.take() else {
            return;
        };
        // The compositor sends its previous selection just after keyboard
        // enter. A queued Android copy is newer than that focus snapshot.
        self.skip_snapshot |= self.enter_snapshot;
        self.hub.invalidate_reads();
        let source = self.manager.create_data_source(qh, Data);
        for mime in offered.types.keys() {
            source.offer(mime.clone());
        }
        device.set_selection(Some(&source), serial);
        if let Some((old, _)) = self.source.replace((source, offered)) {
            old.destroy();
        }
    }

    fn selection(&mut self, offer: Option<wl_data_offer::WlDataOffer>, conn: &Connection) {
        self.selection = offer.as_ref().map(Proxy::id);
        for (id, old) in self
            .offers
            .extract_if(|id, _| Some(id) != self.selection.as_ref())
        {
            self.types.remove(&id);
            old.destroy();
        }
        self.enter_snapshot = false;
        if std::mem::take(&mut self.skip_snapshot) {
            return;
        }
        if let Some(offer) = offer {
            let types = self.types.get(&offer.id()).cloned().unwrap_or_default();
            // Selection events are delivered on keyboard enter. Consume them
            // after refocus even when nothing else changed in Android.
            receive_offer(&types, conn, self.hub.clone(), |mime, fd| {
                offer.receive(mime.into(), fd)
            });
        } else {
            self.hub.invalidate_reads();
        }
    }
}

struct Data;
impl Dispatch2<wl_data_device_manager::WlDataDeviceManager, State> for Data {
    fn event(
        &self,
        _: &mut State,
        _: &wl_data_device_manager::WlDataDeviceManager,
        _: wl_data_device_manager::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
    }
}
impl Dispatch2<wl_data_device::WlDataDevice, State> for Data {
    fn event(
        &self,
        st: &mut State,
        _: &wl_data_device::WlDataDevice,
        event: wl_data_device::Event,
        conn: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(c) = st.clipboard.as_mut() else {
            return;
        };
        match event {
            wl_data_device::Event::DataOffer { id } => {
                c.types.insert(id.id(), Vec::new());
                c.offers.insert(id.id(), id);
            }
            wl_data_device::Event::Selection { id } => c.selection(id, conn),
            // No drag-and-drop handling here. Release its unused offer.
            wl_data_device::Event::Enter {
                id: Some(offer), ..
            } => {
                c.types.remove(&offer.id());
                c.offers.remove(&offer.id());
                offer.destroy();
            }
            _ => {}
        }
    }
    fn event_created_child(
        opcode: u16,
        qh: &QueueHandle<State>,
    ) -> Arc<dyn wayland_client::backend::ObjectData> {
        assert_eq!(opcode, wl_data_device::EVT_DATA_OFFER_OPCODE);
        qh.make_data::<wl_data_offer::WlDataOffer, _>(Data)
    }
}
impl Dispatch2<wl_data_offer::WlDataOffer, State> for Data {
    fn event(
        &self,
        st: &mut State,
        offer: &wl_data_offer::WlDataOffer,
        event: wl_data_offer::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event
            && let Some(c) = st.clipboard.as_mut()
            && let Some(types) = c.types.get_mut(&offer.id())
            && types.len() < 64
        {
            types.push(mime_type);
        }
    }
}
impl Dispatch2<wl_data_source::WlDataSource, State> for Data {
    fn event(
        &self,
        st: &mut State,
        source: &wl_data_source::WlDataSource,
        event: wl_data_source::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        let Some(c) = st.clipboard.as_mut() else {
            return;
        };
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                if let Some((current, offered)) = &c.source
                    && current == source
                {
                    serve(offered, mime_type, fd);
                }
            }
            wl_data_source::Event::Cancelled => {
                if c.source.as_ref().is_some_and(|(s, _)| s == source) {
                    c.source = None;
                }
                source.destroy();
            }
            _ => {}
        }
    }
}
