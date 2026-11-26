//! M6 F20 & F21 integration tests:
//! - T1-F20-04 (`zdr_route`): admin route selection accepts only qualified ZDR/no-training routes.
//! - T2-M6-02 (`zdr_boundary`): unqualified, allow-collection, fallback, personal-key, or expired
//!   ZDR routes fail closed.
//! - T1-F21-05 (`rollback`): rollback closes admission, cancels active streams, advances fences,
//!   preserves destruction ledger + retired accounts + consumed nonces, and refuses unsafe recovery.

use std::sync::Arc;

use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use waddle_broker::admission::replica_fence::BrokerReplicaFence;
use waddle_broker::admission::rollback::{
    RollbackController, RollbackError, RollbackScope, RollbackTarget,
};
use waddle_broker::admission::turso::TursoAdmissionLedgerStore;
use waddle_broker::egress::{
    DataCollectionPolicy, ProviderOperation, QualifiedZdrRoute, RouteKeyOrigin,
    RouteQualificationError, ZdrRouteQuery, ZdrRouteRegistry,
};
use waddle_broker::envelope::{EnvelopeCipher, SealedEnvelope};
use waddle_broker::key_hierarchy::TenantDekRecord;
use waddle_broker::kms::{KmsKeyRef, WrappedKeyBlob};
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::ledger::{
    DestructionLedgerStore, DestructionReason, DestructionRecord, DestructionTargetKind,
    EnrollmentNonceRecord, RetiredAccountRecord,
};
use waddle_broker_core::admission::{
    AdmissionLedgerStore, AttemptId, CredentialKey, CredentialScope, FenceKey, FenceKind,
    FenceState, RoomScopeKey,
};
use waddle_broker_core::boot_clock::SystemBootClock;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::placement::FenceGeneration;

fn qualified_route() -> QualifiedZdrRoute {
    QualifiedZdrRoute {
        provider: "openrouter".to_owned(),
        tier: "enterprise-zdr".to_owned(),
        deployment: "zdr".to_owned(),
        model: "typesafe/jev-1.13".to_owned(),
        operation: ProviderOperation::ChatCompletions,
        downstream_routing: "openrouter-zdr-strict".to_owned(),
        data_collection: DataCollectionPolicy::Deny,
        zdr: true,
        allow_fallback: false,
        qualified_until_ms: 1_750_000_000_000,
    }
}

#[test]
fn zdr_route_selects_only_qualified_zero_retention_no_training_routes() {
    let mut registry = ZdrRouteRegistry::new();
    registry
        .register_qualified(qualified_route())
        .expect("qualified ZDR route registers cleanly");

    let selected = registry
        .select_route(&ZdrRouteQuery {
            provider: "openrouter",
            tier: "enterprise-zdr",
            deployment: "zdr",
            model: "typesafe/jev-1.13",
            operation: ProviderOperation::ChatCompletions,
            downstream_routing: "openrouter-zdr-strict",
            key_origin: RouteKeyOrigin::ManagedTenant,
            now_ms: 1_749_999_999_999,
        })
        .expect("qualified ZDR route selects cleanly before expiry");
    assert_eq!(selected.data_collection, DataCollectionPolicy::Deny);
    assert!(selected.zdr);
    assert!(!selected.allow_fallback);

    let personal_with_agreement = registry.select_route(&ZdrRouteQuery {
        provider: "openrouter",
        tier: "enterprise-zdr",
        deployment: "zdr",
        model: "typesafe/jev-1.13",
        operation: ProviderOperation::ChatCompletions,
        downstream_routing: "openrouter-zdr-strict",
        key_origin: RouteKeyOrigin::PersonalMember {
            verified_zdr_agreement: true,
        },
        now_ms: 1_749_000_000_000,
    });
    assert!(personal_with_agreement.is_ok());

    let unknown = registry.select_route(&ZdrRouteQuery {
        provider: "openrouter",
        tier: "standard",
        deployment: "default",
        model: "typesafe/jev-1.13",
        operation: ProviderOperation::ChatCompletions,
        downstream_routing: "openrouter-default",
        key_origin: RouteKeyOrigin::ManagedTenant,
        now_ms: 1_749_000_000_000,
    });
    assert_eq!(unknown, Err(RouteQualificationError::UnknownRoute));
}

