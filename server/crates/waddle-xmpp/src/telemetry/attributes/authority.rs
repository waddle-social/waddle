//! Attributes for admission, custody, fencing, mailbox and transport
//! progress metrics. Each is a closed enum; none can carry a JID, a stream
//! id or any other identifier.

use super::sealed;
use super::MetricAttribute;

/// `pump` — which supervised transport pump a sample describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoPump {
    /// The pump that reads frames from the peer.
    Reader,
    /// The pump that writes frames to the peer.
    Writer,
}

impl IoPump {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 2] = [Self::Reader, Self::Writer];
}

impl sealed::Sealed for IoPump {}
impl MetricAttribute for IoPump {
    fn key(&self) -> &'static str {
        "pump"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Reader => "reader",
            Self::Writer => "writer",
        }
    }
}

/// `cause` — why a supervised pump or its custodian ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IoExitCause {
    /// The peer closed the transport.
    PeerClosed,
    /// The owner cancelled it.
    Cancelled,
    /// The transport returned an error.
    IoError,
    /// A bounded wait expired.
    Deadline,
    /// The task panicked.
    Panicked,
    /// It was dropped without a recorded completion.
    Abandoned,
}

impl IoExitCause {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 6] = [
        Self::PeerClosed,
        Self::Cancelled,
        Self::IoError,
        Self::Deadline,
        Self::Panicked,
        Self::Abandoned,
    ];
}

impl sealed::Sealed for IoExitCause {}
impl MetricAttribute for IoExitCause {
    fn key(&self) -> &'static str {
        "cause"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::PeerClosed => "peer_closed",
            Self::Cancelled => "cancelled",
            Self::IoError => "io_error",
            Self::Deadline => "deadline",
            Self::Panicked => "panicked",
            Self::Abandoned => "abandoned",
        }
    }
}

/// `lane` — which mailbox lane of a connection actor a sample describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorLane {
    /// Lifecycle and control messages.
    Control,
    /// Stanza deliveries.
    Data,
}

impl ActorLane {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 2] = [Self::Control, Self::Data];
}

impl sealed::Sealed for ActorLane {}
impl MetricAttribute for ActorLane {
    fn key(&self) -> &'static str {
        "lane"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Data => "data",
        }
    }
}

/// `reason` — why a connection actor mailbox refused a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxRejection {
    /// The lane was at capacity.
    Full,
    /// The actor had stopped.
    Closed,
    /// The sender's wait expired.
    Deadline,
}

impl MailboxRejection {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 3] = [Self::Full, Self::Closed, Self::Deadline];
}

impl sealed::Sealed for MailboxRejection {}
impl MetricAttribute for MailboxRejection {
    fn key(&self) -> &'static str {
        "reason"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Closed => "closed",
            Self::Deadline => "deadline",
        }
    }
}

/// `event` — what a connection actor turn handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorTurnEvent {
    /// A frame read from the client.
    InboundFrame,
    /// A stanza addressed to the client.
    OutboundDelivery,
    /// A lifecycle or control message.
    Control,
    /// A timer the actor armed.
    Timer,
    /// The shutdown signal.
    Shutdown,
}

impl ActorTurnEvent {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 5] = [
        Self::InboundFrame,
        Self::OutboundDelivery,
        Self::Control,
        Self::Timer,
        Self::Shutdown,
    ];
}

impl sealed::Sealed for ActorTurnEvent {}
impl MetricAttribute for ActorTurnEvent {
    fn key(&self) -> &'static str {
        "event"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::InboundFrame => "inbound_frame",
            Self::OutboundDelivery => "outbound_delivery",
            Self::Control => "control",
            Self::Timer => "timer",
            Self::Shutdown => "shutdown",
        }
    }
}

/// `cause` — why a connection actor retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorRetirementCause {
    /// The client closed its stream.
    ClientClose,
    /// The transport failed.
    TransportError,
    /// A newer binding of the same full JID replaced it.
    Replaced,
    /// The server drained it.
    ServerShutdown,
    /// A policy violation closed the stream.
    Policy,
    /// The actor panicked.
    Panic,
    /// It was dropped without retiring.
    Abandoned,
    /// Retirement was forced at a deadline.
    Deadline,
}

impl ActorRetirementCause {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 8] = [
        Self::ClientClose,
        Self::TransportError,
        Self::Replaced,
        Self::ServerShutdown,
        Self::Policy,
        Self::Panic,
        Self::Abandoned,
        Self::Deadline,
    ];
}

