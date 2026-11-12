//! Typed mailbox messages for the binding transitions of one connection.
//!
//! A binding is prepared, installed, confirmed and retired in that order, and
//! each step names the exact attempt it acts on, so a replayed or stale step
//! cannot touch a later binding on the same physical socket. The connection
//! actor handles each of them as one step of its binding ledger
//! (`super::binding`). Nothing sends them yet: the connection's own bind and
//! resume handling drives the same ledger from inside its turn, and the
//! account coordinator is the sender these messages are for.

use std::sync::atomic::{AtomicU64, Ordering};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef};

/// Caller-generated monotonic key for retrying a canceled actor request.
///
/// Use a fresh ID per logical operation and reuse it only to retry while that
/// operation remains unresolved. Each actor admits IDs in strictly increasing
/// order; once a higher ID is admitted, any unseen lower ID is permanently
/// rejected. This gives bounded replay rejection without a tombstone set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::server::routes::websocket) struct ConnectionActorOperationId(u64);

impl ConnectionActorOperationId {
    /// Mint an operation key that may be replayed to recover an in-flight
    /// request after its original ask future is canceled. `None` once the
    /// process has exhausted the key space.
    pub(in crate::server::routes::websocket) fn fresh() -> Option<Self> {
        static NEXT_OPERATION_ID: AtomicU64 = AtomicU64::new(1);
        NEXT_OPERATION_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current.checked_add(1)
            })
            .ok()
            .map(Self)
    }
}

/// Opaque actor-issued permission to install one fresh binding attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::server::routes::websocket) struct ConnectionActorBindingAttempt {
    pub(in crate::server::routes::websocket) connection_ref: ConnectionRef,
    pub(in crate::server::routes::websocket) binding_ref: BindingRef,
    pub(in crate::server::routes::websocket) operation_id: ConnectionActorOperationId,
    pub(in crate::server::routes::websocket) epoch: u64,
}

/// Prepare one fresh binding attempt. Replaying the same operation ID while
/// preparation is pending returns the exact same actor-minted attempt.
pub(in crate::server::routes::websocket) struct PrepareBinding {
    pub(in crate::server::routes::websocket) operation_id: ConnectionActorOperationId,
}

/// Install one actor-issued bind or resume attempt on this physical
/// connection. Retrying the same attempt reports the already-installed exact
/// outcome.
pub(in crate::server::routes::websocket) struct InstallBinding {
    pub(in crate::server::routes::websocket) attempt: ConnectionActorBindingAttempt,
}

/// Confirm that the caller received the installed binding result. If this
/// handoff is lost, the exact physical connection stops at its configured
/// deadline instead of retaining an unacknowledged active binding forever.
pub(in crate::server::routes::websocket) struct ConfirmBindingInstall(
    pub(in crate::server::routes::websocket) ConnectionActorBindingAttempt,
);

/// Retire one exact binding attempt. A stale completion cannot clear a later
/// binding on the same physical socket.
pub(in crate::server::routes::websocket) struct RetireBinding(
    pub(in crate::server::routes::websocket) BindingRef,
);
