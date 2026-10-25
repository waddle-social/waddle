//! Per-user actor managing connection state, presence, and delivery.
//!
//! One `UserActor` exists per bare JID. It owns all mutable per-user state
//! (connected resources with their outbound channels, presence, pending
//! subscriptions, carbons) and processes messages sequentially, removing the
//! need for external locking.
//!
//! The delivery surface lives in the [`delivery`] submodule: it reproduces
//! the `ConnectionRegistry` routing invariants (RFC 6121 §8.5.2.1 bare-JID
//! resource selection, the `DeliveryKind` recipient-pass split, non-blocking
//! `try_send` fan-out with typed [`BroadcastOutcome`], and the Q7b
//! pending-flush SM-row binding) inside the actor so the DashMap delivery
//! path can be retired once the migration's invariant tests cover it (see
//! ADR-0017 Phase 1).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use jid::{BareJid, FullJid};
use kameo::message::Context;
use kameo::Actor;
use tracing::debug;

use crate::registry::connection_registry::{ConnectionEntry, PresenceState};
use crate::Stanza;

pub mod delivery;

/// Cross-crate test controls, kept behind the existing `test-utils` feature.
#[cfg(feature = "test-utils")]
pub mod test_support {
    use super::UserActor;
    use std::sync::Arc;
    use tokio::sync::{oneshot, Notify};

    pub struct GateMailbox {
        pub entered: Arc<Notify>,
        pub release_rx: oneshot::Receiver<()>,
    }

    impl kameo::message::Message<GateMailbox> for UserActor {
        type Reply = ();

        async fn handle(
            &mut self,
            msg: GateMailbox,
            _ctx: &mut kameo::message::Context<Self, Self::Reply>,
        ) -> Self::Reply {
            msg.entered.notify_one();
            let _ = msg.release_rx.await;
        }
    }

    pub struct MailboxNoop;

    impl kameo::message::Message<MailboxNoop> for UserActor {
        type Reply = ();

        async fn handle(
            &mut self,
            _msg: MailboxNoop,
            _ctx: &mut kameo::message::Context<Self, Self::Reply>,
        ) -> Self::Reply {
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;

mod implementation;
pub use implementation::health_check_or_wedge_kill;
pub use implementation::ClearPresenceState;
pub use implementation::ConflictCloseAllResources;
pub use implementation::DrainPendingSubscriptions;
pub use implementation::GetAvailableResources;
pub use implementation::GetOtherResources;
pub use implementation::GetPresenceState;
pub use implementation::GetResources;
pub use implementation::GetRoutingResources;
pub use implementation::HealthCheck;
pub use implementation::IsCarbonsEnabled;
pub use implementation::IsConnected;
pub use implementation::QueuePendingSubscription;
pub use implementation::RegisterConnection;
pub use implementation::RegisterConnectionIfOwnerOrAbsent;
pub use implementation::ResourceCount;
pub use implementation::ResourceRoutingState;
pub use implementation::SetCarbonsEnabled;
pub use implementation::UnregisterConnection;
pub use implementation::UnregisterConnectionAndReportEmpty;
pub use implementation::UnregisterConnectionOutcome;
pub use implementation::UpdatePresence;
pub use implementation::UpdatePresenceState;
pub use implementation::UserActor;
