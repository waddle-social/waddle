//! Comprehensive M4 tests for 3-fence `AdmissionLedgerStore` (Turso WAL &
//! SqliteCompat), `BrokerReplicaFence`, device-signed invocation, payer
//! separation, principal denial, `unknown_after_send` quarantine, tenant
//! deletion cascade, and deterministic `Barrier` + PObserve trace verification.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use p256::elliptic_curve::rand_core::OsRng;
use tempfile::TempDir;
use tokio::sync::Barrier;
use uuid::Uuid;
use waddle_broker::admission::invocation::{
    validate_principal_and_payer, InvocationError, InvocationPrincipal, PayerSelection,
    SignedInferenceInvocation,
};
use waddle_broker::admission::postgres::SqliteCompatAdmissionLedgerStore;
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::service::{
    AdmissionBarrierHooks, AdmissionServiceError, AdmitInvocationInput, BrokerAdmissionService,
    TraceRecorder,
};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::{AdmissionQueryStore, AttemptStatus};
use waddle_broker::authz::local::LocalPermissionBackend;
use waddle_broker::authz::openfga::{
    InProcessOpenFgaServer, OpenFgaConfig, OpenFgaPermissionBackend,
};
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::custody::{BrokerCustodyService, EnrollCredentialInput};
use waddle_broker::idp_proof::{
    build_device_enroll_pop_transcript, p256_jwk_thumbprint, AuthProvenance, DeviceKeyRegistry,
    EnrollDeviceKeyInput, VerifiedIdpIntent,
};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::admission::{
    AdmissionError, AdmissionLedgerStore, AdmitAttemptRequest, AttemptId, AttemptOutcome,
    BudgetLimits, CredentialKey, CredentialScope, FenceKey, FenceKind, FenceSnapshot, FenceState,
    RoomScopeKey, RunId, StepId, ThreeFenceSnapshot,
};
use waddle_broker_core::boot_clock::FakeBootClock;
use waddle_broker_core::identity::{
    AccountId, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckRequest, PermissionBackend, PermissionBackendError,
};
use waddle_broker_core::placement::FenceGeneration;
use waddle_broker_core::store::{TenantQueryScope, TenantStore};
use waddle_broker_core::trace_verifier::{assert_secret_free, verify_jsonl_trace};
use waddle_tenancy::turso_store::{TursoPoolConfig, TursoTenantStore};

fn test_tenant(name: &str) -> TenantId {
    TenantId::parse(name).expect("valid tenant id")
}