impl sealed::Sealed for ActorRetirementCause {}
impl MetricAttribute for ActorRetirementCause {
    fn key(&self) -> &'static str {
        "cause"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::ClientClose => "client_close",
            Self::TransportError => "transport_error",
            Self::Replaced => "replaced",
            Self::ServerShutdown => "server_shutdown",
            Self::Policy => "policy",
            Self::Panic => "panic",
            Self::Abandoned => "abandoned",
            Self::Deadline => "deadline",
        }
    }
}

/// `stage` — the stage at which a restricted-runtime start refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedStartupStage {
    /// The operator handoff did not parse for this release and deployment.
    Handoff,
    /// The pinned read-only preflight did not complete.
    Preflight,
    /// The attested serving pool could not be attached.
    ServingPool,
    /// A store attachment or a later startup step failed.
    Startup,
}

impl RestrictedStartupStage {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 4] = [
        Self::Handoff,
        Self::Preflight,
        Self::ServingPool,
        Self::Startup,
    ];
}

impl sealed::Sealed for RestrictedStartupStage {}
impl MetricAttribute for RestrictedStartupStage {
    fn key(&self) -> &'static str {
        "stage"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Handoff => "handoff",
            Self::Preflight => "preflight",
            Self::ServingPool => "serving_pool",
            Self::Startup => "startup",
        }
    }
}

/// `store` — the store whose runtime attachment failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedStore {
    /// The room observation store.
    RoomObservation,
    /// The pending delivery store.
    PendingDelivery,
    /// The inbox store.
    Inbox,
    /// The stream management store.
    StreamManagement,
    /// The pubsub store.
    PubSub,
    /// The spaces store.
    Spaces,
    /// The push registrations store.
    PushRegistrations,
    /// The notification activity store.
    NotificationActivity,
    /// The notification outbox store.
    NotificationOutbox,
    /// The call teardown store.
    CallTeardown,
    /// The push service store.
    PushService,
    /// The room effect outbox store.
    RoomEffectOutbox,
    /// The muc destroy outbox store.
    MucDestroyOutbox,
    /// The mam store.
    Mam,
    /// The webhook deliveries store.
    WebhookDeliveries,
    /// The claims store.
    Claims,
    /// The keypair lease store.
    KeypairLease,
    /// The muc rooms store.
    MucRooms,
}

impl RestrictedStore {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 18] = [
        Self::RoomObservation,
        Self::PendingDelivery,
        Self::Inbox,
        Self::StreamManagement,
        Self::PubSub,
        Self::Spaces,
        Self::PushRegistrations,
        Self::NotificationActivity,
        Self::NotificationOutbox,
        Self::CallTeardown,
        Self::PushService,
        Self::RoomEffectOutbox,
        Self::MucDestroyOutbox,
        Self::Mam,
        Self::WebhookDeliveries,
        Self::Claims,
        Self::KeypairLease,
        Self::MucRooms,
    ];
}

impl sealed::Sealed for RestrictedStore {}
impl MetricAttribute for RestrictedStore {
    fn key(&self) -> &'static str {
        "store"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::RoomObservation => "room_observation",
            Self::PendingDelivery => "pending_delivery",
            Self::Inbox => "inbox",
            Self::StreamManagement => "stream_management",
            Self::PubSub => "pubsub",
            Self::Spaces => "spaces",
            Self::PushRegistrations => "push_registrations",
            Self::NotificationActivity => "notification_activity",
            Self::NotificationOutbox => "notification_outbox",
            Self::CallTeardown => "call_teardown",
            Self::PushService => "push_service",
            Self::RoomEffectOutbox => "room_effect_outbox",
            Self::MucDestroyOutbox => "muc_destroy_outbox",
            Self::Mam => "mam",
            Self::WebhookDeliveries => "webhook_deliveries",
            Self::Claims => "claims",
            Self::KeypairLease => "keypair_lease",
            Self::MucRooms => "muc_rooms",
        }
    }
}

/// `authority` — the authority an ingress transaction entered under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressEntryAuthorityKind {
    /// SQLite, or PostgreSQL without physical attestation.
    Unattested,
    /// The registered epoch guard on an attested pool.
    InstalledGuard,
    /// The candidate relation guard on an attested pool.
    RelationGuard,
}

impl IngressEntryAuthorityKind {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 3] = [Self::Unattested, Self::InstalledGuard, Self::RelationGuard];
}

impl sealed::Sealed for IngressEntryAuthorityKind {}
impl MetricAttribute for IngressEntryAuthorityKind {
    fn key(&self) -> &'static str {
        "authority"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Unattested => "unattested",
            Self::InstalledGuard => "installed_guard",
            Self::RelationGuard => "relation_guard",
        }
    }
}

