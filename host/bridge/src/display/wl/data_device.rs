//! GNOME's focus-scoped clipboard (`wl_data_device`). Its objects and keyboard
//! serials must come from this display connection, not from the separate
//! data-control connection.
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
use crate::system::clipboard::{ClipboardHub, MARKER, Offered, receive_offer, serve};

pub(crate) struct Clipboard {
    hub: Arc<ClipboardHub>,
    manager: wl_data_device_manager::WlDataDeviceManager,
    device: Option<wl_data_device::WlDataDevice>,
    offers: HashMap<ObjectId, wl_data_offer::WlDataOffer>,
    types: HashMap<ObjectId, Vec<String>>,
    selection: Option<ObjectId>,
    source: Option<(wl_data_source::WlDataSource, Arc<Offered>)>,
    queue: Queue<Arc<Offered>>,
}

impl Clipboard {
    pub(super) fn new(
        globals: &GlobalList,
        qh: &QueueHandle<State>,
        shared: &Shared,
        ev: &EventLoop<State>,
    ) -> Result<Option<Self>> {
        // Check for the hub before binding anything. A session without the hub
        // neither reads nor writes the desktop clipboard.
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
                    let next = c.queue.copy(offered);
                    c.publish(next, &qh);
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
            queue: Queue::default(),
        }))
    }

    pub(crate) fn seat(&mut self, seat: &wl_seat::WlSeat, qh: &QueueHandle<State>) {
        if self.device.is_none() {
            self.device = Some(self.manager.get_data_device(seat, qh, Data));
        }
    }

    pub(crate) fn focus(&mut self, serial: Option<u32>, qh: &QueueHandle<State>) {
        let next = self.queue.focus(serial);
        self.publish(next, qh);
    }

    fn publish(&mut self, next: Option<(u32, Arc<Offered>)>, qh: &QueueHandle<State>) {
        let (Some((serial, offered)), Some(device)) = (next, self.device.as_ref()) else {
            return;
        };
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
        let types = offer
            .as_ref()
            .and_then(|o| self.types.get(&o.id()))
            .cloned()
            .unwrap_or_default();
        if !self.queue.selection(types.iter().any(|t| t == MARKER)) {
            return;
        }
        if let Some(offer) = offer {
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

/// Decides when an Android copy becomes the selection. GNOME takes a
/// selection only from the client with keyboard focus, so a copy waits for
/// focus. Focus can move before the compositor reads `set_selection`. It then
/// cancels the source, and the copy waits for the next focus.
struct Queue<T> {
    pending: Option<T>,
    /// The copy last handed to the compositor, until its selection event.
    sent: Option<T>,
    serial: Option<u32>,
    enter_snapshot: bool,
    skip_snapshot: bool,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            pending: None,
            sent: None,
            serial: None,
            enter_snapshot: false,
            skip_snapshot: false,
        }
    }
}

impl<T: Clone> Queue<T> {
    /// A copy in Android. Returns what to publish and with which serial.
    fn copy(&mut self, copy: T) -> Option<(u32, T)> {
        self.pending = Some(copy);
        self.next()
    }

    /// Keyboard enter with its serial, or leave.
    fn focus(&mut self, serial: Option<u32>) -> Option<(u32, T)> {
        self.serial = serial;
        self.enter_snapshot = serial.is_some();
        self.skip_snapshot = false;
        self.next()
    }

    fn next(&mut self) -> Option<(u32, T)> {
        let serial = self.serial?;
        let copy = self.pending.take()?;
        // The compositor sends its previous selection just after keyboard
        // enter. A queued Android copy is newer than that focus snapshot.
        self.skip_snapshot |= self.enter_snapshot;
        self.sent = Some(copy.clone());
        Some((serial, copy))
    }

    /// A selection event, which is `ours` when it offers the bridge's marker.
    /// Returns whether to read it into Android.
    fn selection(&mut self, ours: bool) -> bool {
        // A focus snapshot of the bridge's own earlier selection doesn't
        // confirm a copy sent on that enter.
        if ours && !self.enter_snapshot {
            self.sent = None;
        }
        self.enter_snapshot = false;
        !std::mem::take(&mut self.skip_snapshot)
    }

    /// The compositor cancelled the current source. If it took the source,
    /// another client's copy replaced it. If it never sent the source back as
    /// the selection, it refused it.
    fn cancelled(&mut self) {
        if let Some(copy) = self.sent.take() {
            self.pending.get_or_insert(copy);
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
            // Drag and drop is not handled, so release the unused offer.
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
                    c.queue.cancelled();
                }
                source.destroy();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Queue;

    #[test]
    fn a_copy_waits_for_focus() {
        let mut q = Queue::default();
        assert_eq!(q.copy("a"), None);
        assert_eq!(q.focus(Some(7)), Some((7, "a")));
        // The focus snapshot is older than the copy, so it stays out of
        // Android, and the copy's own selection follows.
        assert!(!q.selection(false));
        assert!(q.selection(true));
        assert_eq!(q.focus(None), None);
        assert_eq!(q.focus(Some(8)), None);
    }

    #[test]
    fn a_refused_copy_is_published_on_the_next_focus() {
        let mut q = Queue::default();
        q.focus(Some(7));
        assert!(q.selection(false));
        // Focus moves to another window before the compositor reads the
        // copy, so it cancels the source without a selection event.
        assert_eq!(q.copy("a"), Some((7, "a")));
        q.focus(None);
        q.cancelled();
        assert_eq!(q.focus(Some(9)), Some((9, "a")));
        assert!(!q.selection(false));
        assert!(q.selection(true));
    }

    #[test]
    fn a_replaced_copy_stays_replaced() {
        let mut q = Queue::default();
        q.focus(Some(7));
        q.selection(false);
        assert_eq!(q.copy("a"), Some((7, "a")));
        assert!(q.selection(true));
        q.focus(None);
        // Another client copied, and its selection is the snapshot to read.
        q.cancelled();
        assert_eq!(q.focus(Some(9)), None);
        assert!(q.selection(false));
    }

    #[test]
    fn a_newer_copy_outranks_a_refused_one() {
        let mut q = Queue::default();
        q.focus(Some(7));
        q.selection(false);
        q.copy("a");
        q.focus(None);
        q.copy("b");
        q.cancelled();
        assert_eq!(q.focus(Some(9)), Some((9, "b")));
    }

    #[test]
    fn a_snapshot_of_an_earlier_copy_confirms_nothing() {
        let mut q = Queue::default();
        q.focus(Some(7));
        q.selection(false);
        q.copy("a");
        q.selection(true);
        q.focus(None);
        q.copy("b");
        assert_eq!(q.focus(Some(9)), Some((9, "b")));
        // The snapshot is still the bridge's "a", and then focus moves away
        // before the compositor reads "b".
        assert!(!q.selection(true));
        q.focus(None);
        q.cancelled();
        assert_eq!(q.focus(Some(11)), Some((11, "b")));
    }
}