fn test_holder(spiffe: &str) -> HolderInstance {
    HolderInstance {
        spiffe_id: SpiffeId::parse(spiffe).expect("valid spiffe id"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([7u8; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![1u8; 32]).expect("valid proof"),
    }
}

fn default_budget() -> BudgetLimits {
    BudgetLimits {
        max_input_tokens: 10_000,
        max_output_tokens: 4_000,
        max_spend_microcents: 1_000_000,
        max_steps: 4,
    }
}

fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn enroll_test_device(
    registry: &DeviceKeyRegistry,
    tenant: &TenantId,
    account_id: AccountId,
) -> (Uuid, SigningKey) {
    let signing_key = SigningKey::random(&mut OsRng);
    let pubkey_sec1 = signing_key
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let jkt = p256_jwk_thumbprint(&pubkey_sec1).expect("jkt");
    let device_key_id = Uuid::new_v4();
    let now = now_ms();
    let expires_at_ms = now + 60_000;
    let transcript = build_device_enroll_pop_transcript(
        "trust.example",
        tenant,
        account_id,
        device_key_id,
        "enroll-nonce-1",
        expires_at_ms,
    );
    let sig: Signature = signing_key.sign(&transcript);
    let idp_intent = VerifiedIdpIntent {
        account_id,
        tenant_id: tenant.clone(),
        device_jkt: Some(jkt),
        is_tenant_admin: false,
        proof_id: "idp-proof-1".to_owned(),
    };
    registry
        .enroll_device_key(EnrollDeviceKeyInput {
            trust_domain: "trust.example",
            provenance: AuthProvenance::ExternalIdp,
            idp_intent: &idp_intent,
            device_key_id,
            public_key_sec1: pubkey_sec1,
            nonce: "enroll-nonce-1",
            expires_at_ms,
            now_ms: now,
            pop_signature: &sig.to_bytes(),
        })
        .expect("enroll device key");
    (device_key_id, signing_key)
}

struct SignedInvocationParams<'a> {
    tenant: &'a TenantId,
    account_id: AccountId,
    device_key_id: Uuid,
    signing_key: &'a SigningKey,
    room: &'a RoomScopeKey,
    payer: PayerSelection,
    nonce: [u8; 32],
    expires_at_ms: i64,
    budget: BudgetLimits,
}

fn build_signed_invocation(params: SignedInvocationParams<'_>) -> SignedInferenceInvocation {
    let mut inv = SignedInferenceInvocation {
        trust_domain: "trust.example".to_owned(),
        tenant_id: params.tenant.clone(),
        account_id: params.account_id,
        device_key_id: params.device_key_id,
        extension_id: "ai-chatbot".to_owned(),
        extension_digest_sha256: [0xAB; 32],
        room_scope: params.room.clone(),
        provider: "openai".to_owned(),
        payer: params.payer,
        command_digest_sha256: [0xCD; 32],
        nonce: params.nonce,
        expires_at_ms: params.expires_at_ms,
        budget: params.budget,
        signature: Vec::new(),
    };
    let sig: Signature = params.signing_key.sign(&inv.canonical_transcript());
    inv.signature = sig.to_bytes().to_vec();
    inv
}

async fn exercise_three_fence_contract<S>(store: &S)
where
    S: AdmissionLedgerStore + AdmissionQueryStore,
{
    let tenant = test_tenant("acme.example");
    let cred = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room = RoomScopeKey {
        scope_id: "room-general".to_owned(),
    };
    let snap = store
        .snapshot_fences(&tenant, &cred, &room)
        .await
        .expect("snapshot fences");
    assert_eq!(snap.credential.generation, FenceGeneration::new(1));
    assert_eq!(snap.room_or_space.generation, FenceGeneration::new(1));
    assert_eq!(snap.placement.generation, FenceGeneration::new(1));
    assert_eq!(snap.credential.state, FenceState::Open);

    let attempt_id = AttemptId::new_v4();
    let run_id = RunId::new_v4();
    let nonce = [1u8; 32];
    let req = AdmitAttemptRequest {
        attempt_id,
        run_id,
        step_id: StepId::new(0),
        tenant_id: tenant.clone(),
        credential: cred.clone(),
        room_or_space: room.clone(),
        holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
        expected_fences: snap,
        invocation_nonce: nonce,
        expires_at_ms: now_ms() + 60_000,
        reserved_spend_microcents: 250_000,
        budget: default_budget(),
    };
    let receipt = store.admit_attempt(&req).await.expect("admit attempt");
    assert_eq!(receipt.attempt_id, attempt_id);

    // Replaying the exact same nonce fails closed with NonceReplayed.
    let mut replay_req = req.clone();
    replay_req.attempt_id = AttemptId::new_v4();
    replay_req.step_id = StepId::new(1);
    assert!(matches!(
        store.admit_attempt(&replay_req).await,
        Err(AdmissionError::NonceReplayed)
    ));

    // Closing the credential fence denies admission with stale snapshot.
    let cred_key = FenceKey {
        tenant_id: tenant.clone(),
        kind: FenceKind::Credential(cred.clone()),
    };
    let next_gen = store
        .close_and_advance_fence(&cred_key)
        .await
        .expect("close cred fence");
    assert_eq!(next_gen, FenceGeneration::new(2));

    let mut stale_req = req.clone();
    stale_req.attempt_id = AttemptId::new_v4();
    stale_req.step_id = StepId::new(1);
    stale_req.invocation_nonce = [2u8; 32];
    assert!(matches!(
        store.admit_attempt(&stale_req).await,
        Err(AdmissionError::FenceStaleOrClosed(FenceKind::Credential(_)))
    ));

    // Reopening with wrong generation fails; reopening with exact generation succeeds.
    assert!(matches!(
        store.reopen_fence(&cred_key, FenceGeneration::new(1)).await,
        Err(AdmissionError::FenceStaleOrClosed(_))
    ));
    store
        .reopen_fence(&cred_key, next_gen)
        .await
        .expect("reopen at gen 2");

    // Reconcile attempt 1 and check budget accounting.
    store
        .reconcile_attempt(
            attempt_id,
            AttemptOutcome::Completed {
                input_tokens: 500,
                output_tokens: 200,
                actual_spend_microcents: 120_000,
            },
        )
        .await
        .expect("reconcile completed");

    let run = store
        .get_run_budget(run_id)
        .await
        .expect("get run")
        .expect("run exists");
    assert_eq!(run.spent_microcents, 120_000);
    assert_eq!(run.reserved_microcents, 0);
    assert_eq!(run.steps_used, 1);
}

#[tokio::test]
async fn admission_ledger_three_fence_turso_and_sqlite_compat() {
    let dir = TempDir::new().expect("tempdir");
    let turso_store = TursoAdmissionLedgerStore::open(dir.path().join("admission.db"))
        .await
        .expect("open turso admission store");
    exercise_three_fence_contract(&turso_store).await;

    let sqlite_store = SqliteCompatAdmissionLedgerStore::in_memory()
        .await
        .expect("open sqlite compat store");
    exercise_three_fence_contract(&sqlite_store).await;
}

#[tokio::test]
async fn replica_fence_closes_before_mutation_and_aborts_peer_streams() {
    let dir = TempDir::new().expect("tempdir");
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("replica.db"))
            .await
            .expect("open turso"),
    );
    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    let tenant = test_tenant("acme.example");
    perms
        .provision_tenant_store(&tenant)
        .await
        .expect("provision");
    let cred_id = Uuid::new_v4();
    let account = AccountId::new_v4();
    let cred_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::UserCredential,
            id: cred_id.to_string(),
        },
        relation: AuthzRelation::new("owner"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: account.to_string(),
            relation: None,
        },
    };
    perms
        .write_tuples(
            &tenant,
            AuthzModule::Inference,
            std::slice::from_ref(&cred_tuple),
        )
        .await
        .expect("grant user cred");

    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(100)));
    let fence_a = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn waddle_broker::admission::ReplicaFenceStore>,
        Arc::clone(&clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
    ));
    let fence_b = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn waddle_broker::admission::ReplicaFenceStore>,
        Arc::clone(&clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
    ));
    fence_a.tick_once().await.expect("tick a");
    fence_b.tick_once().await.expect("tick b");

    let device_keys = Arc::new(DeviceKeyRegistry::new());
    let (dev_id, signing_key) = enroll_test_device(&device_keys, &tenant, account);
    let trace = TraceRecorder::new();

    let svc_a = BrokerAdmissionService::new(
        "broker-a",
        Arc::clone(&ledger),
        Arc::clone(&perms) as Arc<dyn PermissionBackend>,
        Arc::clone(&device_keys),
        Arc::clone(&fence_a),
        trace.clone(),
    );
    let svc_b = BrokerAdmissionService::new(
        "broker-b",
        Arc::clone(&ledger),
        Arc::clone(&perms) as Arc<dyn PermissionBackend>,
        Arc::clone(&device_keys),
        Arc::clone(&fence_b),
        trace.clone(),
    );

    let room = RoomScopeKey {
        scope_id: "room-1".to_owned(),
    };
    let payer = PayerSelection::Member {
        account_id: account,
        credential_id: cred_id,
    };
    let nonce = [9u8; 32];
    let exp = now_ms() + 60_000;
    let signed = build_signed_invocation(SignedInvocationParams {
        tenant: &tenant,
        account_id: account,
        device_key_id: dev_id,
        signing_key: &signing_key,
        room: &room,
        payer,
        nonce,
        expires_at_ms: exp,
        budget: default_budget(),
    });

    // Replica B admits an in-flight stream.
    let admitted = svc_b
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            payer,
            room_or_space: room.clone(),
            holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-b"),
            invocation_nonce: nonce,
            expires_at_ms: exp,
            reserved_spend_microcents: 50_000,
            budget: default_budget(),
            signed_invocation: Some(signed),
            now_ms: now_ms(),
        })
        .await
        .expect("admit on broker-b");
    assert!(!*admitted.cancel_rx.borrow());

    // Replica A revokes the credential: closes fence -> deletes tuple -> ticks peer B -> acks.
    let fence_key = FenceKey {
        tenant_id: tenant.clone(),
        kind: FenceKind::Credential(payer.credential_key()),
    };
    let perms_for_revoke = Arc::clone(&perms);
    let tenant_for_revoke = tenant.clone();
    let new_gen = svc_a
        .restrict_fence(
            &fence_key,
            &[Arc::clone(&fence_b)],
            false,
            move || async move {
                perms_for_revoke
                    .delete_tuples(&tenant_for_revoke, AuthzModule::Inference, &[cred_tuple])
                    .await
            },
        )
        .await
        .expect("restrict fence");
    assert_eq!(new_gen, FenceGeneration::new(2));

    // Peer B's in-flight stream was aborted BEFORE RevokeAck completed!
    assert!(*admitted.cancel_rx.borrow(), "peer stream must be canceled");
    assert_eq!(fence_b.active_stream_count(), 0);

    let jsonl = trace.to_jsonl();
    assert_secret_free(&jsonl, &["sk-test-secret"]).expect("secret-free trace");
    verify_jsonl_trace(&jsonl).expect("PObserve trace valid");
}