/// `outcome` — how an ingress transaction entry ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngressEntryOutcome {
    /// The transaction was handed out.
    Entered,
    /// No transaction was acquired before the deadline.
    AcquisitionTimeout,
    /// The epoch preparation timed out.
    PreparationTimeout,
    /// The epoch preparation failed.
    PreparationFailed,
    /// The live protocol epoch is above the one this binary implements.
    EpochUnsupported,
    /// The entry could not prove its bounds or the epoch.
    Refused,
    /// The database failed before preparation.
    Failed,
}

impl IngressEntryOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 7] = [
        Self::Entered,
        Self::AcquisitionTimeout,
        Self::PreparationTimeout,
        Self::PreparationFailed,
        Self::EpochUnsupported,
        Self::Refused,
        Self::Failed,
    ];
}

impl sealed::Sealed for IngressEntryOutcome {}
impl MetricAttribute for IngressEntryOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Entered => "entered",
            Self::AcquisitionTimeout => "acquisition_timeout",
            Self::PreparationTimeout => "preparation_timeout",
            Self::PreparationFailed => "preparation_failed",
            Self::EpochUnsupported => "epoch_unsupported",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

/// `outcome` — how one leased room-destroy completion ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestroyCompletionOutcome {
    /// The completion was delivered and removed.
    Completed,
    /// A lapsed lease of another holder was taken over.
    LeaseTakenOver,
    /// The completion was returned to the queue.
    Retried,
    /// The completion could not be processed.
    Failed,
}

impl DestroyCompletionOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 4] = [
        Self::Completed,
        Self::LeaseTakenOver,
        Self::Retried,
        Self::Failed,
    ];
}

impl sealed::Sealed for DestroyCompletionOutcome {}
impl MetricAttribute for DestroyCompletionOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::LeaseTakenOver => "lease_taken_over",
            Self::Retried => "retried",
            Self::Failed => "failed",
        }
    }
}

/// `outcome` — how a keypair-slot lease operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeypairLeaseOutcome {
    /// A slot was leased.
    Acquired,
    /// No slot in the pool was free.
    Exhausted,
    /// A renewal found the slot stolen or lapsed.
    FencingLost,
    /// The slot was released.
    Released,
}

impl KeypairLeaseOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 4] = [
        Self::Acquired,
        Self::Exhausted,
        Self::FencingLost,
        Self::Released,
    ];
}

impl sealed::Sealed for KeypairLeaseOutcome {}
impl MetricAttribute for KeypairLeaseOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Acquired => "acquired",
            Self::Exhausted => "exhausted",
            Self::FencingLost => "fencing_lost",
            Self::Released => "released",
        }
    }
}

/// `outcome` — how one ordered relay delivery ended for its sender.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayOutcome {
    /// The peer acknowledged the delivery.
    Acked,
    /// The peer refused the delivery.
    Nacked,
    /// The outcome is unknown; the delivery may have committed.
    Uncertain,
    /// No reply arrived in time.
    TimedOut,
    /// The peer speaks another message version.
    IncompatiblePeer,
}

impl RelayOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 5] = [
        Self::Acked,
        Self::Nacked,
        Self::Uncertain,
        Self::TimedOut,
        Self::IncompatiblePeer,
    ];
}

impl sealed::Sealed for RelayOutcome {}
impl MetricAttribute for RelayOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Acked => "acked",
            Self::Nacked => "nacked",
            Self::Uncertain => "uncertain",
            Self::TimedOut => "timed_out",
            Self::IncompatiblePeer => "incompatible_peer",
        }
    }
}

/// `outcome` — how an XEP-0198 enable request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmEnableOutcome {
    /// Stream management was enabled with resumption.
    Enabled,
    /// Stream management was enabled without resumption.
    EnabledWithoutResume,
    /// The request was refused.
    Refused,
    /// Persisting the session failed.
    Failed,
}

impl SmEnableOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 4] = [
        Self::Enabled,
        Self::EnabledWithoutResume,
        Self::Refused,
        Self::Failed,
    ];
}

impl sealed::Sealed for SmEnableOutcome {}
impl MetricAttribute for SmEnableOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::EnabledWithoutResume => "enabled_without_resume",
            Self::Refused => "refused",
            Self::Failed => "failed",
        }
    }
}

