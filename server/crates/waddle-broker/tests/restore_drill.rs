use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use waddle_broker::{
    BrokerCustodyService, BrokerError, CredentialLifecycleStatus, EnrollCredentialInput,
    KeyHierarchyService, LocalKmsProvider, SecretBytes, SqliteCompatLedgerStore, TursoLedgerStore,
};
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::permission::{
    AuthzModule, AuthzObject, AuthzRelation, AuthzSubject, AuthzTuple, CheckDecision, CheckRequest,
    PermissionBackend, PermissionBackendError, TenantAuthzStoreRef,
};
use waddle_broker_core::store::TenantStore;
use waddle_tenancy::{TursoPoolConfig, TursoTenantStore};

fn tenant(raw: &str) -> TenantId {
    TenantId::parse(raw).expect("valid tenant id")
}

fn digest(s: &str) -> [u8; 32] {
    Sha256::digest(s.as_bytes()).into()
}

#[derive(Default)]
struct RecordingPermissionBackend {
    deleted_stores: Mutex<HashSet<TenantId>>,
}

impl RecordingPermissionBackend {
    fn was_deleted(&self, tenant_id: &TenantId) -> bool {
        self.deleted_stores
            .lock()
            .expect("lock")
            .contains(tenant_id)
    }
}

#[async_trait]
impl PermissionBackend for RecordingPermissionBackend {
    async fn check(&self, _req: &CheckRequest) -> Result<CheckDecision, PermissionBackendError> {
        Ok(CheckDecision::Denied)
    }

    async fn write_tuples(
        &self,
        _tenant: &TenantId,
        _module: AuthzModule,
        _tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError> {
        Ok(())
    }

    async fn delete_tuples(
        &self,
        _tenant: &TenantId,
        _module: AuthzModule,
        _tuples: &[AuthzTuple],
    ) -> Result<(), PermissionBackendError> {
        Ok(())
    }

    async fn read_tuples(
        &self,
        _tenant: &TenantId,
        _object: &AuthzObject,
        _relation: Option<&AuthzRelation>,
    ) -> Result<Vec<AuthzTuple>, PermissionBackendError> {
        Ok(Vec::new())
    }

    async fn swap_exclusive_relation(
        &self,
        _tenant: &TenantId,
        _module: AuthzModule,
        _object: &AuthzObject,
        _relation: &AuthzRelation,
        _expected_subject: &AuthzSubject,
        _new_subject: &AuthzSubject,
    ) -> Result<(), PermissionBackendError> {
        Ok(())
    }

    async fn replace_exclusive_relation(
        &self,
        _tenant: &TenantId,
        _module: AuthzModule,
        _object: &AuthzObject,
        _relation: &AuthzRelation,
        _new_subject: &AuthzSubject,
    ) -> Result<(), PermissionBackendError> {
        Ok(())
    }

    async fn provision_tenant_store(
        &self,
        tenant: &TenantId,
    ) -> Result<TenantAuthzStoreRef, PermissionBackendError> {
        Ok(TenantAuthzStoreRef {
            tenant_id: tenant.clone(),
            store_id: format!("openfga-store-{tenant}"),
            model_version: "1.2".to_owned(),
        })
    }

    async fn delete_tenant_store(&self, tenant: &TenantId) -> Result<(), PermissionBackendError> {
        self.deleted_stores
            .lock()
            .expect("lock")
            .insert(tenant.clone());
        Ok(())
    }
}

#[tokio::test]
async fn lifecycle_rotate_replace_and_revoke_invalidate_prior_versions() {
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("waddle.social", kms));
    let ledger = Arc::new(
        SqliteCompatLedgerStore::connect_in_memory()
            .await
            .expect("sqlite compat store"),
    );
    let svc = BrokerCustodyService::new(hierarchy, ledger)
        .await
        .expect("custody svc");

    let t = tenant("alpha.waddle.social");
    let v1_secret = SecretBytes::from_slice(b"sk-lifecycle-v1").expect("v1");
    let v2_secret = SecretBytes::from_slice(b"sk-lifecycle-v2").expect("v2");
    let v3_secret = SecretBytes::from_slice(b"sk-lifecycle-v3").expect("v3");

