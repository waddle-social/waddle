//! The tokio runtime the network and keyring code runs on.
//!
//! GTK owns the main thread and its GLib main loop; tokio runs on its own
//! worker threads. UI code awaits [`spawn`]ed work from
//! `glib::spawn_future_local`, so nothing blocks the main loop and no GTK
//! object crosses to a tokio thread.

use std::future::Future;
use std::sync::OnceLock;

use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

fn runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("waddle-tokio")
            .enable_all()
            .build()
            .unwrap_or_else(|error| panic!("the tokio runtime failed to start: {error}"))
    })
}

/// Run `future` on the tokio runtime. The returned handle can be awaited
/// from the GLib main context.
pub fn spawn<F>(future: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    runtime().spawn(future)
}