#[test]
fn zdr_boundary_rejects_unqualified_fallback_personal_unverified_and_expired_routes() {
    let mut registry = ZdrRouteRegistry::new();

    let mut no_zdr = qualified_route();
    no_zdr.zdr = false;
    assert_eq!(
        registry.register_qualified(no_zdr),
        Err(RouteQualificationError::ZdrNotEnabled)
    );

    let mut allow_collection = qualified_route();
    allow_collection.data_collection = DataCollectionPolicy::Allow;
    assert_eq!(
        registry.register_qualified(allow_collection),
        Err(RouteQualificationError::DataCollectionNotDenied)
    );

    let mut fallback = qualified_route();
    fallback.allow_fallback = true;
    assert_eq!(
        registry.register_qualified(fallback),
        Err(RouteQualificationError::FallbackProhibited)
    );

    registry
        .register_qualified(qualified_route())
        .expect("valid route registers");

    // Personal key lacking verified ZDR agreement must fail closed.
    let personal_denied = registry.select_route(&ZdrRouteQuery {
        provider: "openrouter",
        tier: "enterprise-zdr",
        deployment: "zdr",
        model: "typesafe/jev-1.13",
        operation: ProviderOperation::ChatCompletions,
        downstream_routing: "openrouter-zdr-strict",
        key_origin: RouteKeyOrigin::PersonalMember {
            verified_zdr_agreement: false,
        },
        now_ms: 1_749_000_000_000,
    });
    assert_eq!(
        personal_denied,
        Err(RouteQualificationError::PersonalKeyIneligible)
    );

    // Exact boundary `now_ms == qualified_until_ms` must expire closed.
    let exact_expiry = registry.select_route(&ZdrRouteQuery {
        provider: "openrouter",
        tier: "enterprise-zdr",
        deployment: "zdr",
        model: "typesafe/jev-1.13",
        operation: ProviderOperation::ChatCompletions,
        downstream_routing: "openrouter-zdr-strict",
        key_origin: RouteKeyOrigin::ManagedTenant,
        now_ms: 1_750_000_000_000,
    });
    assert_eq!(
        exact_expiry,
        Err(RouteQualificationError::QualificationExpired {
            qualified_until_ms: 1_750_000_000_000,
            now_ms: 1_750_000_000_000,
        })
    );

    let past_expiry = registry.select_route(&ZdrRouteQuery {
        provider: "openrouter",
        tier: "enterprise-zdr",
        deployment: "zdr",
        model: "typesafe/jev-1.13",
        operation: ProviderOperation::ChatCompletions,
        downstream_routing: "openrouter-zdr-strict",
        key_origin: RouteKeyOrigin::ManagedTenant,
        now_ms: 1_750_000_000_001,
    });
    assert_eq!(
        past_expiry,
        Err(RouteQualificationError::QualificationExpired {
            qualified_until_ms: 1_750_000_000_000,
            now_ms: 1_750_000_000_001,
        })
    );
}