    let r1 = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: "lc-nonce-1".to_owned(),
            payload_digest_sha256: digest("payload-1"),
            expires_at_ms: i64::MAX,
            plaintext: v1_secret,
        })
        .await
        .expect("enroll v1");
    assert_eq!(r1.version, 1);

    // Rotate v1 -> v2.
    let r2 = svc
        .rotate(&t, r1.credential_id, &v2_secret)
        .await
        .expect("rotate v2");
    assert_eq!(r2.version, 2);
    let active = svc
        .unseal_for_egress(&t, r1.credential_id)
        .await
        .expect("unseal v2");
    assert_eq!(active, v2_secret);

    // Replace r1 with a brand-new credential; r1 becomes permanently destroyed.
    let r3 = svc
        .replace(
            &t,
            r1.credential_id,
            &v3_secret,
            "lc-nonce-replace".to_owned(),
            digest("payload-replace"),
            i64::MAX,
        )
        .await
        .expect("replace");
    assert_ne!(r3.credential_id, r1.credential_id);
    assert!(matches!(
        svc.unseal_for_egress(&t, r1.credential_id).await,
        Err(BrokerError::CredentialDestroyed(_))
    ));
    assert_eq!(
        svc.unseal_for_egress(&t, r3.credential_id)
            .await
            .expect("unseal replacement"),
        v3_secret
    );

    // Revoke/withdraw r3.
    let rev = svc
        .revoke_and_destroy(&t, r3.credential_id)
        .await
        .expect("revoke");
    assert_eq!(rev.status, CredentialLifecycleStatus::Revoked);
    assert!(matches!(
        svc.unseal_for_egress(&t, r3.credential_id).await,
        Err(BrokerError::CredentialDestroyed(_))
    ));
}

#[tokio::test]
async fn destruction_ledger_and_retired_account_jids_restore_enforcement() {
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("waddle.social", kms));
    let store = Arc::new(
        SqliteCompatLedgerStore::connect_in_memory()
            .await
            .expect("sqlite compat"),
    );
    let svc = BrokerCustodyService::new(hierarchy, store.clone())
        .await
        .expect("svc");

    let t = tenant("beta.waddle.social");
    let alice = AccountId::new_v4();
    let rec = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: Some(alice),
            provider: "openrouter".to_owned(),
            nonce: "retire-nonce-1".to_owned(),
            payload_digest_sha256: digest("alice-cred"),
            expires_at_ms: i64::MAX,
            plaintext: SecretBytes::from_slice(b"sk-alice-member-key").expect("secret"),
        })
        .await
        .expect("enroll alice");

    // Take backup before retiring alice.
    let backup = store.snapshot_envelopes().await.expect("snapshot");

    // Retire alice (`broker_retired_accounts` + destruction ledger + KMS shred).
    let deleted = svc
        .retire_account(&t, alice, "alice@beta.waddle.social", "alice")
        .await
        .expect("retire alice");
    assert_eq!(deleted, 1);

    // Restore stale backup containing alice's credential.
    store
        .restore_envelopes_snapshot(&backup)
        .await
        .expect("restore snapshot");
    svc.begin_restore().await.expect("begin restore");
    assert!(!svc.egress_open());
    assert!(matches!(
        svc.unseal_for_egress(&t, rec.credential_id).await,
        Err(BrokerError::EgressClosedPendingRestore)
    ));

    // Apply destruction ledger & reopen: alice's credential is purged & unusable.
    let report = svc
        .apply_destruction_ledger_and_reopen()
        .await
        .expect("apply ledger");
    assert!(svc.egress_open());
    assert_eq!(report.purged_envelopes, 1);
    assert!(svc.unseal_for_egress(&t, rec.credential_id).await.is_err());

    // Retired account cannot enroll new credentials either.
    let re_enroll = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: Some(alice),
            provider: "openrouter".to_owned(),
            nonce: "retire-nonce-2".to_owned(),
            payload_digest_sha256: digest("alice-cred-2"),
            expires_at_ms: i64::MAX,
            plaintext: SecretBytes::from_slice(b"sk-alice-new").expect("secret"),
        })
        .await;
    assert!(matches!(re_enroll, Err(BrokerError::AccountRetired(_))));
}

