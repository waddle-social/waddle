//! Dormant ownership core for the eventual WebSocket connection actor.
//!
//! The active `/ws` loop still owns its own state. This core consumes an
//! existing state without exposing it and serializes stop with operations that
//! hold the lifecycle gate. The actor fixture exercises this same core.

use super::state::WsConnState;
use super::supervised_io::StopReason;
#[cfg(feature = "test-support")]
use std::collections::HashSet;
#[cfg(feature = "test-support")]
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex, MutexGuard};
#[cfg(feature = "test-support")]
use tokio::sync::watch;
use waddle_xmpp::ownership::ConnectionRef;

#[cfg(feature = "test-support")]
#[path = "connection_actor.rs"]
pub mod fixture;

/// The unique protocol-state owner and its independently shared lifecycle gate.
struct ConnectionActorCore {
    state: WsConnState,
    lifecycle: Arc<ConnectionActorLifecycle>,
}

impl ConnectionActorCore {
    fn new(state: WsConnState) -> Self {
        Self {
            state,
            lifecycle: Arc::new(ConnectionActorLifecycle::new()),
        }
    }

    fn connection_ref(&self) -> &ConnectionRef {
        &self.state.connection_ref
    }
}

struct ConnectionActorLifecycle {
    state: Mutex<ConnectionActorLifecycleState>,
    #[cfg(feature = "test-support")]
    stop_callback_count: AtomicU64,
    #[cfg(feature = "test-support")]
    stop_callback_watch: watch::Sender<u64>,
}

#[derive(Default)]
struct ConnectionActorLifecycleState {
    stop_reason: Option<StopReason>,
    #[cfg(feature = "test-support")]
    pending_handled_operations: HashSet<fixture::ConnectionActorOperationId>,
    #[cfg(feature = "test-support")]
    pending_binding_handoff: Option<u64>,
}

struct ConnectionActorStopped;

struct ConnectionActorLifecycleGuard<'a> {
    state: MutexGuard<'a, ConnectionActorLifecycleState>,
}

impl ConnectionActorLifecycle {
    fn new() -> Self {
        #[cfg(feature = "test-support")]
        let (stop_callback_watch, _) = watch::channel(0);
        Self {
            state: Mutex::new(ConnectionActorLifecycleState::default()),
            #[cfg(feature = "test-support")]
            stop_callback_count: AtomicU64::new(0),
            #[cfg(feature = "test-support")]
            stop_callback_watch,
        }
    }

    fn lock_running(&self) -> Result<ConnectionActorLifecycleGuard<'_>, ConnectionActorStopped> {
        let state = self.lock_state();
        if state.stop_reason.is_some() {
            return Err(ConnectionActorStopped);
        }
        Ok(ConnectionActorLifecycleGuard { state })
    }

    fn request_stop(&self, reason: StopReason) -> bool {
        let mut state = self.lock_state();
        if state.stop_reason.is_some() {
            return false;
        }
        state.stop_reason = Some(reason);
        true
    }

    fn lock_state(&self) -> MutexGuard<'_, ConnectionActorLifecycleState> {
        match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}
