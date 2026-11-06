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
use std::sync::atomic::Ordering;

use jid::{BareJid, FullJid};
use kameo::message::Context;
use kameo::Actor;
use tracing::debug;

use crate::registry::connection_registry::{
    ConnectionEntry, LocalConnectionBindingId, PresenceState,
};
use crate::Stanza;

pub mod delivery;

/// Test controls, compiled for unit tests and the `test-utils` feature only.
#[cfg(any(test, feature = "test-utils"))]
pub mod test_support {
    use super::UserActor;
    use crate::registry::connection_registry::LocalConnectionBindingId;
    use std::sync::Arc;
    use tokio::sync::{oneshot, Notify};

    /// Maximum copied child-mutation outcomes retained by one actor fixture.
    pub const RESOURCE_MUTATION_OBSERVATION_CAPACITY: usize = 64;

    /// The child actor's observed decision, not a model prediction.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ResourceMutationDisposition {
        Inserted,
        Replaced,
        Removed,
        Retained,
        AlreadyAbsent,
        Rejected,
    }

    /// Copied identity evidence for one actual child mutation decision.
    /// Child messages alone do not prove publication, claim authority,
    /// custody, or teardown, so this is characterization only.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum ResourceMutationObservation {
        Register {
            previous: Option<LocalConnectionBindingId>,
            candidate: LocalConnectionBindingId,
            candidate_binding: Option<crate::ownership::BindingRef>,
            disposition: ResourceMutationDisposition,
        },
        RegisterIfOwnerOrAbsent {
            previous: Option<LocalConnectionBindingId>,
            candidate: LocalConnectionBindingId,
            candidate_binding: Option<crate::ownership::BindingRef>,
            owner: LocalConnectionBindingId,
            disposition: ResourceMutationDisposition,
        },
        ProvisionalMirror {
            previous: Option<LocalConnectionBindingId>,
            candidate: LocalConnectionBindingId,
            candidate_binding: Option<crate::ownership::BindingRef>,
            expected: Option<LocalConnectionBindingId>,
            disposition: ResourceMutationDisposition,
        },
        Remove {
            previous: Option<LocalConnectionBindingId>,
            owner: Option<LocalConnectionBindingId>,
            disposition: ResourceMutationDisposition,
        },
        ConflictClose {
            resource_count: usize,
        },
    }

    /// Bounded observation batch. Overflow means later decisions were not
    /// retained and must not be interpreted as absent mutations.
    #[derive(Debug, Clone, PartialEq, Eq, kameo::Reply)]
    pub struct ResourceMutationObservationBatch {
        pub observations: Vec<ResourceMutationObservation>,
        pub overflow_count: usize,
        /// Existing-slot replacement has no witnessed retirement receipts,
        /// so the typed model's Bind transition is unsupported for it.
        pub unsupported_direct_replacement_count: usize,
        /// This child decision journal does not contain endpoint publication,
        /// claim authorization, custody, or teardown receipts. The typed
        /// transition model is therefore deliberately not invoked here.
        pub model_comparison: ModelComparison,
        /// Stable for this fixture actor lifetime; synthetic only.
        pub synthetic_claim_identity: crate::ownership::UserClaimIdentity,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum ModelComparison {
        InconclusiveMissingExternalReceipts,
    }

    /// Read and clear the test-only child-mutation journal.
    pub struct TakeResourceMutationObservations;

    impl kameo::message::Message<TakeResourceMutationObservations> for UserActor {
        type Reply = ResourceMutationObservationBatch;

        async fn handle(
            &mut self,
            _msg: TakeResourceMutationObservations,
            _ctx: &mut kameo::message::Context<Self, Self::Reply>,
        ) -> Self::Reply {
            let unsupported_direct_replacement_count = self
                .transition_observations
                .iter()
                .filter(|observation| match observation {
                    ResourceMutationObservation::Register { disposition, .. }
                    | ResourceMutationObservation::RegisterIfOwnerOrAbsent {
                        disposition, ..
                    }
                    | ResourceMutationObservation::ProvisionalMirror { disposition, .. } => {
                        *disposition == ResourceMutationDisposition::Replaced
                    }
                    ResourceMutationObservation::Remove { .. }
                    | ResourceMutationObservation::ConflictClose { .. } => false,
                })
                .count();
            ResourceMutationObservationBatch {
                observations: std::mem::replace(
                    &mut self.transition_observations,
                    Vec::with_capacity(RESOURCE_MUTATION_OBSERVATION_CAPACITY),
                ),
                overflow_count: std::mem::take(&mut self.transition_observation_overflow),
                unsupported_direct_replacement_count,
                model_comparison: ModelComparison::InconclusiveMissingExternalReceipts,
                synthetic_claim_identity: self.synthetic_claim_identity.clone(),
            }
        }
    }

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
#[cfg(any(test, feature = "test-utils"))]
pub use implementation::ConfirmProvisionalConnection;
#[cfg(any(test, feature = "test-utils"))]
pub use implementation::ConfirmedProvisionalMirror;
pub use implementation::ConflictCloseAllResources;
pub use implementation::DrainPendingSubscriptions;
pub use implementation::GetAvailableResources;
pub use implementation::GetOtherResources;
pub use implementation::GetPresenceState;
pub use implementation::GetResources;
pub use implementation::GetRoutingResources;
pub use implementation::HealthCheck;
#[cfg(feature = "test-utils")]
pub use implementation::HoldUserActorForPublication;
pub use implementation::IsCarbonsEnabled;
pub use implementation::IsConnected;
pub use implementation::QueuePendingSubscription;
pub use implementation::RegisterConnection;
pub use implementation::RegisterConnectionIfOwnerOrAbsent;
#[cfg(any(test, feature = "test-utils"))]
pub use implementation::RegisterProvisionalConnectionIfExpected;
pub use implementation::ResourceCount;
pub use implementation::ResourceRoutingState;
pub use implementation::SetCarbonsEnabled;
pub use implementation::UnregisterConnection;
pub use implementation::UnregisterConnectionAndReportEmpty;
pub use implementation::UnregisterConnectionOutcome;
pub use implementation::UpdatePresence;
pub use implementation::UpdatePresenceState;
pub use implementation::UserActor;
