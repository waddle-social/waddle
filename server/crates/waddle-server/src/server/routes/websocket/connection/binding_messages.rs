//! Typed mailbox messages for the binding transitions of one connection.
//!
//! A binding is prepared, installed, confirmed and retired in that order, and
//! each step names the exact attempt it acts on, so a replayed or stale step
//! cannot touch a later binding on the same physical socket. The connection
//! actor has no handlers for these messages while the registration path
//! installs bindings itself; they are the contract those handlers follow once
//! the actor owns the outbound endpoint.

use std::sync::atomic::{AtomicU64, Ordering};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef};

/// Caller-generated monotonic key for retrying a canceled actor request.
///
/// Use a fresh ID per logical operation and reuse it only to retry while that
/// operation remains unresolved. Each actor admits IDs in strictly increasing
/// order; once a higher ID is admitted, any unseen lower ID is permanently
/// rejected. This gives bounded replay rejection without a tombstone set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct ConnectionActorOperationId(u64);

impl ConnectionActorOperationId {
    /// Mint an operation key that may be replayed to recover an in-flight
    /// request after its original ask future is canceled. `None` once the
    /// process has exhausted the key space.
    pub(super) fn fresh() -> Option<Self> {
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
pub(super) struct ConnectionActorBindingAttempt {
    pub(super) connection_ref: ConnectionRef,
    pub(super) binding_ref: BindingRef,
    pub(super) operation_id: ConnectionActorOperationId,
    pub(super) epoch: u64,
}

impl ConnectionActorBindingAttempt {
    /// Exact binding identity the actor minted for this transition.
    pub(super) fn binding_ref(&self) -> &BindingRef {
        &self.binding_ref
    }
}

/// Prepare one fresh binding attempt. Replaying the same operation ID while
/// preparation is pending returns the exact same actor-minted attempt.
pub(super) struct PrepareBinding {
    pub(super) operation_id: ConnectionActorOperationId,
}

/// Install one actor-issued bind or resume attempt on this physical
/// connection. Retrying the same attempt reports the already-installed exact
/// outcome.
pub(super) struct InstallBinding {
    pub(super) attempt: ConnectionActorBindingAttempt,
}

/// Confirm that the caller received the installed binding result. If this
/// handoff is lost, the exact physical connection stops at its configured
/// deadline instead of retaining an unacknowledged active binding forever.
pub(super) struct ConfirmBindingInstall(pub(super) ConnectionActorBindingAttempt);

/// Retire one exact binding attempt. A stale completion cannot clear a later
/// binding on the same physical socket.
pub(super) struct RetireBinding(pub(super) BindingRef);
