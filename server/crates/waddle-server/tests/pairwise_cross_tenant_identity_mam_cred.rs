//! T3-03: Pairwise cross-tenant isolation across identity (`AccountId`),
//! storage (`TursoTenantStore`), in-memory registries, broker credentials
//! (`BrokerCustodyService`), and `PermissionBackend` (OpenFGA + SpiceDB).

use std::sync::Arc;

use chrono::Utc;
use tempfile::TempDir;
use waddle_broker::authz::openfga::{
    InProcessOpenFgaServer, OpenFgaConfig, OpenFgaPermissionBackend,
};
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::custody::{BrokerCustodyService, EnrollCredentialInput};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::identity::{
    AccountId, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
};
use waddle_broker_core::placement::FenceGeneration;
use waddle_broker_core::store::{TenantQueryScope, TenantStore};
use waddle_server::permissions::TenantSpiceDbPermissionBackend;
use waddle_tenancy::turso_store::{TursoPoolConfig, TursoTenantStore};

fn holder() -> HolderInstance {
    HolderInstance {
        spiffe_id: SpiffeId::parse("spiffe://trust.example/ns/waddle/sa/cell-a").expect("spiffe"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([5u8; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![5u8; 32]).expect("proof"),
    }
}

#[tokio::test]
async fn cross_tenant_identity_storage_credentials_and_authz_are_isolated() {
    let dir = TempDir::new().expect("tempdir");
    let acme = TenantId::parse("acme.example").expect("acme");
    let globex = TenantId::parse("globex.example").expect("globex");

    // Distinct immutable AccountIds for identical localpart "alice" in two tenants.
    let alice_acme = AccountId::new_v4();
    let alice_globex = AccountId::new_v4();
    assert_ne!(alice_acme, alice_globex);

    // 1. Turso per-tenant DB isolation.
    let store = TursoTenantStore::new(dir.path().join("tenants"), TursoPoolConfig::default(), None)
        .expect("turso store");
    store.provision_tenant(&acme).await.expect("prov acme");
    store.provision_tenant(&globex).await.expect("prov globex");
    let _h_acme = store
        .open_tenant(&TenantQueryScope {
            tenant_id: acme.clone(),
            holder: holder(),
            placement_generation: FenceGeneration::new(1),
        })
        .await
        .expect("open acme");
    let _h_globex = store
        .open_tenant(&TenantQueryScope {
            tenant_id: globex.clone(),
            holder: holder(),
            placement_generation: FenceGeneration::new(1),
        })
        .await
        .expect("open globex");
    assert_ne!(
        store.tenant_db_path(&acme).expect("path acme"),
        store.tenant_db_path(&globex).expect("path globex")
    );

    // 2. Broker credential custody isolation: acme's credential cannot be unsealed under globex.
    let ledger = Arc::new(
        TursoLedgerStore::open_in_dir(dir.path())
            .await
            .expect("custody ledger"),
    );
    let kms = Arc::new(LocalKmsProvider::new());
    let hierarchy = Arc::new(KeyHierarchyService::new("trust.example", kms));
    let custody = BrokerCustodyService::new(hierarchy, ledger)
        .await
        .expect("custody");
    let acme_cred = custody
        .enroll(EnrollCredentialInput {
            tenant_id: acme.clone(),
            account_id: Some(alice_acme),
            provider: "openai".to_owned(),
            plaintext: SecretBytes::new(b"sk-acme-alice-secret".to_vec()).expect("secret"),
            nonce: "nonce-acme-1".to_owned(),
            payload_digest_sha256: [1u8; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
        })
        .await
        .expect("enroll acme");

    assert!(
        custody
            .unseal_for_egress(&globex, acme_cred.credential_id)
            .await
            .is_err(),
        "globex MUST NOT unseal acme credential"
    );

    // 3. PermissionBackend isolation across BOTH OpenFGA and SpiceDB.
    let fga_server = InProcessOpenFgaServer::start().await.expect("openfga");
    let openfga = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: fga_server.endpoint().to_owned(),
        bearer_token: None,
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("openfga client");
    let spicedb = TenantSpiceDbPermissionBackend::in_memory(AuthzCallerRole::Broker);

    for backend in [
        &openfga as &dyn PermissionBackend,
        &spicedb as &dyn PermissionBackend,
    ] {
        backend
            .provision_tenant_store(&acme)
            .await
            .expect("prov acme");
        backend
            .provision_tenant_store(&globex)
            .await
            .expect("prov globex");

        let room_obj = AuthzObject {
            object_type: AuthzObjectType::Channel,
            id: "general".to_owned(),
        };
        let remote_bob = AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: "bob@remote.example".to_owned(),
            relation: None,
        };
        backend
            .write_tuples(
                &acme,
                AuthzModule::Chat,
                &[AuthzTuple {
                    object: room_obj.clone(),
                    relation: AuthzRelation::new("member"),
                    subject: remote_bob.clone(),
                }],
            )
            .await
            .expect("grant bob in acme");

        assert_eq!(
            backend
                .check(&CheckRequest {
                    tenant_id: acme.clone(),
                    object: room_obj.clone(),
                    permission: AuthzRelation::new("read"),
                    subject: remote_bob.clone(),
                    consistency: AuthzConsistency::Strong,
                })
                .await
                .expect("check acme"),
            CheckDecision::Allowed
        );
        assert_eq!(
            backend
                .check(&CheckRequest {
                    tenant_id: globex.clone(),
                    object: room_obj,
                    permission: AuthzRelation::new("read"),
                    subject: remote_bob,
                    consistency: AuthzConsistency::Strong,
                })
                .await
                .expect("check globex"),
            CheckDecision::Denied
        );
    }
}