#[tokio::test]
async fn tenant_delete_purges_turso_wal_and_openfga_store() {
    let dir = TempDir::new().expect("tempdir");
    let broker_store = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("broker"))
            .await
            .expect("broker turso store"),
    );
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("waddle.social", kms));
    let svc = BrokerCustodyService::new(hierarchy, broker_store)
        .await
        .expect("svc");

    let tenant_data_dir = dir.path().join("tenants");
    let tenant_store = TursoTenantStore::new(&tenant_data_dir, TursoPoolConfig::default(), None)
        .expect("turso tenant store");
    let perm_backend = RecordingPermissionBackend::default();

    let t = tenant("doomed.waddle.social");
    tenant_store
        .provision_tenant(&t)
        .await
        .expect("provision tenant");
    let db_path = tenant_data_dir.join("doomed.waddle.social.db");
    assert!(db_path.exists());

    let rec = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: "td-nonce-1".to_owned(),
            payload_digest_sha256: digest("doomed-cred"),
            expires_at_ms: i64::MAX,
            plaintext: SecretBytes::from_slice(b"sk-doomed-tenant").expect("secret"),
        })
        .await
        .expect("enroll");

    svc.delete_tenant(&t, &tenant_store, &perm_backend)
        .await
        .expect("delete tenant");

    // Verify Turso .db, -wal, -shm purged from disk and OpenFGA DeleteStore called.
    assert!(!db_path.exists());
    assert!(!tenant_data_dir.join("doomed.waddle.social.db-wal").exists());
    assert!(!tenant_data_dir.join("doomed.waddle.social.db-shm").exists());
    assert!(perm_backend.was_deleted(&t));

    // Broker refuses egress and future enrollments for deleted tenant.
    assert!(matches!(
        svc.unseal_for_egress(&t, rec.credential_id).await,
        Err(BrokerError::TenantDestroyed(_))
    ));
}

#[tokio::test]
async fn pairwise_sm_replay_destruction_restore() {
    let dir = TempDir::new().expect("tempdir");
    let store = Arc::new(
        TursoLedgerStore::open_in_dir(dir.path())
            .await
            .expect("turso store"),
    );
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("waddle.social", kms));
    let svc = BrokerCustodyService::new(hierarchy, store.clone())
        .await
        .expect("svc");

    let t = tenant("pairwise.waddle.social");
    let secret = SecretBytes::from_slice(b"sk-pairwise-replay-1").expect("secret");
    let d1 = digest("sealed-iq-payload-1");

    let first = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: "sm-nonce-1".to_owned(),
            payload_digest_sha256: d1,
            expires_at_ms: i64::MAX,
            plaintext: secret.clone(),
        })
        .await
        .expect("first enroll");
    assert!(!first.replayed);

    // XEP-0198 reconnect replay with identical nonce + payload digest -> idempotent receipt.
    let replay = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: "sm-nonce-1".to_owned(),
            payload_digest_sha256: d1,
            expires_at_ms: i64::MAX,
            plaintext: secret,
        })
        .await
        .expect("idempotent replay");
    assert!(replay.replayed);
    assert_eq!(replay.credential_id, first.credential_id);
    assert_eq!(replay.version, first.version);

    // Same nonce with mutated payload digest -> fail closed!
    let conflict = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: "sm-nonce-1".to_owned(),
            payload_digest_sha256: digest("mutated-ciphertext"),
            expires_at_ms: i64::MAX,
            plaintext: SecretBytes::from_slice(b"sk-other").expect("other"),
        })
        .await;
    assert!(matches!(conflict, Err(BrokerError::NoncePayloadConflict)));

    // Backup -> destroy -> restore stale backup -> apply ledger -> still destroyed.
    let backup_path = dir.path().join("backup.db");
    store
        .snapshot_vault_to(&backup_path)
        .await
        .expect("snapshot");
    svc.revoke_and_destroy(&t, first.credential_id)
        .await
        .expect("destroy");
    store
        .replace_vault_from_backup(&backup_path)
        .await
        .expect("replace backup");
    svc.begin_restore().await.expect("begin restore");
    assert!(matches!(
        svc.unseal_for_egress(&t, first.credential_id).await,
        Err(BrokerError::EgressClosedPendingRestore)
    ));
    let report = svc
        .apply_destruction_ledger_and_reopen()
        .await
        .expect("reopen");
    assert_eq!(report.purged_envelopes, 1);
    assert!(matches!(
        svc.unseal_for_egress(&t, first.credential_id).await,
        Err(BrokerError::CredentialDestroyed(_))
    ));
}