#[tokio::test]
async fn run_step_ledger_bounds_multi_turn_steps_tokens_and_spend() {
    let store = SqliteCompatAdmissionLedgerStore::in_memory()
        .await
        .expect("open sqlite compat");
    let tenant = test_tenant("acme.example");
    let cred = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room = RoomScopeKey {
        scope_id: "room-tools".to_owned(),
    };
    let snap = store
        .snapshot_fences(&tenant, &cred, &room)
        .await
        .expect("snapshot");
    let run_id = RunId::new_v4();
    let budget = BudgetLimits {
        max_input_tokens: 1_000,
        max_output_tokens: 500,
        max_spend_microcents: 300,
        max_steps: 2,
    };

    for step in 0u32..2 {
        let attempt_id = AttemptId::new_v4();
        let req = AdmitAttemptRequest {
            attempt_id,
            run_id,
            step_id: StepId::new(step),
            tenant_id: tenant.clone(),
            credential: cred.clone(),
            room_or_space: room.clone(),
            holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            expected_fences: snap,
            invocation_nonce: [u8::try_from(step + 10).expect("u8"); 32],
            expires_at_ms: now_ms() + 60_000,
            reserved_spend_microcents: 150,
            budget,
        };
        store.admit_attempt(&req).await.expect("admit step");
        store
            .reconcile_attempt(
                attempt_id,
                AttemptOutcome::Completed {
                    input_tokens: 400,
                    output_tokens: 200,
                    actual_spend_microcents: 120,
                },
            )
            .await
            .expect("reconcile step");
    }

    // Step 2 (3rd step) exceeds max_steps = 2 -> BudgetExceeded.
    let over_step_req = AdmitAttemptRequest {
        attempt_id: AttemptId::new_v4(),
        run_id,
        step_id: StepId::new(2),
        tenant_id: tenant.clone(),
        credential: cred.clone(),
        room_or_space: room.clone(),
        holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
        expected_fences: snap,
        invocation_nonce: [99u8; 32],
        expires_at_ms: now_ms() + 60_000,
        reserved_spend_microcents: 10,
        budget,
    };
    assert!(matches!(
        store.admit_attempt(&over_step_req).await,
        Err(AdmissionError::BudgetExceeded)
    ));

    // Duplicate step_id on a fresh run with max_steps = 3 is rejected.
    let run2 = RunId::new_v4();
    let req_a = AdmitAttemptRequest {
        attempt_id: AttemptId::new_v4(),
        run_id: run2,
        step_id: StepId::new(0),
        tenant_id: tenant.clone(),
        credential: cred.clone(),
        room_or_space: room.clone(),
        holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
        expected_fences: snap,
        invocation_nonce: [101u8; 32],
        expires_at_ms: now_ms() + 60_000,
        reserved_spend_microcents: 50,
        budget: default_budget(),
    };
    store.admit_attempt(&req_a).await.expect("step 0");
    let mut dup_step = req_a.clone();
    dup_step.attempt_id = AttemptId::new_v4();
    dup_step.invocation_nonce = [102u8; 32];
    assert!(matches!(
        store.admit_attempt(&dup_step).await,
        Err(AdmissionError::DuplicateAttempt)
    ));
}

