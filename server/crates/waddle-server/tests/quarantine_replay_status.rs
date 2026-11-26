//! T1-F16-05: XMPP command/nonce replay after `unknown_after_send` quarantine
//! returns existing `Quarantined` status with 0 additional dispatches and 0
//! additional budget reservations.

use chrono::Utc;
use tempfile::TempDir;
use uuid::Uuid;
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::admission::{AdmissionQueryStore, AttemptStatus};
use waddle_broker_core::admission::{
    AdmissionError, AdmissionLedgerStore, AdmitAttemptRequest, AttemptId, AttemptOutcome,
    BudgetLimits, CredentialKey, CredentialScope, RoomScopeKey, RunId, StepId,
};
use waddle_broker_core::identity::{
    HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};

#[tokio::test]
async fn replayed_command_after_quarantine_returns_existing_status_with_zero_redispatch() {
    let dir = TempDir::new().expect("tempdir");
    let store = TursoAdmissionLedgerStore::open(dir.path().join("replay.db"))
        .await
        .expect("open turso admission store");
    let tenant = TenantId::parse("acme.example").expect("tenant");
    let cred = CredentialKey {
        credential_id: Uuid::new_v4(),
        scope: CredentialScope::Tenant,
    };
    let room = RoomScopeKey {
        scope_id: "room-replay".to_owned(),
    };
    let snap = store
        .snapshot_fences(&tenant, &cred, &room)
        .await
        .expect("snapshot");

    let run_id = RunId::new_v4();
    let attempt_id = AttemptId::new_v4();
    let nonce = [0x42; 32];
    let budget = BudgetLimits {
        max_input_tokens: 8_000,
        max_output_tokens: 2_000,
        max_spend_microcents: 500_000,
        max_steps: 4,
    };
    let holder = HolderInstance {
        spiffe_id: SpiffeId::parse("spiffe://trust.example/ns/waddle/sa/cell-a").expect("spiffe"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([3u8; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![9u8; 32]).expect("proof"),
    };
    let req = AdmitAttemptRequest {
        attempt_id,
        run_id,
        step_id: StepId::new(0),
        tenant_id: tenant.clone(),
        credential: cred,
        room_or_space: room,
        holder,
        expected_fences: snap,
        invocation_nonce: nonce,
        expires_at_ms: Utc::now().timestamp_millis() + 60_000,
        reserved_spend_microcents: 150_000,
        budget,
    };

    let receipt = store.admit_attempt(&req).await.expect("initial admit");
    assert_eq!(receipt.attempt_id, attempt_id);

    // Ambiguous provider outcome -> Quarantined.
    store
        .reconcile_attempt(attempt_id, AttemptOutcome::UnknownAfterSend)
        .await
        .expect("quarantine");

    // XEP-0198 reconnect replay with the same command nonce:
    let mut replay_req = req.clone();
    replay_req.attempt_id = AttemptId::new_v4();
    replay_req.step_id = StepId::new(1);
    assert!(matches!(
        store.admit_attempt(&replay_req).await,
        Err(AdmissionError::Quarantined)
    ));

    // Querying by nonce returns the existing Quarantined record; budget was charged only once.
    let existing = store
        .get_attempt_by_nonce(&tenant, &nonce)
        .await
        .expect("query by nonce")
        .expect("existing record");
    assert_eq!(existing.attempt_id, attempt_id);
    assert_eq!(existing.status, AttemptStatus::Quarantined);
    assert_eq!(existing.actual_spend_microcents, 150_000);

    let run = store
        .get_run_budget(run_id)
        .await
        .expect("run budget")
        .expect("run exists");
    assert_eq!(run.spent_microcents, 150_000);
    assert_eq!(run.reserved_microcents, 0);
    assert_eq!(run.steps_used, 1);
}
