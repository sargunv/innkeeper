//! Runs blocking `innkeeper-core` operations on a worker thread, delivering
//! their progress events to the UI thread.

use std::future::Future;

use gtk::{gio, glib};
use innkeeper_core::ops::Event;

/// Runs `f` off the UI thread. Each [`Event`] it reports is handed to
/// `on_event` on the UI thread; the returned future resolves to `f`'s result.
pub fn run<T, F, E>(f: F, on_event: E) -> impl Future<Output = T>
where
    T: Send + 'static,
    F: FnOnce(&dyn Fn(Event)) -> T + Send + 'static,
    E: Fn(Event) + 'static,
{
    let (tx, rx) = async_channel::unbounded::<Event>();
    glib::spawn_future_local(async move {
        while let Ok(e) = rx.recv().await {
            on_event(e);
        }
    });
    let handle = gio::spawn_blocking(move || {
        f(&|e| {
            let _ = tx.send_blocking(e);
        })
    });
    async move {
        match handle.await {
            Ok(v) => v,
            Err(p) => std::panic::resume_unwind(p),
        }
    }
}

/// [`run`] without progress events.
pub fn run_quiet<T, F>(f: F) -> impl Future<Output = T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    run(move |_| f(), |_| {})
}