#[tokio::test]
async fn payer_separation_never_silently_switches_member_to_tenant() {
    let ledger = Arc::new(
        SqliteCompatAdmissionLedgerStore::in_memory()
            .await
            .expect("sqlite compat"),
    );
    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    let tenant = test_tenant("acme.example");
    perms.provision_tenant_store(&tenant).await.expect("prov");

    let account = AccountId::new_v4();
    let tenant_cred_id = Uuid::new_v4();
    let member_cred_id = Uuid::new_v4();

    // Grant ONLY the tenant credential to account; do NOT grant the member credential.
    perms
        .write_tuples(
            &tenant,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::TenantCredential,
                    id: tenant_cred_id.to_string(),
                },
                relation: AuthzRelation::new("tenant_admin"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: account.to_string(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("grant tenant cred");

    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(10)));
    let fence = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn waddle_broker::admission::ReplicaFenceStore>,
        clock,
    ));
    fence.tick_once().await.expect("tick");
    let device_keys = Arc::new(DeviceKeyRegistry::new());
    let (dev_id, signing_key) = enroll_test_device(&device_keys, &tenant, account);
    let trace = TraceRecorder::new();

    let svc = BrokerAdmissionService::new(
        "broker-1",
        Arc::clone(&ledger),
        Arc::clone(&perms) as Arc<dyn PermissionBackend>,
        Arc::clone(&device_keys),
        Arc::clone(&fence),
        trace.clone(),
    );

    let room = RoomScopeKey {
        scope_id: "room-1".to_owned(),
    };
    let member_payer = PayerSelection::Member {
        account_id: account,
        credential_id: member_cred_id,
    };
    let nonce = [42u8; 32];
    let exp = now_ms() + 60_000;
    let signed = build_signed_invocation(SignedInvocationParams {
        tenant: &tenant,
        account_id: account,
        device_key_id: dev_id,
        signing_key: &signing_key,
        room: &room,
        payer: member_payer,
        nonce,
        expires_at_ms: exp,
        budget: default_budget(),
    });

    let res = svc
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            payer: member_payer,
            room_or_space: room,
            holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            invocation_nonce: nonce,
            expires_at_ms: exp,
            reserved_spend_microcents: 100,
            budget: default_budget(),
            signed_invocation: Some(signed),
            now_ms: now_ms(),
        })
        .await;

    // MUST fail closed with AuthorizationDenied for Member payer — never silently switch to Tenant!
    assert!(matches!(
        res,
        Err(AdmissionServiceError::AuthorizationDenied {
            payer: PayerSelection::Member { .. }
        })
    ));
    assert!(
        trace.records().is_empty(),
        "no dispatch emitted on denied payer"
    );
}

#[test]
fn principal_denial_blocks_observer_schedule_webhook_federated_member_creds() {
    let owner = AccountId::new_v4();
    let other = AccountId::new_v4();
    let member_payer = PayerSelection::Member {
        account_id: owner,
        credential_id: Uuid::new_v4(),
    };

    for principal in [
        InvocationPrincipal::Observation,
        InvocationPrincipal::Schedule,
        InvocationPrincipal::Webhook,
        InvocationPrincipal::FederatedRemote {
            remote_jid: "bob@remote.example".to_owned(),
        },
    ] {
        assert!(matches!(
            validate_principal_and_payer(&principal, member_payer),
            Err(InvocationError::PrincipalCannotUseMemberCredential { .. })
        ));
    }

    // SCRAM native_users also denied member credentials.
    assert!(matches!(
        validate_principal_and_payer(
            &InvocationPrincipal::LocalMember {
                account_id: owner,
                provenance: AuthProvenance::NativeScram,
            },
            member_payer,
        ),
        Err(InvocationError::Auth(_))
    ));

    // Cross-account member credential denied.
    assert!(matches!(
        validate_principal_and_payer(
            &InvocationPrincipal::LocalMember {
                account_id: other,
                provenance: AuthProvenance::ExternalIdp,
            },
            member_payer,
        ),
        Err(InvocationError::MemberAccountMismatch)
    ));
}

#[tokio::test]
async fn stale_fence_boundary_minus_one_on_each_of_three_fences_denies() {
    let dir = TempDir::new().expect("tempdir");
    let store = TursoAdmissionLedgerStore::open(dir.path().join("stale.db"))
        .await
        .expect("open turso");
    let tenant = test_tenant("acme.example");
    let cred = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room = RoomScopeKey {
        scope_id: "room-bva".to_owned(),
    };

    // Advance all three fences from 1 -> 2 and reopen them at gen 2.
    for kind in [
        FenceKind::Credential(cred.clone()),
        FenceKind::RoomOrSpace(room.clone()),
        FenceKind::Placement,
    ] {
        let key = FenceKey {
            tenant_id: tenant.clone(),
            kind,
        };
        let gen = store.close_and_advance_fence(&key).await.expect("close");
        assert_eq!(gen, FenceGeneration::new(2));
        store.reopen_fence(&key, gen).await.expect("reopen");
    }

    let current = store
        .snapshot_fences(&tenant, &cred, &room)
        .await
        .expect("snapshot at gen 2");

    // Stale by -1 on credential fence -> denied.
    let stale_cred = ThreeFenceSnapshot {
        credential: FenceSnapshot {
            generation: FenceGeneration::new(1),
            state: FenceState::Open,
        },
        ..current
    };
    let base_req = |fences, nonce_byte| AdmitAttemptRequest {
        attempt_id: AttemptId::new_v4(),
        run_id: RunId::new_v4(),
        step_id: StepId::new(0),
        tenant_id: tenant.clone(),
        credential: cred.clone(),
        room_or_space: room.clone(),
        holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
        expected_fences: fences,
        invocation_nonce: [nonce_byte; 32],
        expires_at_ms: now_ms() + 60_000,
        reserved_spend_microcents: 10,
        budget: default_budget(),
    };

    assert!(matches!(
        store.admit_attempt(&base_req(stale_cred, 1)).await,
        Err(AdmissionError::FenceStaleOrClosed(FenceKind::Credential(_)))
    ));

    // Stale by -1 on room fence -> denied.
    let stale_room = ThreeFenceSnapshot {
        room_or_space: FenceSnapshot {
            generation: FenceGeneration::new(1),
            state: FenceState::Open,
        },
        ..current
    };
    assert!(matches!(
        store.admit_attempt(&base_req(stale_room, 2)).await,
        Err(AdmissionError::FenceStaleOrClosed(FenceKind::RoomOrSpace(
            _
        )))
    ));

    // Stale by -1 on placement fence -> denied.
    let stale_place = ThreeFenceSnapshot {
        placement: FenceSnapshot {
            generation: FenceGeneration::new(1),
            state: FenceState::Open,
        },
        ..current
    };
    assert!(matches!(
        store.admit_attempt(&base_req(stale_place, 3)).await,
        Err(AdmissionError::FenceStaleOrClosed(FenceKind::Placement))
    ));

    // Exact generation (2, 2, 2) -> admitted!
    store
        .admit_attempt(&base_req(current, 4))
        .await
        .expect("exact gen admitted");
}

