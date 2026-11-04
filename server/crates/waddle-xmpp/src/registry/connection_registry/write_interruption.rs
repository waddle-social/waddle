//! Physical-socket write interruption shared with the connection owner.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Orders an authorized force-detach against synchronous socket write admission.
///
/// This belongs to one physical connection, not to a binding or occupancy
/// generation. Cancelling it does not prove that an already-started write, flush,
/// or socket cleanup has completed.
#[derive(Clone, Debug, Default)]
pub struct SocketWriteInterruption {
    state: Arc<InterruptionState>,
    signal: CancellationToken,
}

#[derive(Debug, Default)]
struct InterruptionState {
    gate: Mutex<()>,
    stopped: AtomicBool,
}

impl SocketWriteInterruption {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stop future write admission, then wake any parked transport operation.
    /// The state transition is the stop point; notification happens after the
    /// gate is released so a woken task cannot re-enter it under this lock.
    pub fn cancel(&self) {
        {
            let _guard = self
                .state
                .gate
                .lock()
                .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
            self.state.stopped.store(true, Ordering::Release);
        }
        self.signal.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.state.stopped.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        self.signal.cancelled().await;
    }

    pub fn cancelled_owned(self) -> impl std::future::Future<Output = ()> + Send + 'static {
        self.signal.cancelled_owned()
    }

    /// Admit a synchronous transport enqueue only if stop has not won.
    /// `operation` must neither await nor re-enter this interruption gate.
    pub fn with_uninterrupted<T>(&self, operation: impl FnOnce() -> T) -> Option<T> {
        let _guard = self
            .state
            .gate
            .lock()
            .unwrap_or_else(waddle_xmpp_core::sync::abort_on_poison);
        if self.state.stopped.load(Ordering::Acquire) {
            None
        } else {
            Some(operation())
        }
    }
}
