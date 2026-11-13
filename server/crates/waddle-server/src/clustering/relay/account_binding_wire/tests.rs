use super::super::implementation::RelayAccountBinding;
use super::super::RelayActor;
use super::{
    AccountBindingReplyWire, AccountBindingRequestWire, AuthorityAnswerWire, AuthorityErrorWire,
    AuthorityRequestWire, BindSuccessReceiptWire, ACCOUNT_BINDING_V1_ID,
};
use crate::account_bindings::AccountBindingStoreError;
use crate::account_coordinator::{
    AccountAuthorityAnswer, AccountAuthorityRequest, AccountClaimError, AccountCoordinatorError,
    AccountView, BindingReply, ReleaseSessionRequest, RepairEvidence, SlotStatus,
    UnregisterRequest,
};
use kameo::remote::RemoteMessage;
use minidom::Element;
use waddle_xmpp::ownership::account_coordinator::{
    AccountResourcePresence, AccountResourceRouting, Presentation,
};
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceBindSuccessReceipt, AccountResourceBindingState, AccountResourceExecution,
    AccountResourceMutation, AccountResourceOperation, AccountResourceOperationId,
    AccountResourceOperationOutcome, AccountResourceRevision, AccountResourceSnapshot,
    AccountResourceTransitionError, ActorRegistrationReceipt, BindingEvidence,
    BindingRetirementDisposition, DirectedPresenceTeardownDisposition, EndpointPublicationReceipt,
    EndpointRetirementEvidence, PresentationFlushDisposition, SuccessfulResumeReceipt,
};
use waddle_xmpp::ownership::joint_identity::{
    ResourceLifetime, SessionGeneration, SmBinding, SmSessionKey,
};
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ConnectionRef, NodeIdentity, NodeLeaseIdentity, UserClaimIdentity,
};
use xmpp_parsers::presence::Show;

fn full_jid() -> jid::FullJid {
    "alice@example.test/phone".parse().expect("full JID")
}

fn node_lease(node: &str) -> NodeLeaseIdentity {
    NodeLeaseIdentity::from_node_identity(&NodeIdentity::new(node, "epoch-1"))
}

fn binding_on(node: &str) -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(node_lease(node)))
}

fn claim_identity() -> UserClaimIdentity {
    UserClaimIdentity::fresh(
        "alice@example.test".parse().expect("bare JID"),
        node_lease("owner-a"),
        ClaimEpoch(3),
    )
}

fn sm_binding(generation: u64) -> SmBinding {
    SmBinding {
        session: SmSessionKey::fresh(),
        generation: SessionGeneration::new(generation).expect("session generation"),
    }
}

fn payload() -> Element {
    Element::builder("c", "http://jabber.org/protocol/caps")
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "https://waddle.social",
        )
        .build()
}

/// Every message on the relay is MessagePack with named fields.
fn over_the_relay<T>(value: &T) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned,
{
    let encoded = rmp_serde::to_vec_named(value).expect("encode named MessagePack message");
    rmp_serde::from_slice(&encoded).expect("decode named MessagePack message")
}

/// The request as the owning node rebuilds it.
fn served(request: &AccountAuthorityRequest) -> AccountAuthorityRequest {
    let wire = AuthorityRequestWire::try_from(request).expect("the request has a wire form");
    let message = AccountBindingRequestWire::Authority(wire);
    match over_the_relay(&message) {
        AccountBindingRequestWire::Authority(wire) => {
            AccountAuthorityRequest::try_from(wire).expect("the owner decodes the request")
        }
        AccountBindingRequestWire::Displace { .. } => panic!("an authority request"),
    }
}

/// The answer as the requesting node reads it.
fn read_back(answer: &AccountAuthorityAnswer) -> AccountAuthorityAnswer {
    let reply = AccountBindingReplyWire::Answered(answer.into());
    match over_the_relay(&reply) {
        AccountBindingReplyWire::Answered(wire) => {
            AccountAuthorityAnswer::try_from(wire).expect("the requester decodes the answer")
        }
        other => panic!("an answer, not {other:?}"),
    }
}

fn execute(mutation: AccountResourceMutation) -> AccountAuthorityRequest {
    AccountAuthorityRequest::Execute {
        full_jid: full_jid(),
        operation: Box::new(AccountResourceOperation::new(
            AccountResourceOperationId::fresh(),
            mutation,
        )),
    }
}

#[test]
fn the_message_id_is_the_versioned_account_binding_id() {
    assert_eq!(
        <RelayActor as RemoteMessage<RelayAccountBinding>>::REMOTE_ID,
        ACCOUNT_BINDING_V1_ID
    );
    assert_eq!(
        ACCOUNT_BINDING_V1_ID,
        "waddle.clustering.relay.account_binding.v1"
    );
}