#[tokio::test]
async fn budget_overflow_u64_max_cap_and_reservation_fail_closed() {
    let dir = TempDir::new().expect("tempdir");
    let store = TursoAdmissionLedgerStore::open(dir.path().join("overflow.db"))
        .await
        .expect("open turso");
    let tenant = test_tenant("acme.example");
    let cred = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room = RoomScopeKey {
        scope_id: "room-overflow".to_owned(),
    };
    let snap = store
        .snapshot_fences(&tenant, &cred, &room)
        .await
        .expect("snapshot");
    let run_id = RunId::new_v4();
    let max_budget = BudgetLimits {
        max_input_tokens: u64::MAX,
        max_output_tokens: u64::MAX,
        max_spend_microcents: u64::MAX,
        max_steps: 4,
    };

    // Reserve u64::MAX - 10 succeeds cleanly without i64 truncation.
    let req1 = AdmitAttemptRequest {
        attempt_id: AttemptId::new_v4(),
        run_id,
        step_id: StepId::new(0),
        tenant_id: tenant.clone(),
        credential: cred.clone(),
        room_or_space: room.clone(),
        holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
        expected_fences: snap,
        invocation_nonce: [11u8; 32],
        expires_at_ms: now_ms() + 60_000,
        reserved_spend_microcents: u64::MAX - 10,
        budget: max_budget,
    };
    store
        .admit_attempt(&req1)
        .await
        .expect("reserve near u64::MAX");

    // Adding 11 overflows u64::MAX -> must fail closed with BudgetExceeded!
    let req2 = AdmitAttemptRequest {
        attempt_id: AttemptId::new_v4(),
        run_id,
        step_id: StepId::new(1),
        tenant_id: tenant.clone(),
        credential: cred,
        room_or_space: room,
        holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
        expected_fences: snap,
        invocation_nonce: [12u8; 32],
        expires_at_ms: now_ms() + 60_000,
        reserved_spend_microcents: 11,
        budget: max_budget,
    };
    assert!(matches!(
        store.admit_attempt(&req2).await,
        Err(AdmissionError::BudgetExceeded)
    ));
}

#[tokio::test]
async fn quarantine_unknown_after_send_transitions_and_blocks_redispatch() {
    let dir = TempDir::new().expect("tempdir");
    let db_path = dir.path().join("quarantine.db");
    let store = TursoAdmissionLedgerStore::open(&db_path)
        .await
        .expect("open turso");
    let tenant = test_tenant("acme.example");
    let cred = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room = RoomScopeKey {
        scope_id: "room-q".to_owned(),
    };
    let snap = store
        .snapshot_fences(&tenant, &cred, &room)
        .await
        .expect("snap");
    let attempt_id = AttemptId::new_v4();
    let run_id = RunId::new_v4();
    let nonce = [77u8; 32];
    let req = AdmitAttemptRequest {
        attempt_id,
        run_id,
        step_id: StepId::new(0),
        tenant_id: tenant.clone(),
        credential: cred,
        room_or_space: room,
        holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
        expected_fences: snap,
        invocation_nonce: nonce,
        expires_at_ms: now_ms() + 60_000,
        reserved_spend_microcents: 400_000,
        budget: default_budget(),
    };
    store.admit_attempt(&req).await.expect("admit");
    store
        .reconcile_attempt(attempt_id, AttemptOutcome::UnknownAfterSend)
        .await
        .expect("quarantine");

    // Reopen Turso WAL file from disk to verify persistence across process restart.
    drop(store);
    let reopened = TursoAdmissionLedgerStore::open(&db_path)
        .await
        .expect("reopen turso");
    let rec = reopened
        .get_attempt_by_nonce(&tenant, &nonce)
        .await
        .expect("query by nonce")
        .expect("record exists");
    assert_eq!(rec.status, AttemptStatus::Quarantined);
    assert_eq!(rec.actual_spend_microcents, 400_000);

    // Replaying the same nonce returns Err(AdmissionError::Quarantined).
    let mut replay = req.clone();
    replay.attempt_id = AttemptId::new_v4();
    replay.step_id = StepId::new(1);
    assert!(matches!(
        reopened.admit_attempt(&replay).await,
        Err(AdmissionError::Quarantined)
    ));
}

