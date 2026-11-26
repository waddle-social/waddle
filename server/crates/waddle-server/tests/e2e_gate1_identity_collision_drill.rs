//! T4-S02: Release Evidence Gate 1 — Identity Collision Drill.
//! Two tenants (`acme.example` and `globex.example`) with identical localpart
//! `alice` and identical remote participant `bob@remote.example` across storage,
//! credentials, and both OpenFGA and SpiceDB `PermissionBackend`s.

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
        spiffe_id: SpiffeId::parse("spiffe://trust.example/ns/waddle/sa/cell-shared")
            .expect("spiffe"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([8u8; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![8u8; 32]).expect("proof"),
    }
}

#[tokio::test]
async fn e2e_gate1_identity_collision_drill_acme_and_globex() {
    let dir = TempDir::new().expect("tempdir");
    let acme = TenantId::parse("acme.example").expect("acme");
    let globex = TenantId::parse("globex.example").expect("globex");

    // 1. Storage isolation under TursoTenantStore
    let tenant_store =
        TursoTenantStore::new(dir.path().join("tenants"), TursoPoolConfig::default(), None)
            .expect("turso store");
    tenant_store
        .provision_tenant(&acme)
        .await
        .expect("prov acme");
    tenant_store
        .provision_tenant(&globex)
        .await
        .expect("prov globex");
    for t in [&acme, &globex] {
        tenant_store
            .open_tenant(&TenantQueryScope {
                tenant_id: t.clone(),
                holder: holder(),
                placement_generation: FenceGeneration::new(1),
            })
            .await
            .expect("open tenant");
    }

    // 2. Credential custody isolation for colliding "alice" across tenants
    let alice_acme = AccountId::new_v4();
    let alice_globex = AccountId::new_v4();
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
            provider: "anthropic".to_owned(),
            plaintext: SecretBytes::new(b"sk-ant-acme-only".to_vec()).expect("secret"),
            nonce: "gate1-acme-1".to_owned(),
            payload_digest_sha256: [0xA1; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
        })
        .await
        .expect("enroll acme");
    let globex_cred = custody
        .enroll(EnrollCredentialInput {
            tenant_id: globex.clone(),
            account_id: Some(alice_globex),
            provider: "anthropic".to_owned(),
            plaintext: SecretBytes::new(b"sk-ant-globex-only".to_vec()).expect("secret"),
            nonce: "gate1-globex-1".to_owned(),
            payload_digest_sha256: [0xB2; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
        })
        .await
        .expect("enroll globex");

    assert_eq!(
        custody
            .unseal_for_egress(&acme, acme_cred.credential_id)
            .await
            .expect("unseal acme")
            .as_bytes(),
        b"sk-ant-acme-only"
    );
    assert_eq!(
        custody
            .unseal_for_egress(&globex, globex_cred.credential_id)
            .await
            .expect("unseal globex")
            .as_bytes(),
        b"sk-ant-globex-only"
    );
    assert!(custody
        .unseal_for_egress(&acme, globex_cred.credential_id)
        .await
        .is_err());
    assert!(custody
        .unseal_for_egress(&globex, acme_cred.credential_id)
        .await
        .is_err());

    // 3. PermissionBackend collision drill across OpenFGA AND SpiceDB:
    // identical object IDs ("general", "cred-shared") and identical subject ("bob@remote.example").
    let fga_server = InProcessOpenFgaServer::start().await.expect("openfga");
    let openfga = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: fga_server.endpoint().to_owned(),
        bearer_token: None,
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("openfga");
    let spicedb = TenantSpiceDbPermissionBackend::in_memory(AuthzCallerRole::Broker);

    for backend in [
        &openfga as &dyn PermissionBackend,
        &spicedb as &dyn PermissionBackend,
    ] {
        let ref_acme = backend
            .provision_tenant_store(&acme)
            .await
            .expect("prov acme");
        let ref_globex = backend
            .provision_tenant_store(&globex)
            .await
            .expect("prov globex");
        assert_ne!(ref_acme.store_id, ref_globex.store_id);

        let shared_channel = AuthzObject {
            object_type: AuthzObjectType::Channel,
            id: "general".to_owned(),
        };
        let bob = AuthzSubject {
            subject_type: AuthzSubjectType::Account,
            id: "bob@remote.example".to_owned(),
            relation: None,
        };
        backend
            .write_tuples(
                &acme,
                AuthzModule::Chat,
                &[AuthzTuple {
                    object: shared_channel.clone(),
                    relation: AuthzRelation::new("member"),
                    subject: bob.clone(),
                }],
            )
            .await
            .expect("grant bob in acme");
        backend
            .write_tuples(
                &globex,
                AuthzModule::Chat,
                &[AuthzTuple {
                    object: shared_channel.clone(),
                    relation: AuthzRelation::new("outcast"),
                    subject: bob.clone(),
                }],
            )
            .await
            .expect("ban bob in globex");

        assert_eq!(
            backend
                .check(&CheckRequest {
                    tenant_id: acme.clone(),
                    object: shared_channel.clone(),
                    permission: AuthzRelation::new("read"),
                    subject: bob.clone(),
                    consistency: AuthzConsistency::Strong,
                })
                .await
                .expect("acme read"),
            CheckDecision::Allowed
        );
        assert_eq!(
            backend
                .check(&CheckRequest {
                    tenant_id: globex.clone(),
                    object: shared_channel,
                    permission: AuthzRelation::new("read"),
                    subject: bob,
                    consistency: AuthzConsistency::Strong,
                })
                .await
                .expect("globex read"),
            CheckDecision::Denied
        );
    }
}
