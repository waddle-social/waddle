//! Wire form of the account authority requests one node sends the node
//! that owns an account (`waddle.clustering.relay.account_binding.v1`).
//!
//! The in-process requests and answers of
//! [`crate::account_coordinator::AccountAuthorityRequest`] carry evidence
//! types whose constructors are the only way to mint them. They are not
//! serialized themselves: each crosses the relay as the DTO here and is
//! rebuilt on the other side through the same constructors, so a change to
//! an in-process type cannot silently change the wire. Identity values
//! (`BindingRef`, `ConnectionRef`, `UserClaimIdentity`, JIDs) keep their
//! own frozen serde form.
//!
//! A connection drives its binding with `Bind`, `PublishEndpoint`,
//! `RegisterActor`, `UpdateSnapshot`, `Detach` and `SuccessfulResume`, and
//! retires it with an unregister and a repair. The other slot mutations
//! are the coordinator's own and have no wire form.

use kameo::Reply;
use minidom::Element;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use waddle_xmpp::ownership::account_coordinator::{
    AccountResourcePresence, AccountResourceRouting, AccountResourcesError, BindingFenceError,
    Presentation,
};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindSuccessReceipt, AccountResourceBindingState, AccountResourceExecuteError,
    AccountResourceExecution, AccountResourceMutation, AccountResourceOperation,
    AccountResourceOperationId, AccountResourceOperationOutcome, AccountResourceRevision,
    AccountResourceSnapshot, AccountResourceTransitionError, ActorRegistrationReceipt,
    BindingEvidence, BindingRetirementDisposition, DirectedPresenceTeardownDisposition,
    EndpointPublicationReceipt, EndpointRetirementEvidence, PresentationFlushDisposition,
    SuccessfulResumeReceipt,
};
use waddle_xmpp::ownership::endpoint_transition::LogicalResourceGeneration;
use waddle_xmpp::ownership::joint_identity::{
    ResourceLifetime, SessionGeneration, SmBinding, SmSessionKey,
};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, UserClaimIdentity};
use waddle_xmpp_core::OccupancySessionGeneration;
use xmpp_parsers::presence::Show;

use crate::account_coordinator::{
    AccountAuthorityAnswer, AccountAuthorityRequest, AccountClaimError, AccountCoordinatorError,
    AccountView, BindingReply, ReleaseSessionRequest, RepairEvidence, RepairOutcome,
    SessionRelease, SlotStatus, UnregisterOutcome, UnregisterRequest,
};

/// The message id of an account authority request.
pub const ACCOUNT_BINDING_V1_ID: &str = "waddle.clustering.relay.account_binding.v1";

/// A value that has no form on the wire, or a wire value that does not
/// describe one. Nothing is sent, or nothing is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the account authority request has no wire form")]
pub struct AccountBindingWireError;

type Wire<T> = Result<T, AccountBindingWireError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingEvidenceWire {
    pub binding_ref: BindingRef,
    pub connection_ref: ConnectionRef,
}

impl From<&BindingEvidence> for BindingEvidenceWire {
    fn from(evidence: &BindingEvidence) -> Self {
        Self {
            binding_ref: evidence.binding_ref().clone(),
            connection_ref: evidence.connection_ref().clone(),
        }
    }
}

