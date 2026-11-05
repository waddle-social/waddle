//! Minimal public fixtures for this crate's XEP integration suites.
//!
//! The helpers are test-only by construction and use `expect` internally when
//! creating fixed local fixtures; they are never enabled in release images.

#[cfg(feature = "test-support")]
pub mod iq_forward;
#[cfg(feature = "test-support")]
pub mod postgres_proxy;
#[cfg(feature = "test-support")]
pub(crate) mod relation_guard_lab;
#[cfg(feature = "test-support")]
pub use relation_guard_lab::verify_disposable_relation_guard_chain;
#[cfg(all(feature = "test-support", feature = "clustering"))]
pub use relation_guard_lab::verify_recorded_room_archive;
#[cfg(feature = "test-support")]
mod ordered_claim;
#[cfg(feature = "test-support")]
mod shared_correctness;
#[cfg(feature = "test-support")]
pub use crate::pending_delivery::database::ordered_claim::{OrderedClaimError, OrderedClaimPhase};
#[cfg(feature = "test-support")]
pub use ordered_claim::{OrderedClaimAttemptLab, OrderedClaimLab, OrderedClaimObservationLab};
#[cfg(feature = "test-support")]
pub use shared_correctness::{
    probe_public_mam_acl_rollback, SharedCorrectnessFixture, SharedCorrectnessSubmitError,
    SharedPendingResolverFixture,
};
#[cfg(feature = "test-support")]
pub mod supervised_io;
#[cfg(feature = "test-support")]
pub mod actor_local_endpoint {
    pub use crate::server::routes::websocket::actor_local_endpoint::{
        ActorEndpointCleanupIndex, ActorLocalEndpointError, ActorLocalEndpointOwner,
        ActorLocalPublishError, ActorLocalWriteError, ActorPublicationProbe,
    };
    pub use crate::server::routes::websocket::{
        ConnectionActorDriverTestAdmissionError, ConnectionActorDriverTestGate,
        ConnectionActorDriverTestHandle, ConnectionActorDriverTestSnapshot,
        ConnectionActorDriverTestTicket, ConnectionActorDriverTestUnknownReason,
    };
}
#[cfg(feature = "test-support")]
pub mod connection_actor {
    //! Dark, test-only vertical proof for a connection actor that owns the
    //! real WebSocket connection state and its reserved physical pump pair.
    pub use crate::server::routes::websocket::connection_actor::{
        canonical_actor_gate, start_connection_actor, start_connection_actor_with_effect,
        start_connection_actor_with_hooks, AcknowledgementResult, CanonicalActorAdmission,
        CanonicalActorGate, CompleteHandledInbound, CompleteHandledInboundAtLifecycleGate,
        CompleteHandledInboundBeforeLifecycleGate, CompletePrebindAuth,
        CompleteValidatedPrebindAuth, ConfirmBindingInstall, ConnectionActor,
        ConnectionActorBindingAttempt, ConnectionActorController, ConnectionActorError,
        ConnectionActorGateBarrier, ConnectionActorOperationId, ConnectionActorSequence,
        ConnectionActorSnapshot, ConnectionActorStartBarrier, ConnectionActorStartError,
        ConnectionActorStartHooks, ConnectionWireFrame, CountableEffectDisposition,
        CountableInboundEffect, CountableStanzaEffect, InstallBinding, OutputAdmissionPause,
        PanicConnectionActor, PauseConnectionActor, PrebindAuthAttempt, PrebindAuthOutcome,
        PrebindAuthToken, PrebindInput, PrebindPhase, PrebindSaslInput, PrebindSnapshot,
        PrebindValidationAttempt, PrepareBinding, RequestAcknowledgement, ReserveHandledInbound,
        RetireBinding, SeedInboundCounterForTest, SnapshotConnection, SnapshotPrebind,
        StopConnectionActor,
    };
}

use crate::db::DatabasePool;
use crate::ingress::IngressAuthority;
use crate::notification_outbox::NotificationOutboxStore;
use crate::notification_settings_projection::NotificationSettingsProjectionStore;
use crate::pending_delivery::DatabasePendingDeliveryStorage;
use std::sync::Arc;

#[cfg(feature = "test-support")]
use crate::config::LineageConfig;
#[cfg(feature = "test-support")]
use crate::db::bootstrap::VerifiedGlobalAttachment;
#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPgPool;
#[cfg(feature = "test-support")]
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
#[cfg(feature = "test-support")]
use crate::db::Database;
#[cfg(feature = "tes