#[tokio::test]
async fn e2e_gate5_backup_restore_drill_turso_wal() {
    let dir = TempDir::new().expect("tempdir");
    let store = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("broker"))
            .await
            .expect("turso ledger"),
    );
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("waddle.social", kms));
    let svc = BrokerCustodyService::new(hierarchy, store.clone())
        .await
        .expect("svc");

    let t = tenant("gate5.waddle.social");
    let alice = AccountId::new_v4();
    let sentinel_c1 = "sk-gate5-c1-sentinel-7f6e5d4c3b2a";
    let sentinel_c2 = "sk-gate5-c2-sentinel-1a2b3c4d5e6f";

    // 1. Enroll c1 (tenant) and c2 (member).
    let c1 = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: "g5-n1".to_owned(),
            payload_digest_sha256: digest("g5-c1"),
            expires_at_ms: i64::MAX,
            plaintext: SecretBytes::from_slice(sentinel_c1.as_bytes()).expect("c1"),
        })
        .await
        .expect("enroll c1");
    let c2 = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: Some(alice),
            provider: "anthropic".to_owned(),
            nonce: "g5-n2".to_owned(),
            payload_digest_sha256: digest("g5-c2"),
            expires_at_ms: i64::MAX,
            plaintext: SecretBytes::from_slice(sentinel_c2.as_bytes()).expect("c2"),
        })
        .await
        .expect("enroll c2");

    // Verify unseal works and raw Turso vault.db bytes contain zero plaintext sentinels.
    assert_eq!(
        svc.unseal_for_egress(&t, c1.credential_id)
            .await
            .expect("unseal c1")
            .as_bytes(),
        sentinel_c1.as_bytes()
    );
    let backup_path = dir.path().join("vault-backup.db");
    store
        .snapshot_vault_to(&backup_path)
        .await
        .expect("snapshot backup");
    let raw_db_bytes = std::fs::read(&backup_path).expect("read backup");
    assert!(
        !raw_db_bytes
            .windows(sentinel_c1.len())
            .any(|w| w == sentinel_c1.as_bytes()),
        "sentinel c1 leaked into Turso DB file"
    );
    assert!(
        !raw_db_bytes
            .windows(sentinel_c2.len())
            .any(|w| w == sentinel_c2.as_bytes()),
        "sentinel c2 leaked into Turso DB file"
    );

    // 2. Rotate c1 v1 -> v2, revoke c2, retire alice.
    let c1_v2_secret = SecretBytes::from_slice(b"sk-gate5-c1-v2-fresh").expect("v2");
    let _ = svc
        .rotate(&t, c1.credential_id, &c1_v2_secret)
        .await
        .expect("rotate c1");
    let _ = svc
        .revoke_and_destroy(&t, c2.credential_id)
        .await
        .expect("revoke c2");
    let _ = svc
        .retire_account(&t, alice, "alice@gate5.waddle.social", "alice")
        .await
        .expect("retire alice");

    // 3. Restore stale backup (contains c1 v1 and c2!) and enter restore mode.
    store
        .replace_vault_from_backup(&backup_path)
        .await
        .expect("replace vault from backup");
    svc.begin_restore().await.expect("begin restore");
    assert!(!svc.egress_open());

    // 4. Verify egress is blocked before ledger application.
    assert!(matches!(
        svc.unseal_for_egress(&t, c1.credential_id).await,
        Err(BrokerError::EgressClosedPendingRestore)
    ));
    assert!(matches!(
        svc.unseal_for_egress(&t, c2.credential_id).await,
        Err(BrokerError::EgressClosedPendingRestore)
    ));

    // 5. Apply destruction ledger & reopen.
    let report = svc
        .apply_destruction_ledger_and_reopen()
        .await
        .expect("apply ledger & reopen");
    assert!(svc.egress_open());
    assert_eq!(report.purged_envelopes, 2); // Both stale c1 v1 and destroyed c2 purged!

    // 6. Both c1 v1 (superseded) and c2 (destroyed + retired alice) remain unusable.
    assert!(svc.unseal_for_egress(&t, c1.credential_id).await.is_err());
    assert!(svc.unseal_for_egress(&t, c2.credential_id).await.is_err());

    // 7. A newly enrolled non-destroyed credential c3 works cleanly.
    let c3_secret = SecretBytes::from_slice(b"sk-gate5-c3-clean").expect("c3");
    let c3 = svc
        .enroll(EnrollCredentialInput {
            tenant_id: t.clone(),
            account_id: None,
            provider: "openrouter".to_owned(),
            nonce: "g5-n3".to_owned(),
            payload_digest_sha256: digest("g5-c3"),
            expires_at_ms: i64::MAX,
            plaintext: c3_secret.clone(),
        })
        .await
        .expect("enroll c3");
    assert_eq!(
        svc.unseal_for_egress(&t, c3.credential_id)
            .await
            .expect("unseal c3"),
        c3_secret
    );
}
