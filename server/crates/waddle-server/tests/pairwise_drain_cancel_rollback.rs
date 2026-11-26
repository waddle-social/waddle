//! T3-11: Pairwise (F10 + F18 + F21) — Graceful drain (`WADDLE_DRAIN_TIMEOUT_SECS`)
//! cancels in-flight inference streams synchronously, and `RollbackController`
//! disables admission, advances fences, and rejects plaintext credential restore
//! or fence removal.

use std::sync::Arc;
use std::time::Duration;

use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::rollback::{
    RollbackController, RollbackError, RollbackScope, RollbackTarget,
};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::ReplicaFenceStore;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker_core::admission::{
    AdmissionLedgerStore, AttemptId, CredentialKey, CredentialScope, FenceKey, FenceKind,
    FenceState, RoomScopeKey,
};
use waddle_broker_core::boot_clock::SystemBootClock;
use waddle_broker_core::identity::TenantId;
use waddle_ecdysis::GracefulShutdown;
use waddle_extensions::runtime::inference::{HostInferenceChunk, InferenceStream, StreamItem};
use waddle_tenancy::quiesce::drain_timeout_from_env;

#[tokio::test]
async fn pairwise_drain_cancels_stream_and_rollback_closes_fences_fail_closed() {
    let timeout = drain_timeout_from_env();
    let shutdown = GracefulShutdown::new(timeout);
    let stop_token = shutdown.stop_token();

    let (mut stream, tx, upstream_cancel_rx, _cancel_handle) =
        InferenceStream::with_bounded_channel(4, 1024);
    tx.send(StreamItem::Chunk(HostInferenceChunk {
        delta: b"token-0".to_vec(),
        sequence: 0,
        finish_reason: None,
    }))
    .await
    .expect("chunk 0");
    assert_eq!(
        stream
            .next_chunk()
            .await
            .expect("next_chunk")
            .expect("chunk 0")
            .delta,
        b"token-0"
    );

    let stream_task = tokio::spawn(async move {
        let _held = stream;
        stop_token.cancelled().await;
    });

    // Trigger server drain shutdown -> stream drops -> upstream_cancel_rx notifies synchronously.
    shutdown.trigger_stop();
    tokio::time::timeout(Duration::from_secs(2), stream_task)
        .await
        .expect("drain join timeout")
        .expect("stream task");
    assert!(
        *upstream_cancel_rx.borrow(),
        "graceful drain stop must drop InferenceStream and cancel upstream"
    );

    // Verify RollbackController closes credential fence, cancels replica streams, and blocks unsafe rollback recovery.
    let dir = TempDir::new().expect("tempdir");
    let admission = Arc::new(
        TursoAdmissionLedgerStore::open(dir.path().join("admission.db"))
            .await
            .expect("open admission"),
    );
    let ledger_dir = dir.path().join("ledger");
    std::fs::create_dir_all(&ledger_dir).expect("ledger dir");
    let ledger = TursoLedgerStore::open_in_dir(&ledger_dir)
        .await
        .expect("open ledger");
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let cred_key = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room_key = RoomScopeKey {
        scope_id: "general@muc.acme.example".to_owned(),
    };
    let before = admission
        .snapshot_fences(&tenant, &cred_key, &room_key)
        .await
        .expect("snapshot before rollback");

    let replica_fence = BrokerReplicaFence::new(
        Uuid::new_v4(),
        Arc::clone(&admission) as Arc<dyn ReplicaFenceStore>,
        Arc::new(SystemBootClock),
    );
    replica_fence.tick_once().await.expect("tick");
    let replica_stream_cancel = replica_fence
        .register_stream(
            AttemptId::new_v4(),
            [
                (
                    FenceKey {
                        tenant_id: tenant.clone(),
                        kind: FenceKind::Credential(cred_key.clone()),
                    },
                    before.credential.generation,
                ),
                (
                    FenceKey {
                        tenant_id: tenant.clone(),
                        kind: FenceKind::RoomOrSpace(room_key.clone()),
                    },
                    before.room_or_space.generation,
                ),
                (
                    FenceKey {
                        tenant_id: tenant.clone(),
                        kind: FenceKind::Placement,
                    },
                    before.placement.generation,
                ),
            ],
        )
        .expect("register replica stream");

    let controller = RollbackController::new();
    let rollback_cancel = CancellationToken::new();
    let report = controller
        .execute_rollback(
            RollbackScope::ProviderProfile {
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "openrouter/free".to_owned(),
            },
            &[FenceKey {
                tenant_id: tenant.clone(),
                kind: FenceKind::Credential(cred_key.clone()),
            }],
            RollbackTarget {
                admission_store: admission.as_ref(),
                replica_fence: Some(&replica_fence),
                destruction_ledger: &ledger,
                cancel_token: Some(&rollback_cancel),
            },
        )
        .await
        .expect("execute rollback");

    assert!(report.stream_drain_signaled);
    assert!(rollback_cancel.is_cancelled());
    assert!(*replica_stream_cancel.borrow());
    assert_eq!(
        controller.check_admissible(&tenant, "openrouter", "default", "openrouter/free"),
        Err(RollbackError::AdmissionDisabled)
    );
    let after = admission
        .snapshot_fences(&tenant, &cred_key, &room_key)
        .await
        .expect("snapshot after rollback");
    assert_eq!(after.credential.state, FenceState::Closed);
    assert_eq!(
        controller.reject_plaintext_credential_restore(),
        Err(RollbackError::PlaintextCredentialRestoreForbidden)
    );
    assert_eq!(
        controller.reject_fence_removal(),
        Err(RollbackError::FenceRemovalForbidden)
    );
    let _ = mpsc::channel::<u8>(1);
}
