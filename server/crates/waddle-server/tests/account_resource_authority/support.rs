//! Builders for the mutations and evidence the suites drive.

use jid::FullJid;
use waddle_server::account_bindings::AccountBindingStore;
use waddle_server::db::Database;
use waddle_xmpp::ownership::account_resource_transition::{
    AccountResourceExecution, AccountResourceMutation, AccountResourceOperation,
    AccountResourceOperationId, AccountResourceOperationOutcome, AccountResourceRetirementEvidence,
    AccountResourceRevision, AccountResourceSnapshot, AccountResourceTransitionError,
    ActorRegistrationReceipt, ActorUnregistrationEvidence, BindingEvidence,
    BindingRetirementDisposition, DirectedPresenceTeardownDisposition,
    DirectedPresenceTeardownReceipt, EndpointPublicationReceipt, EndpointRetirementEvidence,
    PresentationFlushDisposition, PresentationFlushReceipt, SuccessfulResumeReceipt,
};
use waddle_xmpp::ownership::joint_identity::{ResourceLifetime, SmBinding};
use waddle_xmpp::ownership::{
    BindingRef, ClaimEpoch, ConnectionRef, NodeIdentity, NodeLeaseIdentity, SessionGeneration,
    SmSessionKey, UserClaimIdentity,
};

pub fn claim(epoch: i64) -> UserClaimIdentity {
    UserClaimIdentity::fresh(
        "alice@example.test".parse().expect("valid bare JID"),
        NodeLeaseIdentity::from_node_identity(&NodeIdentity::new("node-a", "epoch-a")),
        ClaimEpoch(epoch),
    )
}

/// Insert the durable stream-management session row a snapshot's session
/// reference points at.
pub async fn insert_session(db: &Database) -> SmSessionKey {
    let session = SmSessionKey::fresh();
    let id = session.as_uuid();
    let connection = db.guard().await.expect("database connection");
    connection
        .execute(
            &format!(
                "INSERT INTO sm_durable_sessions (session_id, resume_token_sha256, full_jid, \
                 state, generation, binding, bare_jid, auth_context_id, auth_context_version, \
                 principal_auth_epoch, inbound_handled, outbound_acked, \
                 outbound_exposure_ceiling, outbound_appended, outbound_unacked_bytes, \
                 max_resume_secs, enable_operation, last_operation, updated_at_ms) \
                 VALUES ('{id}', '{id}', 'alice@example.test/sm', 1, 1, '{{}}', \
                 'alice@example.test', '{id}', 1, 1, 0, 0, 0, 0, 0, 300, '{id}', '{id}', 0)"
            ),
            (),
        )
        .await
        .expect("insert a session row");
    session
}

pub fn resource(name: &str) -> FullJid {
    format!("alice@example.test/{name}")
        .parse()
        .expect("valid full JID")
}

pub fn fresh_binding() -> BindingRef {
    BindingRef::fresh(ConnectionRef::fresh(NodeLeaseIdentity::local_process()))
}

pub fn operation(mutation: AccountResourceMutation) -> AccountResourceOperation {
    AccountResourceOperation::new(AccountResourceOperationId::fresh(), mutation)
}

pub fn publication(binding: &BindingRef) -> EndpointPublicationReceipt {
    EndpointPublicationReceipt::observed(BindingEvidence::for_binding(binding))
}

pub fn registration(claim: &UserClaimIdentity, binding: &BindingRef) -> ActorRegistrationReceipt {
    ActorRegistrationReceipt::observed(claim.clone(), BindingEvidence::for_binding(binding))
}

pub fn sm_binding(session: SmSessionKey, generation: u64) -> SmBinding {
    SmBinding {
        session,
        generation: SessionGeneration::new(generation).expect("valid generation"),
    }
}

pub fn bind(
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
    binding: &BindingRef,
) -> AccountResourceMutation {
    AccountResourceMutation::Bind {
        claim_identity: claim.clone(),
        expected_revision: revision,
        binding_ref: binding.clone(),
        lifetime: ResourceLifetime::fresh(),
    }
}

pub fn publish(
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
    binding: &BindingRef,
) -> AccountResourceMutation {
    AccountResourceMutation::PublishEndpoint {
        claim_identity: claim.clone(),
        expected_revision: revision,
        binding_ref: binding.clone(),
        publication: publication(binding),
    }
}

pub fn register(
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
    binding: &BindingRef,
) -> AccountResourceMutation {
    AccountResourceMutation::RegisterActor {
        claim_identity: claim.clone(),
        expected_revision: revision,
        binding_ref: binding.clone(),
        registration: registration(claim, binding),
    }
}

pub fn update(
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
    binding: &BindingRef,
    snapshot: AccountResourceSnapshot,
) -> AccountResourceMutation {
    AccountResourceMutation::UpdateSnapshot {
        claim_identity: claim.clone(),
        expected_revision: revision,
        binding_ref: binding.clone(),
        snapshot,
    }
}