#[tokio::test]
async fn quarantine_budget_conserves_reserved_spend_conservatively() {
    let store = SqliteCompatAdmissionLedgerStore::in_memory()
        .await
        .expect("sqlite compat");
    let tenant = test_tenant("acme.example");
    let cred = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room = RoomScopeKey {
        scope_id: "room-qb".to_owned(),
    };
    let snap = store
        .snapshot_fences(&tenant, &cred, &room)
        .await
        .expect("snap");
    let run_id = RunId::new_v4();
    let budget = BudgetLimits {
        max_input_tokens: 10_000,
        max_output_tokens: 10_000,
        max_spend_microcents: 500,
        max_steps: 4,
    };
    let attempt_id = AttemptId::new_v4();
    store
        .admit_attempt(&AdmitAttemptRequest {
            attempt_id,
            run_id,
            step_id: StepId::new(0),
            tenant_id: tenant.clone(),
            credential: cred.clone(),
            room_or_space: room.clone(),
            holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            expected_fences: snap,
            invocation_nonce: [81u8; 32],
            expires_at_ms: now_ms() + 60_000,
            reserved_spend_microcents: 350,
            budget,
        })
        .await
        .expect("admit");
    store
        .reconcile_attempt(attempt_id, AttemptOutcome::UnknownAfterSend)
        .await
        .expect("quarantine");

    let run = store
        .get_run_budget(run_id)
        .await
        .expect("run")
        .expect("exists");
    assert_eq!(run.spent_microcents, 350);
    assert_eq!(run.reserved_microcents, 0);

    // Attempting to reserve 151 (> remaining 150) fails closed.
    assert!(matches!(
        store
            .admit_attempt(&AdmitAttemptRequest {
                attempt_id: AttemptId::new_v4(),
                run_id,
                step_id: StepId::new(1),
                tenant_id: tenant,
                credential: cred,
                room_or_space: room,
                holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
                expected_fences: snap,
                invocation_nonce: [82u8; 32],
                expires_at_ms: now_ms() + 60_000,
                reserved_spend_microcents: 151,
                budget,
            })
            .await,
        Err(AdmissionError::BudgetExceeded)
    ));
}

#[tokio::test]
async fn pairwise_quarantine_budget_conservation_no_double_spend() {
    let dir = TempDir::new().expect("tempdir");
    let store = TursoAdmissionLedgerStore::open(dir.path().join("qcons.db"))
        .await
        .expect("turso");
    let tenant = test_tenant("acme.example");
    let cred = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room = RoomScopeKey {
        scope_id: "room-cons".to_owned(),
    };
    let snap = store
        .snapshot_fences(&tenant, &cred, &room)
        .await
        .expect("snap");
    let run_id = RunId::new_v4();
    let budget = BudgetLimits {
        max_input_tokens: 10_000,
        max_output_tokens: 10_000,
        max_spend_microcents: 1_000,
        max_steps: 5,
    };
    let attempt_id = AttemptId::new_v4();
    store
        .admit_attempt(&AdmitAttemptRequest {
            attempt_id,
            run_id,
            step_id: StepId::new(0),
            tenant_id: tenant.clone(),
            credential: cred.clone(),
            room_or_space: room.clone(),
            holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            expected_fences: snap,
            invocation_nonce: [61u8; 32],
            expires_at_ms: now_ms() + 60_000,
            reserved_spend_microcents: 600,
            budget,
        })
        .await
        .expect("admit");
    store
        .reconcile_attempt(attempt_id, AttemptOutcome::UnknownAfterSend)
        .await
        .expect("quarantine");

    // Re-reconciling a quarantined attempt fails with Quarantined and never releases budget.
    assert!(matches!(
        store
            .reconcile_attempt(attempt_id, AttemptOutcome::FailedBeforeSend)
            .await,
        Err(AdmissionError::Quarantined)
    ));

    let run = store
        .get_run_budget(run_id)
        .await
        .expect("budget")
        .expect("exists");
    assert_eq!(run.spent_microcents, 600);
    assert_eq!(run.reserved_microcents, 0);
}

