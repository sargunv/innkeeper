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
    wl_callback, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_display,
    wl_seat,
};
use wayland_client::{Connection, Proxy, QueueHandle};

use super::State;
use crate::system::Shared;
use crate::system::clipboard::{ClipboardHub, Offered, receive_offer, serve};

pub(crate) struct Clipboard {
    hub: Arc<ClipboardHub>,
    display: wl_display::WlDisplay,
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
        conn: &Connection,
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
            display: conn.display(),
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
        // The compositor takes or refuses the source while it handles
        // set_selection, so a cancel before this sync's done is a refusal.
        self.display.sync(qh, Sent(self.queue.generation()));
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
        if !self.queue.selection() {
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

/// Decides when an Android copy becomes the selection. GNOME takes a
/// selection only from the client with keyboard focus, so a copy waits for
/// focus. Focus can move before the compositor reads `set_selection`. It then
/// cancels the source, and the copy waits for the next focus.
struct Queue<T> {
    pending: Option<T>,
    /// The copy last handed to the compositor and its generation, until a
    /// sync after it confirms that the compositor took it.
    sent: Option<(u64, T)>,
    generation: u64,
    serial: Option<u32>,
    enter_snapshot: bool,
    skip_snapshot: bool,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            pending: None,
            sent: None,
            generation: 0,
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
        self.generation += 1;
        self.sent = Some((self.generation, copy.clone()));
        Some((serial, copy))
    }

    /// The generation of the copy last handed to the compositor.
    fn generation(&self) -> u64 {
        self.generation
    }

    /// The sync after `generation`'s copy is done, so the compositor took it.
    fn confirmed(&mut self, generation: u64) {
        if self.sent.as_ref().is_some_and(|(g, _)| *g == generation) {
            self.sent = None;
        }
    }

    /// A selection event. Returns whether to read it into Android.
    fn selection(&mut self) -> bool {
        self.enter_snapshot = false;
        !std::mem::take(&mut self.skip_snapshot)
    }

    /// The compositor cancelled the current source. Before its confirmation,
    /// that's a refusal, and the copy waits for the next focus. After it,
    /// another client's copy replaced it.
    fn cancelled(&mut self) {
        if let Some((_, copy)) = self.sent.take() {
            self.pending.get_or_insert(copy);
        }
    }
}

/// A sync after `set_selection`, with the generation of the copy it sent.
struct Sent(u64);
impl Dispatch2<wl_callback::WlCallback, State> for Sent {
    fn event(
        &self,
        st: &mut State,
        _: &wl_callback::WlCallback,
        event: wl_callback::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wl_callback::Event::Done { .. } = event
            && let Some(c) = st.clipboard.as_mut()
        {
            c.queue.confirmed(self.0);
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
        assert!(!q.selection());
        q.confirmed(q.generation());
        assert!(q.selection());
        assert_eq!(q.focus(None), None);
        assert_eq!(q.focus(Some(8)), None);
    }

    #[test]
    fn a_refused_copy_is_published_on_the_next_focus() {
        let mut q = Queue::default();
        q.focus(Some(7));
        assert!(q.selection());
        // Focus moves to another window before the compositor reads the
        // copy, so it cancels the source before the sync is done.
        assert_eq!(q.copy("a"), Some((7, "a")));
        q.focus(None);
        q.cancelled();
        assert_eq!(q.focus(Some(9)), Some((9, "a")));
    }

    #[test]
    fn a_replaced_copy_stays_replaced() {
        let mut q = Queue::default();
        q.focus(Some(7));
        q.selection();
        assert_eq!(q.copy("a"), Some((7, "a")));
        q.confirmed(q.generation());
        q.focus(None);
        // Another client copied, and its selection is the snapshot to read.
        q.cancelled();
        assert_eq!(q.focus(Some(9)), None);
        assert!(q.selection());
    }

    #[test]
    fn focus_between_the_bridges_windows_sends_no_snapshot() {
        let mut q = Queue::default();
        q.focus(Some(7));
        q.selection();
        // Mutter sends no selection when focus stays with this client.
        q.focus(None);
        q.focus(Some(8));
        assert_eq!(q.copy("a"), Some((8, "a")));
        q.confirmed(q.generation());
        // A desktop app's copy replaces it and must stay the selection.
        q.focus(None);
        q.cancelled();
        assert_eq!(q.focus(Some(9)), None);
    }

    #[test]
    fn a_newer_copy_outranks_a_refused_one() {
        let mut q = Queue::default();
        q.focus(Some(7));
        q.selection();
        q.copy("a");
        q.focus(None);
        q.copy("b");
        q.cancelled();
        assert_eq!(q.focus(Some(9)), Some((9, "b")));
    }

    #[test]
    fn an_older_copys_sync_confirms_nothing() {
        let mut q = Queue::default();
        q.focus(Some(7));
        q.copy("a");
        let a = q.generation();
        q.copy("b");
        q.confirmed(a);
        q.focus(None);
        q.cancelled();
        assert_eq!(q.focus(Some(9)), Some((9, "b")));
    }
}