#[tokio::test]
async fn rollback_closes_admission_advances_fences_cancels_streams_and_preserves_ledger() {
    let tmp = TempDir::new().expect("tempdir");
    let admission = Arc::new(
        TursoAdmissionLedgerStore::open(tmp.path().join("admission.db"))
            .await
            .expect("open admission store"),
    );
    let ledger_dir = tmp.path().join("ledger");
    std::fs::create_dir_all(&ledger_dir).expect("create ledger dir");
    let ledger = TursoLedgerStore::open_in_dir(&ledger_dir)
        .await
        .expect("open ledger store");

    let tenant = TenantId::parse("acme.example").expect("tenant");
    let account = AccountId::new_v4();
    let cred_id = Uuid::new_v4();
    let dek_id = Uuid::new_v4();
    let destroyed_id = Uuid::new_v4();
    ledger
        .append_destruction(&DestructionRecord {
            entry_id: Uuid::new_v4(),
            target_kind: DestructionTargetKind::Credential,
            tenant_id: tenant.clone(),
            target_id: destroyed_id.to_string(),
            reason: DestructionReason::Revoked,
            destroyed_at_ms: 1000,
        })
        .await
        .expect("append destruction");
    ledger
        .record_retired_account(&RetiredAccountRecord {
            tenant_id: tenant.clone(),
            account_id: account,
            jid_key: "alice@acme.example".to_owned(),
            localpart_key: "alice".to_owned(),
            retired_at_ms: 1000,
        })
        .await
        .expect("record retired account");
    let nonce_hash = [0x5Au8; 32];
    ledger
        .claim_enrollment_nonce_and_put_envelope(
            &EnrollmentNonceRecord {
                nonce_sha256: nonce_hash,
                payload_digest_sha256: [0x11u8; 32],
                tenant_id: tenant.clone(),
                credential_id: cred_id,
                credential_version: 1,
                expires_at_ms: 999_999,
                created_at_ms: 1000,
            },
            &waddle_broker::ledger::StoredEnvelopeRecord {
                credential_id: cred_id,
                tenant_id: tenant.clone(),
                account_id: None,
                provider: "openrouter".to_owned(),
                version: 1,
                envelope: SealedEnvelope {
                    cipher: EnvelopeCipher::Aes256Gcm,
                    nonce: [1u8; 12],
                    ciphertext: b"cipher".to_vec(),
                    wrapped_dek_id: dek_id,
                    dek_version: 1,
                    credential_version: 1,
                },
                tenant_dek: TenantDekRecord {
                    dek_id,
                    tenant_id: tenant.clone(),
                    version: 1,
                    wrapped_by_kms: WrappedKeyBlob {
                        key_ref: KmsKeyRef::new("local://test").expect("kms key ref"),
                        ciphertext: b"wrapped".to_vec(),
                    },
                },
                user_dek: None,
                created_at_ms: 1000,
            },
        )
        .await
        .expect("claim nonce");

    let cred_key = CredentialKey {
        credential_id: cred_id,
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
        Arc::clone(&admission) as Arc<dyn waddle_broker::admission::ReplicaFenceStore>,
        Arc::new(SystemBootClock),
    );
    replica_fence.tick_once().await.expect("initial fence tick");
    let stream_cancel = replica_fence
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
        .expect("register active stream");
    assert!(!*stream_cancel.borrow());

    let cancel_token = CancellationToken::new();
    let controller = RollbackController::new();
    controller
        .check_admissible(&tenant, "openrouter", "default", "openrouter/free")
        .expect("admissible prior to rollback");

    let affected = vec![FenceKey {
        tenant_id: tenant.clone(),
        kind: FenceKind::Credential(cred_key.clone()),
    }];
    let report = controller
        .execute_rollback(
            RollbackScope::ProviderProfile {
                provider: "openrouter".to_owned(),
                deployment: "default".to_owned(),
                model: "openrouter/free".to_owned(),
            },
            &affected,
            RollbackTarget {
                admission_store: admission.as_ref(),
                replica_fence: Some(&replica_fence),
                destruction_ledger: &ledger,
                cancel_token: Some(&cancel_token),
            },
        )
        .await
        .expect("execute rollback");

    assert!(report.stream_drain_signaled);
    assert!(cancel_token.is_cancelled());
    assert_eq!(report.replica_streams_canceled, 1);
    assert!(*stream_cancel.borrow(), "active replica stream canceled");
    assert_eq!(report.closed_fences.len(), 1);
    assert!(report.closed_fences[0].1 > FenceGeneration::new(before.credential.generation.get()));

    // Subsequent admission fails closed.
    assert_eq!(
        controller.check_admissible(&tenant, "openrouter", "default", "openrouter/free"),
        Err(RollbackError::AdmissionDisabled)
    );
    // Durable fence remains Closed at strictly advanced generation.
    let after = admission
        .snapshot_fences(&tenant, &cred_key, &room_key)
        .await
        .expect("snapshot after rollback");
    assert_eq!(after.credential.state, FenceState::Closed);
    assert!(after.credential.generation > before.credential.generation);

    // Destruction records, retired accounts, and consumed nonces remain preserved.
    assert!(ledger
        .is_destroyed(
            DestructionTargetKind::Credential,
            &tenant,
            &destroyed_id.to_string()
        )
        .await
        .expect("check destroyed"));
    assert!(ledger
        .is_account_retired(&tenant, account)
        .await
        .expect("check retired"));
    assert!(ledger
        .get_enrollment_nonce(&nonce_hash)
        .await
        .expect("check nonce")
        .is_some());

    // Unsafe recovery actions are unconditionally rejected.
    assert_eq!(
        controller.reject_plaintext_credential_restore(),
        Err(RollbackError::PlaintextCredentialRestoreForbidden)
    );
    assert_eq!(
        controller.reject_fence_removal(),
        Err(RollbackError::FenceRemovalForbidden)
    );
}