#[tokio::test]
async fn pairwise_revoke_inflight_authz_barrier_and_pobserve_trace() {
    let dir = TempDir::new().expect("tempdir");
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("barrier_revoke.db"))
            .await
            .expect("open turso"),
    );
    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    let tenant = test_tenant("acme.example");
    perms.provision_tenant_store(&tenant).await.expect("prov");

    let account = AccountId::new_v4();
    let cred_id = Uuid::new_v4();
    let cred_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::UserCredential,
            id: cred_id.to_string(),
        },
        relation: AuthzRelation::new("owner"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: account.to_string(),
            relation: None,
        },
    };
    perms
        .write_tuples(
            &tenant,
            AuthzModule::Inference,
            std::slice::from_ref(&cred_tuple),
        )
        .await
        .expect("grant");

    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(100)));
    let fence = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn waddle_broker::admission::ReplicaFenceStore>,
        clock,
    ));
    fence.tick_once().await.expect("tick");

    let device_keys = Arc::new(DeviceKeyRegistry::new());
    let (dev_id, signing_key) = enroll_test_device(&device_keys, &tenant, account);
    let trace = TraceRecorder::new();

    // Deterministic barriers:
    // 1. `before_check_barrier` ensures dispatch has snapshotted (1, 1, 1) Open.
    // 2. `after_check_barrier` ensures dispatch passed PermissionBackend::check (Allowed),
    //    then revocation closes & advances the credential fence to gen 2 BEFORE admit_attempt!
    let before_check_barrier = Arc::new(Barrier::new(2));
    let after_check_barrier = Arc::new(Barrier::new(2));

    let svc_dispatch = Arc::new(
        BrokerAdmissionService::new(
            "broker-dispatch",
            Arc::clone(&ledger),
            Arc::clone(&perms) as Arc<dyn PermissionBackend>,
            Arc::clone(&device_keys),
            Arc::clone(&fence),
            trace.clone(),
        )
        .with_barrier_hooks(AdmissionBarrierHooks {
            before_authz_check: Some(Arc::clone(&before_check_barrier)),
            after_authz_check: Some(Arc::clone(&after_check_barrier)),
        }),
    );
    let svc_revoke = Arc::new(BrokerAdmissionService::new(
        "broker-revoke",
        Arc::clone(&ledger),
        Arc::clone(&perms) as Arc<dyn PermissionBackend>,
        Arc::clone(&device_keys),
        Arc::clone(&fence),
        trace.clone(),
    ));

    let room = RoomScopeKey {
        scope_id: "room-race".to_owned(),
    };
    let payer = PayerSelection::Member {
        account_id: account,
        credential_id: cred_id,
    };
    let nonce = [55u8; 32];
    let exp = now_ms() + 60_000;
    let signed = build_signed_invocation(SignedInvocationParams {
        tenant: &tenant,
        account_id: account,
        device_key_id: dev_id,
        signing_key: &signing_key,
        room: &room,
        payer,
        nonce,
        expires_at_ms: exp,
        budget: default_budget(),
    });

    let dispatch_task = {
        let svc = Arc::clone(&svc_dispatch);
        let tenant_c = tenant.clone();
        let room_c = room.clone();
        tokio::spawn(async move {
            svc.admit_invocation(AdmitInvocationInput {
                attempt_id: AttemptId::new_v4(),
                run_id: RunId::new_v4(),
                step_id: StepId::new(0),
                tenant_id: tenant_c,
                principal: InvocationPrincipal::LocalMember {
                    account_id: account,
                    provenance: AuthProvenance::ExternalIdp,
                },
                payer,
                room_or_space: room_c,
                holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
                invocation_nonce: nonce,
                expires_at_ms: exp,
                reserved_spend_microcents: 100,
                budget: default_budget(),
                signed_invocation: Some(signed),
                now_ms: now_ms(),
            })
            .await
        })
    };

    // Step 1: Dispatch has snapshotted gen 1 (Open) and is about to run PermissionBackend::check.
    before_check_barrier.wait().await;

    // Step 2: Close & advance the credential fence (tuple still exists so check returns Allowed
    // and reaches after_check_barrier).
    let fence_key = FenceKey {
        tenant_id: tenant.clone(),
        kind: FenceKind::Credential(payer.credential_key()),
    };
    let new_gen = ledger
        .close_and_advance_fence(&fence_key)
        .await
        .expect("close fence");
    assert_eq!(new_gen, FenceGeneration::new(2));

    // Step 3: Release dispatch from after_check_barrier into admit_attempt -> must fail closed!
    after_check_barrier.wait().await;

    let dispatch_res = dispatch_task.await.expect("join dispatch");
    assert!(
        matches!(
            dispatch_res,
            Err(AdmissionServiceError::Ledger(
                AdmissionError::FenceStaleOrClosed(FenceKind::Credential(_))
            ))
        ),
        "mid-check revocation MUST fail closed at admit_attempt, got err={:?}",
        dispatch_res.as_ref().err()
    );

    // Complete revocation mutation + replica ack.
    let perms_rev = Arc::clone(&perms);
    let tenant_rev = tenant.clone();
    let _ = svc_revoke
        .restrict_fence(&fence_key, &[], false, move || async move {
            perms_rev
                .delete_tuples(&tenant_rev, AuthzModule::Inference, &[cred_tuple])
                .await
        })
        .await
        .expect("complete restrict");

    let jsonl = trace.to_jsonl();
    assert_secret_free(&jsonl, &["sk-test-secret"]).expect("secret-free");
    verify_jsonl_trace(&jsonl).expect("PObserve trace valid");
}

#[tokio::test]
async fn pairwise_tenant_deletion_cascade() {
    let dir = TempDir::new().expect("tempdir");
    let tenant = test_tenant("doomed.example");

    // 1. Turso per-tenant DB
    let tenant_store =
        TursoTenantStore::new(dir.path().join("tenants"), TursoPoolConfig::default(), None)
            .expect("turso tenant store");
    tenant_store
        .provision_tenant(&tenant)
        .await
        .expect("provision tenant db");
    let _conn = tenant_store
        .open_tenant(&TenantQueryScope {
            tenant_id: tenant.clone(),
            holder: test_holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            placement_generation: FenceGeneration::new(1),
        })
        .await
        .expect("open tenant db");
    let db_path = tenant_store.tenant_db_path(&tenant).expect("db path");
    assert!(db_path.exists());

    // 2. OpenFGA per-tenant store over real HTTP
    let fga_server = InProcessOpenFgaServer::start()
        .await
        .expect("start openfga");
    let fga_backend = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: fga_server.endpoint().to_owned(),
        bearer_token: None,
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("openfga client");
    let store_ref = fga_backend
        .provision_tenant_store(&tenant)
        .await
        .expect("provision openfga store");
    assert!(fga_server.store_exists(&store_ref.store_id));

    // Cell attempting to write inference module to OpenFGA MUST be denied.
    let fga_cell = fga_backend.with_caller_role(AuthzCallerRole::Cell);
    let inf_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::TenantCredential,
            id: Uuid::new_v4().to_string(),
        },
        relation: AuthzRelation::new("tenant_admin"),
        subject: AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: AccountId::new_v4().to_string(),
            relation: None,
        },
    };
    assert!(matches!(
        fga_cell
            .write_tuples(&tenant, AuthzModule::Inference, &[inf_tuple])
            .await,
        Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference
        ))
    ));

    // 3. Broker custody service with KMS + Turso destruction ledger
    let ledger = Arc::new(
        TursoLedgerStore::open_in_dir(dir.path())
            .await
            .expect("open custody ledger"),
    );
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("trust.example", kms));
    let custody = BrokerCustodyService::new(hierarchy, ledger)
        .await
        .expect("init custody");
    let cred = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant.clone(),
            account_id: None,
            provider: "openai".to_owned(),
            plaintext: SecretBytes::new(b"sk-test-secret-doomed-tenant".to_vec()).expect("secret"),
            nonce: "enroll-doomed-1".to_owned(),
            payload_digest_sha256: [0x11; 32],
            expires_at_ms: now_ms() + 60_000,
        })
        .await
        .expect("enroll cred");

    // Cascade delete tenant: Turso WAL purge + OpenFGA DeleteStore + KMS destruction ledger!
    custody
        .delete_tenant(&tenant, &tenant_store, &fga_backend)
        .await
        .expect("cascade delete tenant");

    assert!(!db_path.exists(), "Turso tenant DB must be purged");
    assert!(
        !fga_server.store_exists(&store_ref.store_id),
        "OpenFGA store must be deleted via DeleteStore"
    );
    assert!(
        custody
            .unseal_for_egress(&tenant, cred.credential_id)
            .await
            .is_err(),
        "credential must be permanently destroyed"
    );
    assert!(
        fga_backend
            .check(&CheckRequest {
                tenant_id: tenant.clone(),
                object: AuthzObject {
                    object_type: AuthzObjectType::Space,
                    id: "s1".to_owned(),
                },
                permission: AuthzRelation::new("read"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: "u1".to_owned(),
                    relation: None,
                },
                consistency: AuthzConsistency::Strong,
            })
            .await
            .is_err(),
        "deleted tenant OpenFGA store must fail closed"
    );
}