#[test]
fn every_mutation_a_connection_sends_reaches_the_owner_unchanged() {
    let binding = binding_on("host-b");
    let previous = binding_on("host-a");
    let evidence = BindingEvidence::for_binding(&binding);
    let revision = AccountResourceRevision::new(4);
    let snapshot = AccountResourceSnapshot {
        presence_available: true,
        presence_show: Some(Show::Dnd),
        presence_status: Some("in a meeting".to_owned()),
        presence_priority: -2,
        presence_payloads: vec![payload()],
        carbons_enabled: true,
        roster_interested: true,
        blocklist_interested: false,
        stream_management_ref: Some(sm_binding(2)),
    };
    let mutations = vec![
        AccountResourceMutation::Bind {
            claim_identity: claim_identity(),
            expected_revision: revision,
            binding_ref: binding.clone(),
            lifetime: ResourceLifetime::fresh(),
        },
        AccountResourceMutation::PublishEndpoint {
            claim_identity: claim_identity(),
            expected_revision: revision,
            binding_ref: binding.clone(),
            publication: EndpointPublicationReceipt::observed(evidence.clone()),
        },
        AccountResourceMutation::RegisterActor {
            claim_identity: claim_identity(),
            expected_revision: revision,
            binding_ref: binding.clone(),
            registration: ActorRegistrationReceipt::observed(claim_identity(), evidence.clone()),
        },
        AccountResourceMutation::UpdateSnapshot {
            claim_identity: claim_identity(),
            expected_revision: revision,
            binding_ref: binding.clone(),
            snapshot,
        },
        AccountResourceMutation::Detach {
            claim_identity: claim_identity(),
            expected_revision: revision,
            binding_ref: binding.clone(),
            endpoint: EndpointRetirementEvidence::Unpublished {
                publication: EndpointPublicationReceipt::observed(evidence.clone()),
                removed: evidence.clone(),
            },
            custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
        },
        AccountResourceMutation::Detach {
            claim_identity: claim_identity(),
            expected_revision: revision,
            binding_ref: binding.clone(),
            endpoint: EndpointRetirementEvidence::NeverPublished {
                source: evidence.clone(),
            },
            custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
        },
        AccountResourceMutation::SuccessfulResume {
            claim_identity: claim_identity(),
            expected_revision: revision,
            previous_binding_ref: previous.clone(),
            binding_ref: binding.clone(),
            receipt: SuccessfulResumeReceipt::observed(
                BindingEvidence::for_binding(&previous),
                evidence,
                sm_binding(1),
                sm_binding(2),
                ResourceLifetime::fresh(),
                BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
            ),
        },
    ];
    for mutation in mutations {
        let request = execute(mutation);
        assert_eq!(served(&request), request);
    }
}

#[test]
fn the_coordinators_own_mutations_have_no_wire_form() {
    let request = execute(AccountResourceMutation::BeginRetirement {
        claim_identity: claim_identity(),
        expected_revision: AccountResourceRevision::INITIAL,
        binding_ref: binding_on("host-a"),
        custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
    });
    assert!(AuthorityRequestWire::try_from(&request).is_err());
}

#[test]
fn reads_retirements_and_presentations_reach_the_owner_unchanged() {
    let binding = binding_on("host-b");
    let requests = vec![
        AccountAuthorityRequest::SlotStatus {
            full_jid: full_jid(),
        },
        AccountAuthorityRequest::BindSuccess {
            full_jid: full_jid(),
            binding_ref: binding.clone(),
        },
        AccountAuthorityRequest::MarkPresented {
            full_jid: full_jid(),
            binding_ref: binding.clone(),
            presentation: Presentation::PendingSubscriptions,
        },
        AccountAuthorityRequest::Unregister {
            full_jid: full_jid(),
            request: UnregisterRequest {
                operation: AccountResourceOperationId::fresh(),
                binding_ref: binding.clone(),
                custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
            },
        },
        AccountAuthorityRequest::Repair {
            full_jid: full_jid(),
            binding_ref: binding,
            evidence: RepairEvidence {
                operation: AccountResourceOperationId::fresh(),
                directed_presence:
                    DirectedPresenceTeardownDisposition::RecipientsRetractedAndUnavailableDelivered,
                presentation_flush: PresentationFlushDisposition::Delivered,
            },
        },
        AccountAuthorityRequest::ReleaseSession {
            full_jid: full_jid(),
            request: ReleaseSessionRequest {
                begin: AccountResourceOperationId::fresh(),
                complete: AccountResourceOperationId::fresh(),
                session: SmSessionKey::fresh(),
                custody_disposition: BindingRetirementDisposition::AcceptedWorkHasRecoveryOwner,
                directed_presence:
                    DirectedPresenceTeardownDisposition::RecipientsRetractedAndUnavailableRecoveryOwned,
                presentation_flush: PresentationFlushDisposition::RecoveryOwner,
            },
        },
        AccountAuthorityRequest::ExpiredBindings {
            account: "alice@example.test".parse().expect("bare JID"),
        },
        AccountAuthorityRequest::View {
            account: "alice@example.test".parse().expect("bare JID"),
        },
    ];
    for request in requests {
        assert_eq!(served(&request), request);
    }
}