/// `outcome` — how a dispatch to a room owned by another node ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteDispatchOutcome {
    /// The owner accepted the submission.
    Accepted,
    /// The owner rejected the submission.
    Rejected,
    /// The outcome is unknown; the owner may have accepted.
    Uncertain,
    /// The room's owner changed during the dispatch.
    OwnerMoved,
    /// No reply arrived in time.
    TimedOut,
}

impl RemoteDispatchOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 5] = [
        Self::Accepted,
        Self::Rejected,
        Self::Uncertain,
        Self::OwnerMoved,
        Self::TimedOut,
    ];
}

impl sealed::Sealed for RemoteDispatchOutcome {}
impl MetricAttribute for RemoteDispatchOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Rejected => "rejected",
            Self::Uncertain => "uncertain",
            Self::OwnerMoved => "owner_moved",
            Self::TimedOut => "timed_out",
        }
    }
}

/// `mutation` — which account-resource mutation a coordinator handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountMutationKind {
    Bind,
    PublishEndpoint,
    RegisterActor,
    UpdateSnapshot,
    Detach,
    SuccessfulResume,
    BeginRetirement,
    CompleteRetirement,
    ReplaceClaim,
    AdoptBinding,
    /// Recording a one-time presentation.
    MarkPresented,
    /// Repair of a retirement whose endpoint evidence is overdue.
    Repair,
}

impl AccountMutationKind {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 12] = [
        Self::Bind,
        Self::PublishEndpoint,
        Self::RegisterActor,
        Self::UpdateSnapshot,
        Self::Detach,
        Self::SuccessfulResume,
        Self::BeginRetirement,
        Self::CompleteRetirement,
        Self::ReplaceClaim,
        Self::AdoptBinding,
        Self::MarkPresented,
        Self::Repair,
    ];
}

impl sealed::Sealed for AccountMutationKind {}
impl MetricAttribute for AccountMutationKind {
    fn key(&self) -> &'static str {
        "mutation"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Bind => "bind",
            Self::PublishEndpoint => "publish_endpoint",
            Self::RegisterActor => "register_actor",
            Self::UpdateSnapshot => "update_snapshot",
            Self::Detach => "detach",
            Self::SuccessfulResume => "successful_resume",
            Self::BeginRetirement => "begin_retirement",
            Self::CompleteRetirement => "complete_retirement",
            Self::ReplaceClaim => "replace_claim",
            Self::AdoptBinding => "adopt_binding",
            Self::MarkPresented => "mark_presented",
            Self::Repair => "repair",
        }
    }
}

/// `outcome` — how an account coordinator operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountOperationOutcome {
    /// The operation committed now.
    Committed,
    /// The operation's recorded outcome was returned again.
    Replayed,
    /// The operation was refused by a fence or by the lifecycle.
    Rejected,
    /// The coordinator or its store could not decide the operation.
    Failed,
}

impl AccountOperationOutcome {
    /// Every value, for zero-registration and tests.
    pub const ALL: [Self; 4] = [
        Self::Committed,
        Self::Replayed,
        Self::Rejected,
        Self::Failed,
    ];
}

impl sealed::Sealed for AccountOperationOutcome {}
impl MetricAttribute for AccountOperationOutcome {
    fn key(&self) -> &'static str {
        "outcome"
    }

    fn value(&self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Replayed => "replayed",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    fn distinct<A: MetricAttribute>(values: &[A]) {
        let labels: HashSet<&str> = values.iter().map(MetricAttribute::value).collect();
        assert_eq!(labels.len(), values.len(), "duplicate attribute value");
        for label in labels {
            assert!(
                label
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_'),
                "{label}"
            );
        }
    }

    #[test]
    fn every_value_is_a_distinct_snake_case_label() {
        distinct(&IoPump::ALL);
        distinct(&IoExitCause::ALL);
        distinct(&ActorLane::ALL);
        distinct(&MailboxRejection::ALL);
        distinct(&ActorTurnEvent::ALL);
        distinct(&ActorRetirementCause::ALL);
        distinct(&RestrictedStartupStage::ALL);
        distinct(&RestrictedStore::ALL);
        distinct(&IngressEntryAuthorityKind::ALL);
        distinct(&IngressEntryOutcome::ALL);
        distinct(&DestroyCompletionOutcome::ALL);
        distinct(&KeypairLeaseOutcome::ALL);
        distinct(&RelayOutcome::ALL);
        distinct(&SmEnableOutcome::ALL);
        distinct(&RemoteDispatchOutcome::ALL);
        distinct(&AccountMutationKind::ALL);
        distinct(&AccountOperationOutcome::ALL);
    }
}
