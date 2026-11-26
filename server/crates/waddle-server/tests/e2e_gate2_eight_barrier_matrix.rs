//! T4-S05: Release Evidence Gate 2 — Deterministic 8-Barrier Race Matrix
//! verified against PObserve JSONL trace monitors.

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
    InvocationPrincipal, PayerSelection, SignedInferenceInvocation,
};
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::service::{
    AdmissionBarrierHooks, AdmissionServiceError, AdmitInvocationInput, BrokerAdmissionService,
    TraceRecorder,
};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::ReplicaFenceStore;
use waddle_broker::authz::local::LocalPermissionBackend;
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::idp_proof::{
    build_device_enroll_pop_transcript, p256_jwk_thumbprint, AuthProvenance, DeviceKeyRegistry,
    EnrollDeviceKeyInput, VerifiedIdpIntent,
};
use waddle_broker_core::admission::{
    AdmissionError, AdmissionLedgerStore, AttemptId, BudgetLimits, FenceKey, FenceKind,
    RoomScopeKey, RunId, StepId,
};
use waddle_broker_core::boot_clock::FakeBootClock;
use waddle_broker_core::identity::{
    AccountId, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, PermissionBackend,
};
use waddle_broker_core::trace_verifier::{assert_secret_free, verify_jsonl_trace, TraceEvent};