impl From<BindingEvidenceWire> for BindingEvidence {
    fn from(wire: BindingEvidenceWire) -> Self {
        BindingEvidence::observed(wire.binding_ref, wire.connection_ref)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActorRegistrationWire {
    pub claim_identity: UserClaimIdentity,
    pub source: BindingEvidenceWire,
}

impl From<&ActorRegistrationReceipt> for ActorRegistrationWire {
    fn from(receipt: &ActorRegistrationReceipt) -> Self {
        Self {
            claim_identity: receipt.claim_identity().clone(),
            source: receipt.source().into(),
        }
    }
}

impl From<ActorRegistrationWire> for ActorRegistrationReceipt {
    fn from(wire: ActorRegistrationWire) -> Self {
        ActorRegistrationReceipt::observed(wire.claim_identity, wire.source.into())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceLifetimeWire {
    pub logical: Uuid,
    pub occupancy: OccupancySessionGeneration,
}

impl From<ResourceLifetime> for ResourceLifetimeWire {
    fn from(lifetime: ResourceLifetime) -> Self {
        Self {
            logical: lifetime.logical.as_uuid(),
            occupancy: lifetime.occupancy,
        }
    }
}

impl From<ResourceLifetimeWire> for ResourceLifetime {
    fn from(wire: ResourceLifetimeWire) -> Self {
        ResourceLifetime {
            logical: LogicalResourceGeneration::from_uuid(wire.logical),
            occupancy: wire.occupancy,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SmBindingWire {
    pub session: Uuid,
    pub generation: u64,
}

impl From<SmBinding> for SmBindingWire {
    fn from(binding: SmBinding) -> Self {
        Self {
            session: binding.session.as_uuid(),
            generation: binding.generation.value(),
        }
    }
}

impl TryFrom<SmBindingWire> for SmBinding {
    type Error = AccountBindingWireError;

    fn try_from(wire: SmBindingWire) -> Wire<Self> {
        Ok(SmBinding {
            session: SmSessionKey::from_uuid(wire.session),
            generation: SessionGeneration::new(wire.generation).ok_or(AccountBindingWireError)?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShowWire {
    Away,
    Chat,
    Dnd,
    Xa,
}

impl From<&Show> for ShowWire {
    fn from(show: &Show) -> Self {
        match show {
            Show::Away => Self::Away,
            Show::Chat => Self::Chat,
            Show::Dnd => Self::Dnd,
            Show::Xa => Self::Xa,
        }
    }
}

impl From<ShowWire> for Show {
    fn from(wire: ShowWire) -> Self {
        match wire {
            ShowWire::Away => Show::Away,
            ShowWire::Chat => Show::Chat,
            ShowWire::Dnd => Show::Dnd,
            ShowWire::Xa => Show::Xa,
        }
    }
}

/// Presence payloads travel as serialized elements, each parsed back on
/// the other side.
fn encode_payloads(payloads: &[Element]) -> Vec<String> {
    payloads.iter().map(String::from).collect()
}

fn decode_payloads(payloads: Vec<String>) -> Wire<Vec<Element>> {
    payloads
        .into_iter()
        .map(|payload| {
            payload
                .parse::<Element>()
                .map_err(|_| AccountBindingWireError)
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotWire {
    pub presence_available: bool,
    pub presence_show: Option<ShowWire>,
    pub presence_status: Option<String>,
    pub presence_priority: i8,
    pub presence_payloads: Vec<String>,
    pub carbons_enabled: bool,
    pub roster_interested: bool,
    pub blocklist_interested: bool,
    pub stream_management_ref: Option<SmBindingWire>,
}

impl From<&AccountResourceSnapshot> for SnapshotWire {
    fn from(snapshot: &AccountResourceSnapshot) -> Self {
        Self {
            presence_available: snapshot.presence_available,
            presence_show: snapshot.presence_show.as_ref().map(ShowWire::from),
            presence_status: snapshot.presence_status.clone(),
            presence_priority: snapshot.presence_priority,
            presence_payloads: encode_payloads(&snapshot.presence_payloads),
            carbons_enabled: snapshot.carbons_enabled,
            roster_interested: snapshot.roster_interested,
            blocklist_interested: snapshot.blocklist_interested,
            stream_management_ref: snapshot.stream_management_ref.map(SmBindingWire::from),
        }
    }
}

impl TryFrom<SnapshotWire> for AccountResourceSnapshot {
    type Error = AccountBindingWireError;

    fn try_from(wire: SnapshotWire) -> Wire<Self> {
        Ok(AccountResourceSnapshot {
            presence_available: wire.presence_available,
            presence_show: wire.presence_show.map(Show::from),
            presence_status: wire.presence_status,
            presence_priority: wire.presence_priority,
            presence_payloads: decode_payloads(wire.presence_payloads)?,
            carbons_enabled: wire.carbons_enabled,
            roster_interested: wire.roster_interested,
            blocklist_interested: wire.blocklist_interested,
            stream_management_ref: wire
                .stream_management_ref
                .map(SmBinding::try_from)
                .transpose()?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CustodyWire {
    NoAcceptedWork,
    AcceptedWorkHasRecoveryOwner,
    Unknown,
}

impl From<BindingRetirementDisposition> for CustodyWire {
    fn from(disposition: BindingRetirementDisposition) -> Self {
        match disposition {
            BindingRetirementDisposition::NoAcceptedWork => Self::NoAcceptedWork,
            BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner => {
                Self::AcceptedWorkHasRecoveryOwner
            }
            BindingRetirementDisposition::Unknown => Self::Unknown,
        }
    }
}

impl From<CustodyWire> for BindingRetirementDisposition {
    fn from(wire: CustodyWire) -> Self {
        match wire {
            CustodyWire::NoAcceptedWork => Self::NoAcceptedWork,
            CustodyWire::AcceptedWorkHasRecoveryOwner => Self::AcceptedWorkHasRecoveryOwner,
            CustodyWire::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DirectedPresenceWire {
    NoDirectedRecipients,
    RecipientsRetractedAndUnavailableDelivered,
    RecipientsRetractedAndUnavailableRecoveryOwned,
    Unknown,
}

impl From<DirectedPresenceTeardownDisposition> for DirectedPresenceWire {
    fn from(disposition: DirectedPresenceTeardownDisposition) -> Self {
        use DirectedPresenceTeardownDisposition as D;
        match disposition {
            D::NoDirectedRecipients => Self::NoDirectedRecipients,
            D::RecipientsRetractedAndUnavailableDelivered => {
                Self::RecipientsRetractedAndUnavailableDelivered
            }
            D::RecipientsRetractedAndUnavailableRecoveryOwned => {
                Self::RecipientsRetractedAndUnavailableRecoveryOwned
            }
            D::Unknown => Self::Unknown,
        }
    }
}

impl From<DirectedPresenceWire> for DirectedPresenceTeardownDisposition {
    fn from(wire: DirectedPresenceWire) -> Self {
        match wire {
            DirectedPresenceWire::NoDirectedRecipients => Self::NoDirectedRecipients,
            DirectedPresenceWire::RecipientsRetractedAndUnavailableDelivered => {
                Self::RecipientsRetractedAndUnavailableDelivered
            }
            DirectedPresenceWire::RecipientsRetractedAndUnavailableRecoveryOwned => {
                Self::RecipientsRetractedAndUnavailableRecoveryOwned
            }
            DirectedPresenceWire::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresentationFlushWire {
    NoPendingPresentation,
    Delivered,
    RecoveryOwner,
    Unknown,
}

impl From<PresentationFlushDisposition> for PresentationFlushWire {
    fn from(disposition: PresentationFlushDisposition) -> Self {
        match disposition {
            PresentationFlushDisposition::NoPendingPresentation => Self::NoPendingPresentation,
            PresentationFlushDisposition::Delivered => Self::Delivered,
            PresentationFlushDisposition::RecoveryOwner => Self::RecoveryOwner,
            PresentationFlushDisposition::Unknown => Self::Unknown,
        }
    }
}

impl From<PresentationFlushWire> for PresentationFlushDisposition {
    fn from(wire: PresentationFlushWire) -> Self {
        match wire {
            PresentationFlushWire::NoPendingPresentation => Self::NoPendingPresentation,
            PresentationFlushWire::Delivered => Self::Delivered,
            PresentationFlushWire::RecoveryOwner => Self::RecoveryOwner,
            PresentationFlushWire::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndpointRetirementWire {
    NeverPublished {
        source: BindingEvidenceWire,
    },
    Unpublished {
        publication: BindingEvidenceWire,
        removed: BindingEvidenceWire,
    },
}

impl From<&EndpointRetirementEvidence> for EndpointRetirementWire {
    fn from(evidence: &EndpointRetirementEvidence) -> Self {
        match evidence {
            EndpointRetirementEvidence::NeverPublished { source } => Self::NeverPublished {
                source: source.into(),
            },
            EndpointRetirementEvidence::Unpublished {
                publication,
                removed,
            } => Self::Unpublished {
                publication: publication.source().into(),
                removed: removed.into(),
            },
        }
    }
}

impl From<EndpointRetirementWire> for EndpointRetirementEvidence {
    fn from(wire: EndpointRetirementWire) -> Self {
        match wire {
            EndpointRetirementWire::NeverPublished { source } => Self::NeverPublished {
                source: source.into(),
            },
            EndpointRetirementWire::Unpublished {
                publication,
                removed,
            } => Self::Unpublished {
                publication: EndpointPublicationReceipt::observed(publication.into()),
                removed: removed.into(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeReceiptWire {
    pub previous: BindingEvidenceWire,
    pub resumed: BindingEvidenceWire,
    pub previous_sm: SmBindingWire,
    pub resumed_sm: SmBindingWire,
    pub lifetime: ResourceLifetimeWire,
    pub custody: CustodyWire,
}

impl From<&SuccessfulResumeReceipt> for ResumeReceiptWire {
    fn from(receipt: &SuccessfulResumeReceipt) -> Self {
        Self {
            previous: receipt.previous().into(),
            resumed: receipt.resumed().into(),
            previous_sm: receipt.previous_sm().into(),
            resumed_sm: receipt.resumed_sm().into(),
            lifetime: receipt.lifetime().into(),
            custody: receipt.custody_disposition().into(),
        }
    }
}

impl TryFrom<ResumeReceiptWire> for SuccessfulResumeReceipt {
    type Error = AccountBindingWireError;

    fn try_from(wire: ResumeReceiptWire) -> Wire<Self> {
        Ok(SuccessfulResumeReceipt::observed(
            wire.previous.into(),
            wire.resumed.into(),
            wire.previous_sm.try_into()?,
            wire.resumed_sm.try_into()?,
            wire.lifetime.into(),
            wire.custody.into(),
        ))
    }
}

/// The slot mutations a connection sends for its own binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MutationWire {
    Bind {
        claim_identity: UserClaimIdentity,
        expected_revision: u64,
        binding_ref: BindingRef,
        lifetime: ResourceLifetimeWire,
    },
    PublishEndpoint {
        claim_identity: UserClaimIdentity,
        expected_revision: u64,
        binding_ref: BindingRef,
        publication: BindingEvidenceWire,
    },
    RegisterActor {
        claim_identity: UserClaimIdentity,
        expected_revision: u64,
        binding_ref: BindingRef,
        registration: ActorRegistrationWire,
    },
    UpdateSnapshot {
        claim_identity: UserClaimIdentity,
        expected_revision: u64,
        binding_ref: BindingRef,
        snapshot: SnapshotWire,
    },
    Detach {
        claim_identity: UserClaimIdentity,
        expected_revision: u64,
        binding_ref: BindingRef,
        endpoint: EndpointRetirementWire,
        custody: CustodyWire,
    },
    SuccessfulResume {
        claim_identity: UserClaimIdentity,
        expected_revision: u64,
        previous_binding_ref: BindingRef,
        binding_ref: BindingRef,
        receipt: ResumeReceiptWire,
    },
}

impl TryFrom<&AccountResourceMutation> for MutationWire {
    type Error = AccountBindingWireError;

    fn try_from(mutation: &AccountResourceMutation) -> Wire<Self> {
        Ok(match mutation {
            AccountResourceMutation::Bind {
                claim_identity,
                expected_revision,
                binding_ref,
                lifetime,
            } => Self::Bind {
                claim_identity: claim_identity.clone(),
                expected_revision: expected_revision.value(),
                binding_ref: binding_ref.clone(),
                lifetime: (*lifetime).into(),
            },
            AccountResourceMutation::PublishEndpoint {
                claim_identity,
                expected_revision,
                binding_ref,
                publication,
            } => Self::PublishEndpoint {
                claim_identity: claim_identity.clone(),
                expected_revision: expected_revision.value(),
                binding_ref: binding_ref.clone(),
                publication: publication.source().into(),
            },
            AccountResourceMutation::RegisterActor {
                claim_identity,
                expected_revision,
                binding_ref,
                registration,
            } => Self::RegisterActor {
                claim_identity: claim_identity.clone(),
                expected_revision: expected_revision.value(),
                binding_ref: binding_ref.clone(),
                registration: registration.into(),
            },
            AccountResourceMutation::UpdateSnapshot {
                claim_identity,
                expected_revision,
                binding_ref,
                snapshot,
            } => Self::UpdateSnapshot {
                claim_identity: claim_identity.clone(),
                expected_revision: expected_revision.value(),
                binding_ref: binding_ref.clone(),
                snapshot: snapshot.into(),
            },
            AccountResourceMutation::Detach {
                claim_identity,
                expected_revision,
                binding_ref,
                endpoint,
                custody_disposition,
            } => Self::Detach {
                claim_identity: claim_identity.clone(),
                expected_revision: expected_revision.value(),
                binding_ref: binding_ref.clone(),
                endpoint: endpoint.into(),
                custody: (*custody_disposition).into(),
            },
            AccountResourceMutation::SuccessfulResume {
                claim_identity,
                expected_revision,
                previous_binding_ref,
                binding_ref,
                receipt,
            } => Self::SuccessfulResume {
                claim_identity: claim_identity.clone(),
                expected_revision: expected_revision.value(),
                previous_binding_ref: previous_binding_ref.clone(),
                binding_ref: binding_ref.clone(),
                receipt: receipt.into(),
            },
            AccountResourceMutation::BeginRetirement { .. }
            | AccountResourceMutation::CompleteRetirement { .. }
            | AccountResourceMutation::ReplaceClaim { .. }
            | AccountResourceMutation::AdoptBinding { .. } => return Err(AccountBindingWireError),
        })
    }
}

impl TryFrom<MutationWire> for AccountResourceMutation {
    type Error = AccountBindingWireError;

    fn try_from(wire: MutationWire) -> Wire<Self> {
        Ok(match wire {
            MutationWire::Bind {
                claim_identity,
                expected_revision,
                binding_ref,
                lifetime,
            } => Self::Bind {
                claim_identity,
                expected_revision: AccountResourceRevision::new(expected_revision),
                binding_ref,
                lifetime: lifetime.into(),
            },
            MutationWire::PublishEndpoint {
                claim_identity,
                expected_revision,
                binding_ref,
                publication,
            } => Self::PublishEndpoint {
                claim_identity,
                expected_revision: AccountResourceRevision::new(expected_revision),
                binding_ref,
                publication: EndpointPublicationReceipt::observed(publication.into()),
            },
            MutationWire::RegisterActor {
                claim_identity,
                expected_revision,
                binding_ref,
                registration,
            } => Self::RegisterActor {
                claim_identity,
                expected_revision: AccountResourceRevision::new(expected_revision),
                binding_ref,
                registration: registration.into(),
            },
            MutationWire::UpdateSnapshot {
                claim_identity,
                expected_revision,
                binding_ref,
                snapshot,
            } => Self::UpdateSnapshot {
                claim_identity,
                expected_revision: AccountResourceRevision::new(expected_revision),
                binding_ref,
                snapshot: snapshot.try_into()?,
            },
            MutationWire::Detach {
                claim_identity,
                expected_revision,
                binding_ref,
                endpoint,
                custody,
            } => Self::Detach {
                claim_identity,
                expected_revision: AccountResourceRevision::new(expected_revision),
                binding_ref,
                endpoint: endpoint.into(),
                custody_disposition: custody.into(),
            },
            MutationWire::SuccessfulResume {
                claim_identity,
                expected_revision,
                previous_binding_ref,
                binding_ref,
                receipt,
            } => Self::SuccessfulResume {
                claim_identity,
                expected_revision: AccountResourceRevision::new(expected_revision),
                previous_binding_ref,
                binding_ref,
                receipt: receipt.try_into()?,
            },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresentationWire {
    OfflineMessages,
    PendingSubscriptions,
}

impl From<Presentation> for PresentationWire {
    fn from(presentation: Presentation) -> Self {
        match presentation {
            Presentation::OfflineMessages => Self::OfflineMessages,
            Presentation::PendingSubscriptions => Self::PendingSubscriptions,
        }
    }
}

impl From<PresentationWire> for Presentation {
    fn from(wire: PresentationWire) -> Self {
        match wire {
            PresentationWire::OfflineMessages => Self::OfflineMessages,
            PresentationWire::PendingSubscriptions => Self::PendingSubscriptions,
        }
    }
}

/// One account authority request on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityRequestWire {
    SlotStatus {
        full_jid: jid::FullJid,
    },
    Execute {
        full_jid: jid::FullJid,
        operation: Uuid,
        mutation: Box<MutationWire>,
    },
    BindSuccess {
        full_jid: jid::FullJid,
        binding_ref: BindingRef,
    },
    MarkPresented {
        full_jid: jid::FullJid,
        binding_ref: BindingRef,
        presentation: PresentationWire,
    },
    Unregister {
        full_jid: jid::FullJid,
        operation: Uuid,
        binding_ref: BindingRef,
        custody: CustodyWire,
    },
    Repair {
        full_jid: jid::FullJid,
        binding_ref: BindingRef,
        operation: Uuid,
        directed_presence: DirectedPresenceWire,
        presentation_flush: PresentationFlushWire,
    },
    ReleaseSession {
        full_jid: jid::FullJid,
        begin: Uuid,
        complete: Uuid,
        session: Uuid,
        custody: CustodyWire,
        directed_presence: DirectedPresenceWire,
        presentation_flush: PresentationFlushWire,
    },
    ExpiredBindings {
        account: jid::BareJid,
    },
    View {
        account: jid::BareJid,
    },
}

impl TryFrom<&AccountAuthorityRequest> for AuthorityRequestWire {
    type Error = AccountBindingWireError;

    fn try_from(request: &AccountAuthorityRequest) -> Wire<Self> {
        Ok(match request {
            AccountAuthorityRequest::SlotStatus { full_jid } => Self::SlotStatus {
                full_jid: full_jid.clone(),
            },
            AccountAuthorityRequest::Execute {
                full_jid,
                operation,
            } => Self::Execute {
                full_jid: full_jid.clone(),
                operation: operation.id.as_uuid(),
                mutation: Box::new((&operation.mutation).try_into()?),
            },
            AccountAuthorityRequest::BindSuccess {
                full_jid,
                binding_ref,
            } => Self::BindSuccess {
                full_jid: full_jid.clone(),
                binding_ref: binding_ref.clone(),
            },
            AccountAuthorityRequest::MarkPresented {
                full_jid,
                binding_ref,
                presentation,
            } => Self::MarkPresented {
                full_jid: full_jid.clone(),
                binding_ref: binding_ref.clone(),
                presentation: (*presentation).into(),
            },
            AccountAuthorityRequest::Unregister { full_jid, request } => Self::Unregister {
                full_jid: full_jid.clone(),
                operation: request.operation.as_uuid(),
                binding_ref: request.binding_ref.clone(),
                custody: request.custody_disposition.into(),
            },
            AccountAuthorityRequest::Repair {
                full_jid,
                binding_ref,
                evidence,
            } => Self::Repair {
                full_jid: full_jid.clone(),
                binding_ref: binding_ref.clone(),
                operation: evidence.operation.as_uuid(),
                directed_presence: evidence.directed_presence.into(),
                presentation_flush: evidence.presentation_flush.into(),
            },
            AccountAuthorityRequest::ReleaseSession { full_jid, request } => Self::ReleaseSession {
                full_jid: full_jid.clone(),
                begin: request.begin.as_uuid(),
                complete: request.complete.as_uuid(),
                session: request.session.as_uuid(),
                custody: request.custody_disposition.into(),
                directed_presence: request.directed_presence.into(),
                presentation_flush: request.presentation_flush.into(),
            },
            AccountAuthorityRequest::ExpiredBindings { account } => Self::ExpiredBindings {
                account: account.clone(),
            },
            AccountAuthorityRequest::View { account } => Self::View {
                account: account.clone(),
            },
        })
    }
}

impl TryFrom<AuthorityRequestWire> for AccountAuthorityRequest {
    type Error = AccountBindingWireError;

    fn try_from(wire: AuthorityRequestWire) -> Wire<Self> {
        Ok(match wire {
            AuthorityRequestWire::SlotStatus { full_jid } => Self::SlotStatus { full_jid },
            AuthorityRequestWire::Execute {
                full_jid,
                operation,
                mutation,
            } => Self::Execute {
                full_jid,
                operation: Box::new(AccountResourceOperation::new(
                    AccountResourceOperationId::from_uuid(operation),
                    (*mutation).try_into()?,
                )),
            },
            AuthorityRequestWire::BindSuccess {
                full_jid,
                binding_ref,
            } => Self::BindSuccess {
                full_jid,
                binding_ref,
            },
            AuthorityRequestWire::MarkPresented {
                full_jid,
                binding_ref,
                presentation,
            } => Self::MarkPresented {
                full_jid,
                binding_ref,
                presentation: presentation.into(),
            },
            AuthorityRequestWire::Unregister {
                full_jid,
                operation,
                binding_ref,
                custody,
            } => Self::Unregister {
                full_jid,
                request: UnregisterRequest {
                    operation: AccountResourceOperationId::from_uuid(operation),
                    binding_ref,
                    custody_disposition: custody.into(),
                },
            },
            AuthorityRequestWire::Repair {
                full_jid,
                binding_ref,
                operation,
                directed_presence,
                presentation_flush,
            } => Self::Repair {
                full_jid,
                binding_ref,
                evidence: RepairEvidence {
                    operation: AccountResourceOperationId::from_uuid(operation),
                    directed_presence: directed_presence.into(),
                    presentation_flush: presentation_flush.into(),
                },
            },
            AuthorityRequestWire::ReleaseSession {
                full_jid,
                begin,
                complete,
                session,
                custody,
                directed_presence,
                presentation_flush,
            } => Self::ReleaseSession {
                full_jid,
                request: ReleaseSessionRequest {
                    begin: AccountResourceOperationId::from_uuid(begin),
                    complete: AccountResourceOperationId::from_uuid(complete),
                    session: SmSessionKey::from_uuid(session),
                    custody_disposition: custody.into(),
                    directed_presence: directed_presence.into(),
                    presentation_flush: presentation_flush.into(),
                },
            },
            AuthorityRequestWire::ExpiredBindings { account } => Self::ExpiredBindings { account },
            AuthorityRequestWire::View { account } => Self::View { account },
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingStateWire {
    Prepared,
    Published,
    Active,
    Detached,
    Retiring,
    Retired,
}

impl From<AccountResourceBindingState> for BindingStateWire {
    fn from(state: AccountResourceBindingState) -> Self {
        use AccountResourceBindingState as S;
        match state {
            S::Prepared => Self::Prepared,
            S::Published => Self::Published,
            S::Active => Self::Active,
            S::Detached => Self::Detached,
            S::Retiring => Self::Retiring,
            S::Retired => Self::Retired,
        }
    }
}

impl From<BindingStateWire> for AccountResourceBindingState {
    fn from(wire: BindingStateWire) -> Self {
        match wire {
            BindingStateWire::Prepared => Self::Prepared,
            BindingStateWire::Published => Self::Published,
            BindingStateWire::Active => Self::Active,
            BindingStateWire::Detached => Self::Detached,
            BindingStateWire::Retiring => Self::Retiring,
            BindingStateWire::Retired => Self::Retired,
        }
    }
}

/// [`AccountResourceTransitionError`] on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionErrorWire {
    AccountMismatch,
    ClaimMismatch,
    ClaimIdentityReused,
    RevisionConflict,
    RevisionExhausted,
    AlreadyBound,
    NotBound,
    BindingMismatch,
    BindingOwnerMismatch,
    BindingRefReused,
    ResourceLifetimeReused,
    InstallGenerationExhausted,
    LifecycleConflict,
    EndpointPublicationMismatch,
    ActorRegistrationMismatch,
    StreamManagementReferenceChanged,
    ResumeRequiresStreamManagement,
    ResumeBindingMismatch,
    ResumeConnectionMismatch,
    RetirementCustodyUnknown,
    RetirementEvidenceMismatch,
    RetirementCustodyMismatch,
}

impl From<AccountResourceTransitionError> for TransitionErrorWire {
    fn from(error: AccountResourceTransitionError) -> Self {
        use AccountResourceTransitionError as E;
        match error {
            E::AccountMismatch => Self::AccountMismatch,
            E::ClaimMismatch => Self::ClaimMismatch,
            E::ClaimIdentityReused => Self::ClaimIdentityReused,
            E::RevisionConflict => Self::RevisionConflict,
            E::RevisionExhausted => Self::RevisionExhausted,
            E::AlreadyBound => Self::AlreadyBound,
            E::NotBound => Self::NotBound,
            E::BindingMismatch => Self::BindingMismatch,
            E::BindingOwnerMismatch => Self::BindingOwnerMismatch,
            E::BindingRefReused => Self::BindingRefReused,
            E::ResourceLifetimeReused => Self::ResourceLifetimeReused,
            E::InstallGenerationExhausted => Self::InstallGenerationExhausted,
            E::LifecycleConflict => Self::LifecycleConflict,
            E::EndpointPublicationMismatch => Self::EndpointPublicationMismatch,
            E::ActorRegistrationMismatch => Self::ActorRegistrationMismatch,
            E::StreamManagementReferenceChanged => Self::StreamManagementReferenceChanged,
            E::ResumeRequiresStreamManagement => Self::ResumeRequiresStreamManagement,
            E::ResumeBindingMismatch => Self::ResumeBindingMismatch,
            E::ResumeConnectionMismatch => Self::ResumeConnectionMismatch,
            E::RetirementCustodyUnknown => Self::RetirementCustodyUnknown,
            E::RetirementEvidenceMismatch => Self::RetirementEvidenceMismatch,
            E::RetirementCustodyMismatch => Self::RetirementCustodyMismatch,
        }
    }
}

impl From<TransitionErrorWire> for AccountResourceTransitionError {
    fn from(wire: TransitionErrorWire) -> Self {
        use TransitionErrorWire as W;
        match wire {
            W::AccountMismatch => Self::AccountMismatch,
            W::ClaimMismatch => Self::ClaimMismatch,
            W::ClaimIdentityReused => Self::ClaimIdentityReused,
            W::RevisionConflict => Self::RevisionConflict,
            W::RevisionExhausted => Self::RevisionExhausted,
            W::AlreadyBound => Self::AlreadyBound,
            W::NotBound => Self::NotBound,
            W::BindingMismatch => Self::BindingMismatch,
            W::BindingOwnerMismatch => Self::BindingOwnerMismatch,
            W::BindingRefReused => Self::BindingRefReused,
            W::ResourceLifetimeReused => Self::ResourceLifetimeReused,
            W::InstallGenerationExhausted => Self::InstallGenerationExhausted,
            W::LifecycleConflict => Self::LifecycleConflict,
            W::EndpointPublicationMismatch => Self::EndpointPublicationMismatch,
            W::ActorRegistrationMismatch => Self::ActorRegistrationMismatch,
            W::StreamManagementReferenceChanged => Self::StreamManagementReferenceChanged,
            W::ResumeRequiresStreamManagement => Self::ResumeRequiresStreamManagement,
            W::ResumeBindingMismatch => Self::ResumeBindingMismatch,
            W::ResumeConnectionMismatch => Self::ResumeConnectionMismatch,
            W::RetirementCustodyUnknown => Self::RetirementCustodyUnknown,
            W::RetirementEvidenceMismatch => Self::RetirementEvidenceMismatch,
            W::RetirementCustodyMismatch => Self::RetirementCustodyMismatch,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationOutcomeWire {
    Committed { revision: u64 },
    Rejected(TransitionErrorWire),
}

impl From<AccountResourceOperationOutcome> for OperationOutcomeWire {
    fn from(outcome: AccountResourceOperationOutcome) -> Self {
        match outcome {
            AccountResourceOperationOutcome::Committed { revision } => Self::Committed {
                revision: revision.value(),
            },
            AccountResourceOperationOutcome::Rejected(error) => Self::Rejected(error.into()),
        }
    }
}

impl From<OperationOutcomeWire> for AccountResourceOperationOutcome {
    fn from(wire: OperationOutcomeWire) -> Self {
        match wire {
            OperationOutcomeWire::Committed { revision } => Self::Committed {
                revision: AccountResourceRevision::new(revision),
            },
            OperationOutcomeWire::Rejected(error) => Self::Rejected(error.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BindingReplyWire {
    Applied(OperationOutcomeWire),
    Replayed(OperationOutcomeWire),
    ReplacementPending {
        displaced: BindingRef,
        revision: u64,
    },
}

impl From<&BindingReply> for BindingReplyWire {
    fn from(reply: &BindingReply) -> Self {
        match reply {
            BindingReply::Executed(AccountResourceExecution::Applied(outcome)) => {
                Self::Applied((*outcome).into())
            }
            BindingReply::Executed(AccountResourceExecution::Replayed(outcome)) => {
                Self::Replayed((*outcome).into())
            }
            BindingReply::ReplacementPending {
                displaced,
                revision,
            } => Self::ReplacementPending {
                displaced: displaced.clone(),
                revision: revision.value(),
            },
        }
    }
}

impl From<BindingReplyWire> for BindingReply {
    fn from(wire: BindingReplyWire) -> Self {
        match wire {
            BindingReplyWire::Applied(outcome) => {
                Self::Executed(AccountResourceExecution::Applied(outcome.into()))
            }
            BindingReplyWire::Replayed(outcome) => {
                Self::Executed(AccountResourceExecution::Replayed(outcome.into()))
            }
            BindingReplyWire::ReplacementPending {
                displaced,
                revision,
            } => Self::ReplacementPending {
                displaced,
                revision: AccountResourceRevision::new(revision),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindSuccessReceiptWire {
    pub claim_identity: UserClaimIdentity,
    pub source: BindingEvidenceWire,
    pub revision: u64,
    pub publication: BindingEvidenceWire,
    pub registration: ActorRegistrationWire,
}

impl From<&AccountResourceBindSuccessReceipt> for BindSuccessReceiptWire {
    fn from(receipt: &AccountResourceBindSuccessReceipt) -> Self {
        Self {
            claim_identity: receipt.claim_identity().clone(),
            source: receipt.source().into(),
            revision: receipt.revision().value(),
            publication: receipt.publication().source().into(),
            registration: receipt.registration().into(),
        }
    }
}

impl TryFrom<BindSuccessReceiptWire> for AccountResourceBindSuccessReceipt {
    type Error = AccountBindingWireError;

    fn try_from(wire: BindSuccessReceiptWire) -> Wire<Self> {
        AccountResourceBindSuccessReceipt::attested(
            wire.claim_identity,
            wire.source.into(),
            AccountResourceRevision::new(wire.revision),
            EndpointPublicationReceipt::observed(wire.publication.into()),
            wire.registration.into(),
        )
        .ok_or(AccountBindingWireError)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FenceErrorWire {
    UnknownResource,
    NotBound,
    Superseded,
}

impl From<BindingFenceError> for FenceErrorWire {
    fn from(error: BindingFenceError) -> Self {
        match error {
            BindingFenceError::UnknownResource => Self::UnknownResource,
            BindingFenceError::NotBound => Self::NotBound,
            BindingFenceError::Superseded => Self::Superseded,
        }
    }
}

impl From<FenceErrorWire> for BindingFenceError {
    fn from(wire: FenceErrorWire) -> Self {
        match wire {
            FenceErrorWire::UnknownResource => Self::UnknownResource,
            FenceErrorWire::NotBound => Self::NotBound,
            FenceErrorWire::Superseded => Self::Superseded,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnregisterOutcomeWire {
    Retiring { revision: u64 },
    Stale(FenceErrorWire),
    Rejected(TransitionErrorWire),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RepairOutcomeWire {
    Completed { revision: u64 },
    HeldByNewerBinding(BindingRef),
    NotRetiring(BindingStateWire),
    Rejected(TransitionErrorWire),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionReleaseWire {
    Released { revision: u64 },
    NotReferenced,
    HeldByConnection(BindingRef, BindingStateWire),
    Rejected(TransitionErrorWire),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingWire {
    pub jid: jid::FullJid,
    pub available: bool,
    pub priority: i8,
    pub carbons_enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PresenceWire {
    pub jid: jid::FullJid,
    pub show: Option<ShowWire>,
    pub status: Option<String>,
    pub priority: i8,
    pub payloads: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountViewWire {
    pub resources: Vec<jid::FullJid>,
    pub available: Vec<(jid::FullJid, i8)>,
    pub routable: Vec<jid::FullJid>,
    pub routing: Vec<RoutingWire>,
    pub presence: Vec<PresenceWire>,
    pub roster_interested: Vec<jid::FullJid>,
    pub blocklist_interested: Vec<jid::FullJid>,
    pub bindings: Vec<(jid::FullJid, BindingRef)>,
}

impl From<&AccountView> for AccountViewWire {
    fn from(view: &AccountView) -> Self {
        Self {
            resources: view.resources.clone(),
            available: view.available.clone(),
            routable: view.routable.clone(),
            routing: view
                .routing
                .iter()
                .map(|resource| RoutingWire {
                    jid: resource.jid.clone(),
                    available: resource.available,
                    priority: resource.priority,
                    carbons_enabled: resource.carbons_enabled,
                })
                .collect(),
            presence: view
                .presence
                .iter()
                .map(|presence| PresenceWire {
                    jid: presence.jid.clone(),
                    show: presence.show.as_ref().map(ShowWire::from),
                    status: presence.status.clone(),
                    priority: presence.priority,
                    payloads: encode_payloads(&presence.payloads),
                })
                .collect(),
            roster_interested: view.roster_interested.clone(),
            blocklist_interested: view.blocklist_interested.clone(),
            bindings: view.bindings.clone(),
        }
    }
}

impl TryFrom<AccountViewWire> for AccountView {
    type Error = AccountBindingWireError;

    fn try_from(wire: AccountViewWire) -> Wire<Self> {
        Ok(AccountView {
            resources: wire.resources,
            available: wire.available,
            routable: wire.routable,
            routing: wire
                .routing
                .into_iter()
                .map(|resource| AccountResourceRouting {
                    jid: resource.jid,
                    available: resource.available,
                    priority: resource.priority,
                    carbons_enabled: resource.carbons_enabled,
                })
                .collect(),
            presence: wire
                .presence
                .into_iter()
                .map(|presence| {
                    Ok(AccountResourcePresence {
                        jid: presence.jid,
                        show: presence.show.map(Show::from),
                        status: presence.status,
                        priority: presence.priority,
                        payloads: decode_payloads(presence.payloads)?,
                    })
                })
                .collect::<Wire<_>>()?,
            roster_interested: wire.roster_interested,
            blocklist_interested: wire.blocklist_interested,
            bindings: wire.bindings,
        })
    }
}

/// A resource slot's claim identity, revision and binding on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotStatusWire {
    pub claim_identity: UserClaimIdentity,
    pub revision: u64,
    pub binding: Option<(BindingRef, BindingStateWire)>,
}

/// One answer of the owning node's coordinators on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityAnswerWire {
    SlotStatus(Box<SlotStatusWire>),
    Executed(BindingReplyWire),
    BindSuccess(Option<Box<BindSuccessReceiptWire>>),
    Presented(bool),
    Unregistered(UnregisterOutcomeWire),
    Repaired(RepairOutcomeWire),
    SessionReleased(SessionReleaseWire),
    ExpiredBindings(Vec<(jid::FullJid, BindingRef)>),
    View(AccountViewWire),
}

impl From<&AccountAuthorityAnswer> for AuthorityAnswerWire {
    fn from(answer: &AccountAuthorityAnswer) -> Self {
        match answer {
            AccountAuthorityAnswer::SlotStatus(status) => {
                Self::SlotStatus(Box::new(SlotStatusWire {
                    claim_identity: status.claim_identity.clone(),
                    revision: status.revision.value(),
                    binding: status
                        .binding
                        .as_ref()
                        .map(|(binding, state)| (binding.clone(), (*state).into())),
                }))
            }
            AccountAuthorityAnswer::Executed(reply) => Self::Executed(reply.into()),
            AccountAuthorityAnswer::BindSuccess(receipt) => Self::BindSuccess(
                receipt
                    .as_deref()
                    .map(|receipt| Box::new(BindSuccessReceiptWire::from(receipt))),
            ),
            AccountAuthorityAnswer::Presented(first) => Self::Presented(*first),
            AccountAuthorityAnswer::Unregistered(outcome) => Self::Unregistered(match outcome {
                UnregisterOutcome::Retiring { revision } => UnregisterOutcomeWire::Retiring {
                    revision: revision.value(),
                },
                UnregisterOutcome::Stale(error) => UnregisterOutcomeWire::Stale((*error).into()),
                UnregisterOutcome::Rejected(error) => {
                    UnregisterOutcomeWire::Rejected((*error).into())
                }
            }),
            AccountAuthorityAnswer::Repaired(outcome) => Self::Repaired(match outcome {
                RepairOutcome::Completed { revision } => RepairOutcomeWire::Completed {
                    revision: revision.value(),
                },
                RepairOutcome::HeldByNewerBinding(binding) => {
                    RepairOutcomeWire::HeldByNewerBinding(binding.clone())
                }
                RepairOutcome::NotRetiring(state) => {
                    RepairOutcomeWire::NotRetiring((*state).into())
                }
                RepairOutcome::Rejected(error) => RepairOutcomeWire::Rejected((*error).into()),
            }),
            AccountAuthorityAnswer::SessionReleased(release) => {
                Self::SessionReleased(match release {
                    SessionRelease::Released { revision } => SessionReleaseWire::Released {
                        revision: revision.value(),
                    },
                    SessionRelease::NotReferenced => SessionReleaseWire::NotReferenced,
                    SessionRelease::HeldByConnection(binding, state) => {
                        SessionReleaseWire::HeldByConnection(binding.clone(), (*state).into())
                    }
                    SessionRelease::Rejected(error) => {
                        SessionReleaseWire::Rejected((*error).into())
                    }
                })
            }
            AccountAuthorityAnswer::ExpiredBindings(bindings) => {
                Self::ExpiredBindings(bindings.clone())
            }
            AccountAuthorityAnswer::View(view) => Self::View(view.into()),
        }
    }
}

impl TryFrom<AuthorityAnswerWire> for AccountAuthorityAnswer {
    type Error = AccountBindingWireError;

    fn try_from(wire: AuthorityAnswerWire) -> Wire<Self> {
        Ok(match wire {
            AuthorityAnswerWire::SlotStatus(status) => {
                let SlotStatusWire {
                    claim_identity,
                    revision,
                    binding,
                } = *status;
                Self::SlotStatus(Box::new(SlotStatus {
                    claim_identity,
                    revision: AccountResourceRevision::new(revision),
                    binding: binding.map(|(binding, state)| (binding, state.into())),
                }))
            }
            AuthorityAnswerWire::Executed(reply) => Self::Executed(reply.into()),
            AuthorityAnswerWire::BindSuccess(receipt) => Self::BindSuccess(
                receipt
                    .map(|receipt| {
                        AccountResourceBindSuccessReceipt::try_from(*receipt).map(Box::new)
                    })
                    .transpose()?,
            ),
            AuthorityAnswerWire::Presented(first) => Self::Presented(first),
            AuthorityAnswerWire::Unregistered(outcome) => Self::Unregistered(match outcome {
                UnregisterOutcomeWire::Retiring { revision } => UnregisterOutcome::Retiring {
                    revision: AccountResourceRevision::new(revision),
                },
                UnregisterOutcomeWire::Stale(error) => UnregisterOutcome::Stale(error.into()),
                UnregisterOutcomeWire::Rejected(error) => UnregisterOutcome::Rejected(error.into()),
            }),
            AuthorityAnswerWire::Repaired(outcome) => Self::Repaired(match outcome {
                RepairOutcomeWire::Completed { revision } => RepairOutcome::Completed {
                    revision: AccountResourceRevision::new(revision),
                },
                RepairOutcomeWire::HeldByNewerBinding(binding) => {
                    RepairOutcome::HeldByNewerBinding(binding)
                }
                RepairOutcomeWire::NotRetiring(state) => RepairOutcome::NotRetiring(state.into()),
                RepairOutcomeWire::Rejected(error) => RepairOutcome::Rejected(error.into()),
            }),
            AuthorityAnswerWire::SessionReleased(release) => Self::SessionReleased(match release {
                SessionReleaseWire::Released { revision } => SessionRelease::Released {
                    revision: AccountResourceRevision::new(revision),
                },
                SessionReleaseWire::NotReferenced => SessionRelease::NotReferenced,
                SessionReleaseWire::HeldByConnection(binding, state) => {
                    SessionRelease::HeldByConnection(binding, state.into())
                }
                SessionReleaseWire::Rejected(error) => SessionRelease::Rejected(error.into()),
            }),
            AuthorityAnswerWire::ExpiredBindings(bindings) => Self::ExpiredBindings(bindings),
            AuthorityAnswerWire::View(view) => Self::View(view.try_into()?),
        })
    }
}

/// Why the owning node's coordinators did not decide a request, on the
/// wire. Failures whose detail stays on the owning node (its store, its
/// mailbox) cross as `Undecided`: for the sender the outcome is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AuthorityErrorWire {
    AccountMismatch,
    ResourceLimit,
    UnknownResource,
    SlotOperationIdConflict,
    SlotSnapshotTooLarge,
    Unclaimed,
    HeldElsewhere,
    Stale,
    ClaimLookupFailed,
    OperationIdConflict,
    ReplacementAlreadyPending,
    RetirementBacklogFull,
    Fenced(FenceErrorWire),
    Diverged,
    Undecided,
}

impl From<&AccountCoordinatorError> for AuthorityErrorWire {
    fn from(error: &AccountCoordinatorError) -> Self {
        match error {
            AccountCoordinatorError::Resources(error) => match error {
                AccountResourcesError::AccountMismatch => Self::AccountMismatch,
                AccountResourcesError::ResourceLimit => Self::ResourceLimit,
                AccountResourcesError::UnknownResource => Self::UnknownResource,
                AccountResourcesError::Slot(AccountResourceExecuteError::OperationIdConflict) => {
                    Self::SlotOperationIdConflict
                }
                AccountResourcesError::Slot(AccountResourceExecuteError::SnapshotTooLarge) => {
                    Self::SlotSnapshotTooLarge
                }
            },
            AccountCoordinatorError::Claim(error) => match error {
                AccountClaimError::Unclaimed => Self::Unclaimed,
                AccountClaimError::HeldElsewhere => Self::HeldElsewhere,
                AccountClaimError::Stale => Self::Stale,
                AccountClaimError::LookupFailed => Self::ClaimLookupFailed,
            },
            AccountCoordinatorError::OperationIdConflict => Self::OperationIdConflict,
            AccountCoordinatorError::ReplacementAlreadyPending => Self::ReplacementAlreadyPending,
            AccountCoordinatorError::RetirementBacklogFull => Self::RetirementBacklogFull,
            AccountCoordinatorError::Fenced(error) => Self::Fenced((*error).into()),
            AccountCoordinatorError::Diverged => Self::Diverged,
            AccountCoordinatorError::Store(_)
            | AccountCoordinatorError::Restore(_)
            | AccountCoordinatorError::Mailbox(_)
            | AccountCoordinatorError::OwnerUndecided => Self::Undecided,
        }
    }
}

impl From<AuthorityErrorWire> for AccountCoordinatorError {
    fn from(wire: AuthorityErrorWire) -> Self {
        use AuthorityErrorWire as W;
        match wire {
            W::AccountMismatch => Self::Resources(AccountResourcesError::AccountMismatch),
            W::ResourceLimit => Self::Resources(AccountResourcesError::ResourceLimit),
            W::UnknownResource => Self::Resources(AccountResourcesError::UnknownResource),
            W::SlotOperationIdConflict => Self::Resources(AccountResourcesError::Slot(
                AccountResourceExecuteError::OperationIdConflict,
            )),
            W::SlotSnapshotTooLarge => Self::Resources(AccountResourcesError::Slot(
                AccountResourceExecuteError::SnapshotTooLarge,
            )),
            W::Unclaimed => Self::Claim(AccountClaimError::Unclaimed),
            W::HeldElsewhere => Self::Claim(AccountClaimError::HeldElsewhere),
            W::Stale => Self::Claim(AccountClaimError::Stale),
            W::ClaimLookupFailed => Self::Claim(AccountClaimError::LookupFailed),
            W::OperationIdConflict => Self::OperationIdConflict,
            W::ReplacementAlreadyPending => Self::ReplacementAlreadyPending,
            W::RetirementBacklogFull => Self::RetirementBacklogFull,
            W::Fenced(error) => Self::Fenced(error.into()),
            W::Diverged => Self::Diverged,
            W::Undecided => Self::OwnerUndecided,
        }
    }
}

/// What one `account_binding.v1` message asks of the receiving node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum AccountBindingRequestWire {
    /// Serve an authority request with the receiver's coordinators. The
    /// receiver serves only an account whose claim it holds, or one it
    /// takes because nobody live holds it.
    Authority(AuthorityRequestWire),
    /// Close the receiver's connection that holds `binding_ref` as
    /// replaced by a fresh bind.
    Displace {
        full_jid: jid::FullJid,
        binding_ref: BindingRef,
    },
}

/// The receiver's answer to one `account_binding.v1` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Reply)]
pub enum AccountBindingReplyWire {
    Answered(AuthorityAnswerWire),
    Refused(AuthorityErrorWire),
    /// Whether the receiver held the displaced connection and told it.
    Displaced(bool),
    /// The receiver does not run the account coordinator, or could not
    /// decode the request. Nothing was decided.
    Unavailable,
}

#[cfg(test)]
mod tests;