pub fn presence(available: bool, priority: i8, carbons: bool) -> AccountResourceSnapshot {
    AccountResourceSnapshot {
        presence_available: available,
        presence_priority: priority,
        carbons_enabled: carbons,
        ..AccountResourceSnapshot::default()
    }
}

pub fn detach(
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
    binding: &BindingRef,
) -> AccountResourceMutation {
    AccountResourceMutation::Detach {
        claim_identity: claim.clone(),
        expected_revision: revision,
        binding_ref: binding.clone(),
        endpoint: EndpointRetirementEvidence::Unpublished {
            publication: publication(binding),
            removed: BindingEvidence::for_binding(binding),
        },
        custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
    }
}

pub fn resume(
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
    previous: &BindingRef,
    resumed: &BindingRef,
    previous_sm: SmBinding,
    lifetime: ResourceLifetime,
) -> AccountResourceMutation {
    let resumed_sm = SmBinding {
        session: previous_sm.session,
        generation: previous_sm
            .generation
            .checked_next()
            .expect("next generation"),
    };
    AccountResourceMutation::SuccessfulResume {
        claim_identity: claim.clone(),
        expected_revision: revision,
        previous_binding_ref: previous.clone(),
        binding_ref: resumed.clone(),
        receipt: SuccessfulResumeReceipt::observed(
            BindingEvidence::for_binding(previous),
            BindingEvidence::for_binding(resumed),
            previous_sm,
            resumed_sm,
            lifetime,
            BindingRetirementDisposition::NoAcceptedWork,
        ),
    }
}

pub fn begin_retirement(
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
    binding: &BindingRef,
) -> AccountResourceMutation {
    AccountResourceMutation::BeginRetirement {
        claim_identity: claim.clone(),
        expected_revision: revision,
        binding_ref: binding.clone(),
        custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
    }
}

/// Retirement evidence for a binding that was published and registered
/// under `binding_claim`, whose snapshot was last written at
/// `snapshot_revision`.
pub fn complete_retirement(
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
    binding: &BindingRef,
    binding_claim: &UserClaimIdentity,
    snapshot_revision: AccountResourceRevision,
) -> AccountResourceMutation {
    let evidence = BindingEvidence::for_binding(binding);
    AccountResourceMutation::CompleteRetirement {
        claim_identity: claim.clone(),
        expected_revision: revision,
        binding_ref: binding.clone(),
        evidence: Box::new(AccountResourceRetirementEvidence {
            endpoint: EndpointRetirementEvidence::Unpublished {
                publication: publication(binding),
                removed: evidence.clone(),
            },
            actor: ActorUnregistrationEvidence::Unregistered {
                registration: registration(binding_claim, binding),
                removed: evidence.clone(),
            },
            directed_presence: DirectedPresenceTeardownReceipt::observed(
                evidence.clone(),
                snapshot_revision,
                DirectedPresenceTeardownDisposition::NoDirectedRecipients,
            ),
            presentation_flush: PresentationFlushReceipt::observed(
                evidence,
                snapshot_revision,
                PresentationFlushDisposition::NoPendingPresentation,
            ),
            custody_disposition: BindingRetirementDisposition::NoAcceptedWork,
        }),
    }
}

/// Execute a mutation that must commit now and return its revision.
pub async fn commit(
    store: &dyn AccountBindingStore,
    jid: &FullJid,
    mutation: AccountResourceMutation,
) -> AccountResourceRevision {
    match store
        .execute(&claim(1), jid, &operation(mutation))
        .await
        .expect("store answers")
        .execution
    {
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Committed {
            revision,
        }) => revision,
        other => panic!("expected a committed mutation, got {other:?}"),
    }
}

/// Execute a mutation that must be rejected now and return the reason.
pub async fn reject(
    store: &dyn AccountBindingStore,
    jid: &FullJid,
    mutation: AccountResourceMutation,
) -> AccountResourceTransitionError {
    match store
        .execute(&claim(1), jid, &operation(mutation))
        .await
        .expect("store answers")
        .execution
    {
        AccountResourceExecution::Applied(AccountResourceOperationOutcome::Rejected(error)) => {
            error
        }
        other => panic!("expected a rejected mutation, got {other:?}"),
    }
}

/// Bind `jid` and take the binding to `Active`.
pub async fn activate(
    store: &dyn AccountBindingStore,
    jid: &FullJid,
    claim: &UserClaimIdentity,
    revision: AccountResourceRevision,
) -> (BindingRef, AccountResourceRevision) {
    let binding = fresh_binding();
    let revision = commit(store, jid, bind(claim, revision, &binding)).await;
    let revision = commit(store, jid, publish(claim, revision, &binding)).await;
    let revision = commit(store, jid, register(claim, revision, &binding)).await;
    (binding, revision)
}