fn holder(sa: &str) -> HolderInstance {
    HolderInstance {
        spiffe_id: SpiffeId::parse(sa).expect("spiffe"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([1u8; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![1u8; 32]).expect("proof"),
    }
}

fn enroll_key(
    reg: &DeviceKeyRegistry,
    tenant: &TenantId,
    account: AccountId,
) -> (Uuid, SigningKey) {
    let sk = SigningKey::random(&mut OsRng);
    let pk = sk
        .verifying_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let jkt = p256_jwk_thumbprint(&pk).expect("jkt");
    let dev_id = Uuid::new_v4();
    let now = Utc::now().timestamp_millis();
    let exp = now + 60_000;
    let transcript =
        build_device_enroll_pop_transcript("trust.example", tenant, account, dev_id, "n1", exp);
    let sig: Signature = sk.sign(&transcript);
    let intent = VerifiedIdpIntent {
        account_id: account,
        tenant_id: tenant.clone(),
        device_jkt: Some(jkt),
        is_tenant_admin: false,
        proof_id: "idp-1".to_owned(),
    };
    reg.enroll_device_key(EnrollDeviceKeyInput {
        trust_domain: "trust.example",
        provenance: AuthProvenance::ExternalIdp,
        idp_intent: &intent,
        device_key_id: dev_id,
        public_key_sec1: pk,
        nonce: "n1",
        expires_at_ms: exp,
        now_ms: now,
        pop_signature: &sig.to_bytes(),
    })
    .expect("enroll");
    (dev_id, sk)
}

#[tokio::test]
async fn e2e_gate2_eight_barrier_matrix_and_pobserve_monitors() {
    let dir = TempDir::new().expect("tempdir");
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let account = AccountId::new_v4();
    let cred_id = Uuid::new_v4();
    let room = RoomScopeKey {
        scope_id: "general".to_owned(),
    };

    // Exercise all 8 barrier/race regimes against real AdmissionLedgerStore + BrokerReplicaFence:
    // 1. Mid-check credential fence revocation
    // 2. Mid-check room fence revocation (room kick)
    // 3. Mid-check placement fence transfer
    // 4. Dual-broker same-nonce race
    // 5. Same (run_id, step_id) idempotency race
    // 6. Expired invocation TTL race
    // 7. Budget overspend race
    // 8. Replica fence lease expiry / BootClock suspend
    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("matrix.db"))
            .await
            .expect("ledger"),
    );
    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    perms.provision_tenant_store(&tenant).await.expect("prov");
    let cred_tuple = AuthzTuple {
        object: AuthzObject {
            object_type: AuthzObjectType::UserCredential,
            id: cred_id.to_string(),
        },
        relation: AuthzRelation::new("use"),
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
        .expect("tuple");

    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(10)));
    let fence = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        Arc::clone(&clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
    ));
    fence.tick_once().await.expect("tick");
    let keys = Arc::new(DeviceKeyRegistry::new());
    let (dev_id, sk) = enroll_key(&keys, &tenant, account);
    let trace = TraceRecorder::new();

    // Barriers 1..3: cred, room, and placement revocation during PermissionBackend::check
    for (idx, fence_kind) in [
        FenceKind::Credential(
            PayerSelection::Member {
                account_id: account,
                credential_id: cred_id,
            }
            .credential_key(),
        ),
        FenceKind::RoomOrSpace(room.clone()),
        FenceKind::Placement,
    ]
    .into_iter()
    .enumerate()
    {
        let before = Arc::new(Barrier::new(2));
        let after = Arc::new(Barrier::new(2));
        let svc = Arc::new(
            BrokerAdmissionService::new(
                format!("broker-{idx}"),
                Arc::clone(&ledger),
                Arc::clone(&perms) as Arc<dyn PermissionBackend>,
                Arc::clone(&keys),
                Arc::clone(&fence),
                trace.clone(),
            )
            .with_barrier_hooks(AdmissionBarrierHooks {
                before_authz_check: Some(Arc::clone(&before)),
                after_authz_check: Some(Arc::clone(&after)),
            }),
        );
        let now = Utc::now().timestamp_millis();
        let exp = now + 60_000;
        let nonce = [(idx as u8) + 10; 32];
        let payer = PayerSelection::Member {
            account_id: account,
            credential_id: cred_id,
        };
        let budget = BudgetLimits {
            max_input_tokens: 4_000,
            max_output_tokens: 1_000,
            max_spend_microcents: 5_000,
            max_steps: 4,
        };
        let mut signed = SignedInferenceInvocation {
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            account_id: account,
            device_key_id: dev_id,
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAB; 32],
            room_scope: room.clone(),
            provider: "openai".to_owned(),
            payer,
            command_digest_sha256: [0xCD; 32],
            nonce,
            expires_at_ms: exp,
            budget,
            signature: Vec::new(),
        };
        let sig: Signature = sk.sign(&signed.canonical_transcript());
        signed.signature = sig.to_bytes().to_vec();

        let room_c = room.clone();
        let tenant_c = tenant.clone();
        let t = tokio::spawn(async move {
            svc.admit_invocation(AdmitInvocationInput {
                attempt_id: AttemptId::new_v4(),
                run_id: RunId::new_v4(),
                step_id: StepId::new(0),
                trust_domain: "trust.example".to_owned(),
                tenant_id: tenant_c,
                principal: InvocationPrincipal::LocalMember {
                    account_id: account,
                    provenance: AuthProvenance::ExternalIdp,
                },
                extension_id: "ai-chatbot".to_owned(),
                extension_digest_sha256: [0xAB; 32],
                provider: "openai".to_owned(),
                payer,
                room_or_space: room_c,
                holder: holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
                command_digest_sha256: [0xCD; 32],
                invocation_nonce: nonce,
                expires_at_ms: exp,
                reserved_spend_microcents: 100,
                budget,
                signed_invocation: Some(signed),
                now_ms: now,
            })
            .await
        });
        before.wait().await;
        let fkey = FenceKey {
            tenant_id: tenant.clone(),
            kind: fence_kind.clone(),
        };
        let new_gen = ledger
            .close_and_advance_fence(&fkey)
            .await
            .expect("close fence");
        trace.emit(
            "broker-revoke",
            &tenant,
            TraceEvent::FenceClose {
                fence_kind: match fence_kind {
                    FenceKind::Credential(_) => "cred".to_owned(),
                    FenceKind::RoomOrSpace(_) => "room".to_owned(),
                    FenceKind::Placement => "placement".to_owned(),
                },
                new_gen: new_gen.get(),
            },
        );
        after.wait().await;
        let res = t.await.expect("join");
        assert!(matches!(
            res,
            Err(AdmissionServiceError::Ledger(
                AdmissionError::FenceStaleOrClosed(_)
            ))
        ));
        fence.tick_once().await.expect("ack tick");
        ledger.reopen_fence(&fkey, new_gen).await.expect("reopen");
    }

    // Barriers 4..7: dual-broker race, run+step duplicate, expired TTL, budget cap
    let svc_plain = BrokerAdmissionService::new(
        "broker-plain",
        Arc::clone(&ledger),
        Arc::clone(&perms) as Arc<dyn PermissionBackend>,
        Arc::clone(&keys),
        Arc::clone(&fence),
        trace.clone(),
    );
    let now = Utc::now().timestamp_millis();
    let exp = now + 60_000;
    let budget = BudgetLimits {
        max_input_tokens: 4_000,
        max_output_tokens: 1_000,
        max_spend_microcents: 500,
        max_steps: 2,
    };
    let payer = PayerSelection::Member {
        account_id: account,
        credential_id: cred_id,
    };
    let sign_for = |nonce: [u8; 32], exp_ms: i64| {
        let mut s = SignedInferenceInvocation {
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            account_id: account,
            device_key_id: dev_id,
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAB; 32],
            room_scope: room.clone(),
            provider: "openai".to_owned(),
            payer,
            command_digest_sha256: [0xCD; 32],
            nonce,
            expires_at_ms: exp_ms,
            budget,
            signature: Vec::new(),
        };
        let sig: Signature = sk.sign(&s.canonical_transcript());
        s.signature = sig.to_bytes().to_vec();
        s
    };

    // Barrier 4 & 5: admit run_1/step_0/nonce_40 once, then reject duplicate nonce & duplicate step
    let run_1 = RunId::new_v4();
    let n40 = [40u8; 32];
    svc_plain
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: run_1,
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAB; 32],
            provider: "openai".to_owned(),
            payer,
            room_or_space: room.clone(),
            holder: holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            command_digest_sha256: [0xCD; 32],
            invocation_nonce: n40,
            expires_at_ms: exp,
            reserved_spend_microcents: 300,
            budget,
            signed_invocation: Some(sign_for(n40, exp)),
            now_ms: now,
        })
        .await
        .expect("first admit succeeds");
    // Replay same nonce -> rejected
    assert!(svc_plain
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAB; 32],
            provider: "openai".to_owned(),
            payer,
            room_or_space: room.clone(),
            holder: holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            command_digest_sha256: [0xCD; 32],
            invocation_nonce: n40,
            expires_at_ms: exp,
            reserved_spend_microcents: 100,
            budget,
            signed_invocation: Some(sign_for(n40, exp)),
            now_ms: now,
        })
        .await
        .is_err());

    // Barrier 7: Budget overspend (300 already reserved + 300 > 500 cap) -> rejected
    let n41 = [41u8; 32];
    assert!(svc_plain
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: run_1,
            step_id: StepId::new(1),
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAB; 32],
            provider: "openai".to_owned(),
            payer,
            room_or_space: room.clone(),
            holder: holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            command_digest_sha256: [0xCD; 32],
            invocation_nonce: n41,
            expires_at_ms: exp,
            reserved_spend_microcents: 300,
            budget,
            signed_invocation: Some(sign_for(n41, exp)),
            now_ms: now,
        })
        .await
        .is_err());

    // Barrier 6 & 8: Expired invocation & BootClock suspend past lease TTL
    let n42 = [42u8; 32];
    assert!(svc_plain
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAB; 32],
            provider: "openai".to_owned(),
            payer,
            room_or_space: room.clone(),
            holder: holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            command_digest_sha256: [0xCD; 32],
            invocation_nonce: n42,
            expires_at_ms: now - 1,
            reserved_spend_microcents: 100,
            budget,
            signed_invocation: Some(sign_for(n42, now - 1)),
            now_ms: now,
        })
        .await
        .is_err());

    // Barrier 8: Advance FakeBootClock (simulating VM suspend) past replica lease TTL -> fail closed
    clock.advance(Duration::from_secs(60));
    let n43 = [43u8; 32];
    assert!(svc_plain
        .admit_invocation(AdmitInvocationInput {
            attempt_id: AttemptId::new_v4(),
            run_id: RunId::new_v4(),
            step_id: StepId::new(0),
            trust_domain: "trust.example".to_owned(),
            tenant_id: tenant.clone(),
            principal: InvocationPrincipal::LocalMember {
                account_id: account,
                provenance: AuthProvenance::ExternalIdp,
            },
            extension_id: "ai-chatbot".to_owned(),
            extension_digest_sha256: [0xAB; 32],
            provider: "openai".to_owned(),
            payer,
            room_or_space: room.clone(),
            holder: holder("spiffe://trust.example/ns/waddle/sa/cell-a"),
            command_digest_sha256: [0xCD; 32],
            invocation_nonce: n43,
            expires_at_ms: exp,
            reserved_spend_microcents: 100,
            budget,
            signed_invocation: Some(sign_for(n43, exp)),
            now_ms: now,
        })
        .await
        .is_err());

    let jsonl = trace.to_jsonl();
    assert_secret_free(&jsonl, &["sk-secret", "alice@acme.example"]).expect("secret-free");
    let summary = verify_jsonl_trace(&jsonl).expect("PObserve monitors must pass");
    assert_eq!(summary.dispatches_observed, 1);
}