#[tokio::test]
async fn e2e_gate2_dual_broker_race_single_provider_dispatch_and_pobserve() {
    let dir = TempDir::new().expect("tempdir");
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("dual_broker.db"))
            .await
            .expect("open turso"),
    );
    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    let tenant = test_tenant("acme.example");
    perms.provision_tenant_store(&tenant).await.expect("prov");

    let account = AccountId::new_v4();
    let cred_id = Uuid::new_v4();
    perms
        .write_tuples(
            &tenant,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::UserCredential,
                    id: cred_id.to_string(),
                },
                relation: AuthzRelation::new("owner"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: account.to_string(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("grant");

    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(50)));
    let fence_a = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn waddle_broker::admission::ReplicaFenceStore>,
        Arc::clone(&clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
    ));
    let fence_b = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn waddle_broker::admission::ReplicaFenceStore>,
        Arc::clone(&clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
    ));
    fence_a.tick_once().await.expect("tick a");
    fence_b.tick_once().await.expect("tick b");

    let device_keys = Arc::new(DeviceKeyRegistry::new());
    let (dev_id, signing_key) = enroll_test_device(&device_keys, &tenant, account);
    let trace = TraceRecorder::new();

    // Synchronize both broker replicas right before admit_attempt!
    let race_barrier = Arc::new(Barrier::new(2));
    let svc_a = Arc::new(
        BrokerAdmissionService::new(
            "broker-replica-a",
            Arc::clone(&ledger),
            Arc::clone(&perms) as Arc<dyn PermissionBackend>,
            Arc::clone(&device_keys),
            Arc::clone(&fence_a),
            trace.clone(),
        )
        .with_barrier_hooks(AdmissionBarrierHooks {
            before_authz_check: None,
            after_authz_check: Some(Arc::clone(&race_barrier)),
        }),
    );
    let svc_b = Arc::new(
        BrokerAdmissionService::new(
            "broker-replica-b",
            Arc::clone(&ledger),
            Arc::clone(&perms) as Arc<dyn PermissionBackend>,
            Arc::clone(&device_keys),
            Arc::clone(&fence_b),
            trace.clone(),
        )
        .with_barrier_hooks(AdmissionBarrierHooks {
            before_authz_check: None,
            after_authz_check: Some(Arc::clone(&race_barrier)),
        }),
    );

    let room = RoomScopeKey {
        scope_id: "room-dual".to_owned(),
    };
    let payer = PayerSelection::Member {
        account_id: account,
        credential_id: cred_id,
    };
    let shared_nonce = [0xEE; 32];
    let shared_run = RunId::new_v4();
    let exp = now_ms() + 60_000;
    let signed = build_signed_invocation(SignedInvocationParams {
        tenant: &tenant,
        account_id: account,
        device_key_id: dev_id,
        signing_key: &signing_key,
        room: &room,
        payer,
        nonce: shared_nonce,
        expires_at_ms: exp,
        budget: default_budget(),
    });

    let provider_dispatches = Arc::new(AtomicUsize::new(0));
    let make_input = |holder_sa: &str| AdmitInvocationInput {
        attempt_id: AttemptId::new_v4(),
        run_id: shared_run,
        step_id: StepId::new(0),
        tenant_id: tenant.clone(),
        principal: InvocationPrincipal::LocalMember {
            account_id: account,
            provenance: AuthProvenance::ExternalIdp,
        },
        payer,
        room_or_space: room.clone(),
        holder: test_holder(holder_sa),
        invocation_nonce: shared_nonce,
        expires_at_ms: exp,
        reserved_spend_microcents: 200_000,
        budget: default_budget(),
        signed_invocation: Some(signed.clone()),
        now_ms: now_ms(),
    };

    let input_a = make_input("spiffe://trust.example/ns/waddle/sa/cell-a");
    let input_b = make_input("spiffe://trust.example/ns/waddle/sa/cell-b");

    let d1 = Arc::clone(&provider_dispatches);
    let t1 = tokio::spawn(async move {
        let res = svc_a.admit_invocation(input_a).await;
        if res.is_ok() {
            d1.fetch_add(1, Ordering::SeqCst);
        }
        res
    });
    let d2 = Arc::clone(&provider_dispatches);
    let t2 = tokio::spawn(async move {
        let res = svc_b.admit_invocation(input_b).await;
        if res.is_ok() {
            d2.fetch_add(1, Ordering::SeqCst);
        }
        res
    });

    let (r1, r2) = tokio::join!(t1, t2);
    let r1 = r1.expect("join t1");
    let r2 = r2.expect("join t2");

    // Exactly ONE broker replica wins admission and dispatches to the provider!
    assert_eq!(
        provider_dispatches.load(Ordering::SeqCst),
        1,
        "dual broker race must produce at most 1 provider dispatch (r1={:?}, r2={:?})",
        r1.as_ref().err(),
        r2.as_ref().err()
    );
    assert!(r1.is_ok() ^ r2.is_ok());

    let jsonl = trace.to_jsonl();
    assert_secret_free(&jsonl, &["sk-test-secret"]).expect("secret-free");
    verify_jsonl_trace(&jsonl).expect("PObserve trace valid");
}
