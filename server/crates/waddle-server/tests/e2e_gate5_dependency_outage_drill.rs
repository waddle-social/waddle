//! T4-S11: Release Evidence Gate 5 — Dependency Outage & Fail-Closed
//! Resilience Drill (KMS outage, OpenFGA outage, PlacementLeaseStore outage,
//! and restore-before-ledger egress closure).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use tempfile::TempDir;
use uuid::Uuid;
use waddle_broker::authz::openfga::{OpenFgaConfig, OpenFgaPermissionBackend};
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::custody::{BrokerCustodyService, EnrollCredentialInput};
use waddle_broker::error::{BrokerError, KmsError};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::{KmsKeyRef, KmsProvider, WrappedKeyBlob};
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::ledger::DestructionLedgerStore;
use waddle_broker::secret::{Key256, SecretBytes};
use waddle_broker_core::boot_clock::{BootClock, BootInstant};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject, AuthzSubjectType,
    CheckRequest, PermissionBackend,
};
use waddle_broker_core::placement::FenceGeneration;
use waddle_tenancy::quiesce::LocalLeaseGuard;

struct FailingKms;

#[async_trait]
impl KmsProvider for FailingKms {
    async fn wrap_key(
        &self,
        _key_ref: &KmsKeyRef,
        _plaintext: &Key256,
        _aad: &[u8],
    ) -> Result<WrappedKeyBlob, KmsError> {
        Err(KmsError::Unavailable("simulated KMS outage".to_owned()))
    }

    async fn unwrap_key(
        &self,
        _key_ref: &KmsKeyRef,
        _wrapped: &WrappedKeyBlob,
        _aad: &[u8],
    ) -> Result<Key256, KmsError> {
        Err(KmsError::Unavailable("simulated KMS outage".to_owned()))
    }

    fn user_wrapping_secret_ref(
        &self,
        tenant: &TenantId,
        account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("user/{}/{}", tenant.as_str(), account))
    }

    fn tenant_kek_ref(&self, tenant: &TenantId) -> Result<KmsKeyRef, KmsError> {
        KmsKeyRef::new(format!("tenant/{}", tenant.as_str()))
    }

    async fn ensure_user_wrapping_secret(
        &self,
        _tenant: &TenantId,
        _account: AccountId,
    ) -> Result<KmsKeyRef, KmsError> {
        Err(KmsError::Unavailable("simulated KMS outage".to_owned()))
    }

    async fn ensure_tenant_kek(&self, _tenant_id: &TenantId) -> Result<KmsKeyRef, KmsError> {
        Err(KmsError::Unavailable("simulated KMS outage".to_owned()))
    }

    async fn destroy_wrapping_secret(&self, _key_ref: &KmsKeyRef) -> Result<(), KmsError> {
        Err(KmsError::Unavailable("simulated KMS outage".to_owned()))
    }

    async fn is_destroyed(&self, _key_ref: &KmsKeyRef) -> Result<bool, KmsError> {
        Err(KmsError::Unavailable("simulated KMS outage".to_owned()))
    }
}

struct UnreadableBootClock;

impl BootClock for UnreadableBootClock {
    fn now(&self) -> Option<BootInstant> {
        None
    }

    fn suspend_counted(&self) -> bool {
        true
    }
}

#[tokio::test]
async fn e2e_gate5_dependency_outage_fail_closed_drill() {
    let dir = TempDir::new().expect("tempdir");
    let tenant = TenantId::parse("acme.example").expect("tenant");

    // 1. KMS outage fails closed during enrollment (never falls back to plaintext).
    let ledger = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("custody"))
            .await
            .expect("ledger"),
    );
    let custody = BrokerCustodyService::new(
        Arc::new(KeyHierarchyService::new(
            "trust.example",
            Arc::new(FailingKms),
        )),
        Arc::clone(&ledger) as Arc<dyn DestructionLedgerStore>,
    )
    .await
    .expect("custody");
    let kms_err = custody
        .enroll(EnrollCredentialInput {
            tenant_id: tenant.clone(),
            account_id: None,
            provider: "openai".to_owned(),
            nonce: "outage-kms-1".to_owned(),
            payload_digest_sha256: [0x11; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(b"sk-outage".to_vec()).expect("secret"),
        })
        .await
        .expect_err("KMS outage must fail closed");
    assert!(matches!(
        kms_err,
        BrokerError::Kms(KmsError::Unavailable(_))
    ));

    // 2. OpenFGA outage fails closed on Strong check (never serves stale/allow).
    let fga_unreachable = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: "http://127.0.0.1:9".to_owned(),
        bearer_token: None,
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("openfga");
    let fga_err = fga_unreachable
        .check(&CheckRequest {
            tenant_id: tenant.clone(),
            object: AuthzObject {
                object_type: AuthzObjectType::Channel,
                id: "general".to_owned(),
            },
            permission: AuthzRelation::new("read"),
            subject: AuthzSubject {
                subject_type: AuthzSubjectType::Account,
                id: Uuid::new_v4().to_string(),
                relation: None,
            },
            consistency: AuthzConsistency::Strong,
        })
        .await
        .expect_err("unreachable OpenFGA must fail closed");
    assert!(!fga_err.to_string().is_empty());

    // 3. Unreadable BootClock fails closed on LocalLeaseGuard.
    let guard = LocalLeaseGuard::from_sent(
        Arc::new(UnreadableBootClock),
        tenant.clone(),
        FenceGeneration::new(1),
        BootInstant::from_raw_duration(Duration::from_secs(10)),
        Duration::from_secs(30),
        Duration::from_secs(5),
    )
    .expect("construct guard");
    assert!(
        !guard.is_valid(),
        "unreadable BootClock must fail closed on lease validity"
    );

    // 4. Restore-mode egress gate closes immediately and fails closed until reconciled.
    custody.begin_restore().await.expect("begin restore");
    let restore_err = custody
        .unseal_for_provider_egress(&tenant, Uuid::new_v4(), "openai")
        .await
        .expect_err("restore mode must close egress");
    assert!(matches!(
        restore_err,
        BrokerError::EgressClosedPendingRestore
    ));
}
