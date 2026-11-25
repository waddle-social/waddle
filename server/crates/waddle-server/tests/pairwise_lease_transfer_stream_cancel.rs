//! T3-02: Pairwise (F10 + F15 + F18) — PlacementLease transfer closes the
//! placement fence, triggers `BrokerReplicaFence` stream cancellation, and
//! synchronously cancels the active `InferenceStream` via `Drop`.

use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use uuid::Uuid;
use waddle_broker::admission::invocation::{InvocationPrincipal, PayerSelection};
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::service::{
    AdmitInvocationInput, BrokerAdmissionService, TraceRecorder,
};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::ReplicaFenceStore;
use waddle_broker::authz::local::LocalPermissionBackend;
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::idp_proof::{AuthProvenance, DeviceKeyRegistry};
use waddle_broker_core::admission::{
    AttemptId, BudgetLimits, FenceKey, FenceKind, RoomScopeKey, RunId, StepId,
};
use waddle_broker_core::boot_clock::FakeBootClock;
use waddle_broker_core::identity::{
    AccountId, CellRole, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    AuthzTuple, PermissionBackend,
};
use waddle_broker_core::placement::{PlacementLeaseStore, PlacementMode};
use waddle_broker_core::trace_verifier::{verify_jsonl_trace, TraceEvent};
use waddle_extensions::runtime::inference::{HostInferenceChunk, InferenceStream, StreamItem};
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;

fn holder(sa: &str, key_byte: u8) -> HolderInstance {
    HolderInstance {
        spiffe_id: SpiffeId::parse(sa).expect("spiffe"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([key_byte; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![key_byte; 32]).expect("proof"),
    }
}

#[tokio::test]
async fn pairwise_lease_transfer_closes_placement_fence_and_drops_active_stream() {
    let dir = TempDir::new().expect("tempdir");
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(10)));

    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("leases.db"))
            .await
            .expect("lease store"),
    );
    let role_a = CellRole::parse("cell-shared").expect("role a");
    let holder_a = holder("spiffe://trust.example/ns/waddle/sa/cell-shared", 1);
    let holder_b = holder("spiffe://trust.example/ns/waddle/sa/cell-shared", 2);
    let lease_a = lease_store
        .claim(
            &tenant,
            &role_a,
            PlacementMode::SharedPool,
            &holder_a,
            30_000,
        )
        .await
        .expect("claim lease a");

    let ledger = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("admission.db"))
            .await
            .expect("ledger"),
    );
    let perms = Arc::new(LocalPermissionBackend::new(AuthzCallerRole::Broker));
    perms
        .provision_tenant_store(&tenant)
        .await
        .expect("provision");
    let account = AccountId::new_v4();
    let tenant_cred_id = Uuid::new_v4();
    perms
        .write_tuples(
            &tenant,
            AuthzModule::Inference,
            &[AuthzTuple {
                object: AuthzObject {
                    object_type: AuthzObjectType::TenantCredential,
                    id: tenant_cred_id.to_string(),
                },
                relation: AuthzRelation::new("use"),
                subject: AuthzSubject {
                    subject_type: AuthzSubjectType::Account,
                    id: account.to_string(),
                    relation: None,
                },
            }],
        )
        .await
        .expect("tuple");

    let fence_a = Arc::new(BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&ledger) as Arc<dyn ReplicaFenceStore>,
        Arc::clone(&clock) as Arc<dyn waddle_broker_core::boot_clock::BootClock>,
    ));
    fence_a.tick_once().await.expect("tick a");
    let trace = TraceRecorder::new();
    let svc = BrokerAdmissionService::new(
        "broker-1",
        Arc::clone(&ledger),
        perms,
        Arc::new(DeviceKeyRegistry::new()),
        Arc::clone(&fence_a),
        trace.clone(),
    );

    let admitted = svc
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
            payer: PayerSelection::Tenant {
                credential_id: tenant_cred_id,
            },
            room_or_space: RoomScopeKey {
                scope_id: "general".to_owned(),
            },
            holder: holder_a.clone(),
            command_digest_sha256: [0xCD; 32],
            invocation_nonce: [0x55; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            reserved_spend_microcents: 500,
            budget: BudgetLimits {
                max_input_tokens: 4_000,
                max_output_tokens: 1_000,
                max_spend_microcents: 5_000,
                max_steps: 2,
            },
            signed_invocation: None,
            now_ms: Utc::now().timestamp_millis(),
        })
        .await
        .expect("admitted");

    let (mut stream, tx, upstream_cancel_rx, _cancel_handle) =
        InferenceStream::with_bounded_channel(4, 1024);
    tx.send(StreamItem::Chunk(HostInferenceChunk {
        delta: b"hello".to_vec(),
        sequence: 0,
        finish_reason: None,
    }))
    .await
    .expect("chunk 0");
    assert_eq!(
        stream
            .next_chunk()
            .await
            .expect("chunk")
            .expect("some")
            .delta,
        b"hello"
    );

    trace.emit(
        "broker-1",
        &tenant,
        TraceEvent::StreamOpen {
            stream_id: "s-lease-1".to_owned(),
            permits_after: 1,
            max_permits: 4,
        },
    );

    // Bridge admitted.cancel_rx to drop the InferenceStream when the placement fence closes.
    let mut admit_cancel_rx = admitted.cancel_rx.clone();
    let stream_task = tokio::spawn(async move {
        let _stream = stream;
        let _ = admit_cancel_rx.changed().await;
        // `_stream` is dropped synchronously here when placement fence closes.
    });

    // Lease transfer closes the placement fence first, mutates the lease, ticks fence_a, and awaits ack.
    let placement_fence_key = FenceKey {
        tenant_id: tenant.clone(),
        kind: FenceKind::Placement,
    };
    let lease_store_c = Arc::clone(&lease_store);
    let tenant_c = tenant.clone();
    let holder_b_c = holder_b.clone();
    let holder_a_c = holder_a.clone();
    let new_place_gen = svc
        .restrict_fence(&placement_fence_key, &[], true, move || async move {
            let transferred = lease_store_c
                .transfer(
                    &tenant_c,
                    &holder_a_c,
                    &holder_b_c,
                    lease_a.generation,
                    30_000,
                )
                .await
                .map_err(|e| {
                    waddle_broker_core::permission::PermissionBackendError::Backend(e.to_string())
                })?;
            assert!(transferred.generation > lease_a.generation);
            Ok(())
        })
        .await
        .expect("restrict placement fence and transfer lease");
    assert_eq!(new_place_gen.get(), 2);
    assert!(*admitted.cancel_rx.borrow());

    stream_task.await.expect("stream task exits");
    assert!(
        *upstream_cancel_rx.borrow(),
        "InferenceStream Drop must cancel upstream provider on lease transfer"
    );

    trace.emit(
        "broker-1",
        &tenant,
        TraceEvent::StreamCancelReq {
            stream_id: "s-lease-1".to_owned(),
            tick: 1,
        },
    );
    trace.emit(
        "broker-1",
        &tenant,
        TraceEvent::StreamAbortDone {
            stream_id: "s-lease-1".to_owned(),
            tick: 2,
            permits_after: 0,
        },
    );
    let summary =
        verify_jsonl_trace(&trace.to_jsonl()).expect("PObserve trace must verify cleanly");
    assert_eq!(summary.streams_opened, 1);
    let _ = mpsc::channel::<u8>(1);
}