fn bind_success_receipt(binding: &BindingRef) -> AccountResourceBindSuccessReceipt {
    let evidence = BindingEvidence::for_binding(binding);
    AccountResourceBindSuccessReceipt::attested(
        claim_identity(),
        evidence.clone(),
        AccountResourceRevision::new(3),
        EndpointPublicationReceipt::observed(evidence.clone()),
        ActorRegistrationReceipt::observed(claim_identity(), evidence),
    )
    .expect("consistent receipt parts")
}

#[test]
fn the_owners_answers_reach_the_requester_unchanged() {
    let binding = binding_on("host-b");
    let laptop: jid::FullJid = "alice@example.test/laptop".parse().expect("full JID");
    let answers = vec![
        AccountAuthorityAnswer::SlotStatus(Box::new(SlotStatus {
            claim_identity: claim_identity(),
            revision: AccountResourceRevision::new(5),
            binding: Some((binding.clone(), AccountResourceBindingState::Detached)),
        })),
        AccountAuthorityAnswer::Executed(BindingReply::Executed(
            AccountResourceExecution::Replayed(AccountResourceOperationOutcome::Committed {
                revision: AccountResourceRevision::new(6),
            }),
        )),
        AccountAuthorityAnswer::Executed(BindingReply::Executed(
            AccountResourceExecution::Applied(AccountResourceOperationOutcome::Rejected(
                AccountResourceTransitionError::ResumeConnectionMismatch,
            )),
        )),
        AccountAuthorityAnswer::Executed(BindingReply::ReplacementPending {
            displaced: binding.clone(),
            revision: AccountResourceRevision::new(2),
        }),
        AccountAuthorityAnswer::BindSuccess(Some(Box::new(bind_success_receipt(&binding)))),
        AccountAuthorityAnswer::BindSuccess(None),
        AccountAuthorityAnswer::Presented(true),
        AccountAuthorityAnswer::ExpiredBindings(vec![(full_jid(), binding)]),
        AccountAuthorityAnswer::View(AccountView {
            resources: vec![full_jid(), laptop.clone()],
            available: vec![(laptop.clone(), 5)],
            routable: vec![laptop.clone()],
            routing: vec![AccountResourceRouting {
                jid: laptop.clone(),
                available: true,
                priority: 5,
                carbons_enabled: true,
            }],
            presence: vec![AccountResourcePresence {
                jid: laptop.clone(),
                show: Some(Show::Away),
                status: Some("lunch".to_owned()),
                priority: 5,
                payloads: vec![payload()],
            }],
            roster_interested: vec![laptop],
            blocklist_interested: Vec::new(),
        }),
    ];
    for answer in answers {
        assert_eq!(read_back(&answer), answer);
    }
}

#[test]
fn a_receipt_whose_parts_disagree_is_not_read_as_an_authorization() {
    let binding = binding_on("host-b");
    let mut wire = BindSuccessReceiptWire::from(&bind_success_receipt(&binding));
    wire.registration.source = (&BindingEvidence::for_binding(&binding_on("host-c"))).into();
    let answer = AuthorityAnswerWire::BindSuccess(Some(Box::new(wire)));
    assert!(AccountAuthorityAnswer::try_from(answer).is_err());
}

#[test]
fn a_refusal_keeps_its_reason_and_a_failure_of_the_owner_stays_undecided() {
    let held = AuthorityErrorWire::from(&AccountCoordinatorError::Claim(
        AccountClaimError::HeldElsewhere,
    ));
    assert!(matches!(
        AccountCoordinatorError::from(over_the_relay(&held)),
        AccountCoordinatorError::Claim(AccountClaimError::HeldElsewhere)
    ));
    let store = AuthorityErrorWire::from(&AccountCoordinatorError::Store(
        AccountBindingStoreError::CorruptRow { field: "state" },
    ));
    assert!(matches!(
        AccountCoordinatorError::from(over_the_relay(&store)),
        AccountCoordinatorError::OwnerUndecided
    ));
}

#[test]
fn a_session_generation_of_zero_is_not_a_session() {
    let binding = binding_on("host-b");
    let request = execute(AccountResourceMutation::UpdateSnapshot {
        claim_identity: claim_identity(),
        expected_revision: AccountResourceRevision::INITIAL,
        binding_ref: binding,
        snapshot: AccountResourceSnapshot {
            stream_management_ref: Some(sm_binding(1)),
            ..AccountResourceSnapshot::default()
        },
    });
    let mut wire = AuthorityRequestWire::try_from(&request).expect("wire form");
    if let AuthorityRequestWire::Execute { mutation, .. } = &mut wire {
        if let super::MutationWire::UpdateSnapshot { snapshot, .. } = mutation.as_mut() {
            if let Some(sm) = snapshot.stream_management_ref.as_mut() {
                sm.generation = 0;
            }
        }
    }
    assert!(AccountAuthorityRequest::try_from(wire).is_err());
}
