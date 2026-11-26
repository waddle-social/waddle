//! T4-S01: Release Evidence Gate 1 — Multi-Tenant Backend Parity.
//! Exercises identical multi-tenant SaaS workloads across:
//!   Stack A: TursoTenantStore + OpenFgaPermissionBackend (over real HTTP)
//!   Stack B: Postgres/SqliteCompatPostgresStore + TenantSpiceDbPermissionBackend
//! verifying tenant isolation, colliding account/channel IDs, and module write boundaries.

use std::sync::Arc;

use chrono::Utc;
use tempfile::TempDir;
use uuid::Uuid;
use waddle_broker::authz::openfga::{
    InProcessOpenFgaServer, OpenFgaConfig, OpenFgaPermissionBackend,
};
use waddle_broker::authz::AuthzCallerRole;
use waddle_broker::custody::{BrokerCustodyService, EnrollCredentialInput};
use waddle_broker::key_hierarchy::KeyHierarchyService;
use waddle_broker::kms::local::LocalKmsProvider;
use waddle_broker::ledger::turso::TursoLedgerStore;
use waddle_broker::ledger::DestructionLedgerStore;
use waddle_broker::secret::SecretBytes;
use waddle_broker_core::identity::{
    AccountId, CellRole, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::permission::{
    AuthzConsistency, AuthzModule, AuthzObject, AuthzObjectType, AuthzRelation, AuthzSubject,
    AuthzSubjectType, AuthzTuple, CheckDecision, CheckRequest, PermissionBackend,
    PermissionBackendError,
};
use waddle_broker_core::placement::{FenceGeneration, PlacementLeaseStore, PlacementMode};
use waddle_broker_core::store::{TenantQueryScope, TenantStore};
use waddle_server::permissions::TenantSpiceDbPermissionBackend;
use waddle_tenancy::placement_postgres::SqliteCompatPlacementLeaseStore;
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::postgres_store::SqliteCompatPostgresStore;
use waddle_tenancy::turso_store::{TursoPoolConfig, TursoTenantStore};

fn holder(sa: &str, key: u8) -> HolderInstance {
    HolderInstance {
        spiffe_id: SpiffeId::parse(sa).expect("spiffe"),
        incarnation_pubkey: IncarnationPubKey::from_bytes([key; 32]),
        tls_exporter_proof: TlsExporterProof::new(vec![key; 32]).expect("proof"),
    }
}

async fn run_stack_workload(
    store: &dyn TenantStore,
    leases: &dyn PlacementLeaseStore,
    broker_perms: &dyn PermissionBackend,
    cell_perms: &dyn PermissionBackend,
    custody: &BrokerCustodyService,
) {
    let acme = TenantId::parse("acme.example").expect("acme");
    let globex = TenantId::parse("globex.example").expect("globex");
    let h = holder("spiffe://trust.example/ns/waddle/sa/cell-shared", 7);
    let role = CellRole::parse("cell-shared").expect("cell role");

    store.provision_tenant(&acme).await.expect("prov acme");
    store.provision_tenant(&globex).await.expect("prov globex");

    let lease_acme = leases
        .claim(&acme, &role, PlacementMode::SharedPool, &h, 30_000)
        .await
        .expect("lease acme");
    let lease_globex = leases
        .claim(&globex, &role, PlacementMode::SharedPool, &h, 30_000)
        .await
        .expect("lease globex");

    store
        .open_tenant(&TenantQueryScope {
            tenant_id: acme.clone(),
            holder: h.clone(),
            placement_generation: lease_acme.generation,
        })
        .await
        .expect("open acme");
    store
        .open_tenant(&TenantQueryScope {
            tenant_id: globex.clone(),
            holder: h,
            placement_generation: lease_globex.generation,
        })
        .await
        .expect("open globex");

    let ref_a = broker_perms
        .provision_tenant_store(&acme)
        .await
        .expect("prov authz acme");
    let ref_b = broker_perms
        .provision_tenant_store(&globex)
        .await
        .expect("prov authz globex");
    assert_ne!(ref_a.store_id, ref_b.store_id);

    let alice = AccountId::new_v4();
    let chan = AuthzObject {
        object_type: AuthzObjectType::Channel,
        id: "general".to_owned(),
    };
    let alice_subj = AuthzSubject {
        subject_type: AuthzSubjectType::Account,
        id: alice.to_string(),
        relation: None,
    };
    cell_perms
        .write_tuples(
            &acme,
            AuthzModule::Chat,
            &[AuthzTuple {
                object: chan.clone(),
                relation: AuthzRelation::new("member"),
                subject: alice_subj.clone(),
            }],
        )
        .await
        .expect("cell writes chat");

    // Cell CANNOT write inference tuples on either stack.
    let cred_id = Uuid::new_v4();
    assert!(matches!(
        cell_perms
            .write_tuples(
                &acme,
                AuthzModule::Inference,
                &[AuthzTuple {
                    object: AuthzObject {
                        object_type: AuthzObjectType::TenantCredential,
                        id: cred_id.to_string(),
                    },
                    relation: AuthzRelation::new("use"),
                    subject: alice_subj.clone(),
                }],
            )
            .await,
        Err(PermissionBackendError::ModuleWriteForbidden(
            AuthzModule::Inference
        ))
    ));

    // Same account + channel in acme vs globex: acme allowed, globex denied.
    assert_eq!(
        broker_perms
            .check(&CheckRequest {
                tenant_id: acme.clone(),
                object: chan.clone(),
                permission: AuthzRelation::new("read"),
                subject: alice_subj.clone(),
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("acme read"),
        CheckDecision::Allowed
    );
    assert_eq!(
        broker_perms
            .check(&CheckRequest {
                tenant_id: globex.clone(),
                object: chan,
                permission: AuthzRelation::new("read"),
                subject: alice_subj,
                consistency: AuthzConsistency::Strong,
            })
            .await
            .expect("globex read"),
        CheckDecision::Denied
    );

    // Credential custody isolation
    let c_acme = custody
        .enroll(EnrollCredentialInput {
            tenant_id: acme.clone(),
            account_id: Some(alice),
            provider: "openai".to_owned(),
            nonce: format!("parity-acme-{}", Uuid::new_v4()),
            payload_digest_sha256: [0x11; 32],
            expires_at_ms: Utc::now().timestamp_millis() + 60_000,
            plaintext: SecretBytes::new(b"sk-acme-parity".to_vec()).expect("secret"),
        })
        .await
        .expect("enroll acme");
    assert_eq!(
        custody
            .unseal_for_provider_egress(&acme, c_acme.credential_id, "openai")
            .await
            .expect("unseal acme")
            .as_bytes(),
        b"sk-acme-parity"
    );
    assert!(custody
        .unseal_for_provider_egress(&globex, c_acme.credential_id, "openai")
        .await
        .is_err());
    let _ = FenceGeneration::new(1);
}

#[tokio::test]
async fn e2e_gate1_backend_parity_turso_openfga_vs_postgres_spicedb() {
    let dir = TempDir::new().expect("tempdir");

    // Stack A: Turso + OpenFGA
    let turso_leases = TursoPlacementLeaseStore::open(&dir.path().join("leases_a.db"))
        .await
        .expect("turso leases");
    let turso_store = TursoTenantStore::new(
        dir.path().join("tenants_a"),
        TursoPoolConfig::default(),
        None,
    )
    .expect("turso store");
    let fga_server = InProcessOpenFgaServer::start().await.expect("openfga");
    let fga_broker = OpenFgaPermissionBackend::new(OpenFgaConfig {
        endpoint: fga_server.endpoint().to_owned(),
        bearer_token: None,
        caller_role: AuthzCallerRole::Broker,
        pinned_model_version: None,
    })
    .expect("openfga");
    let fga_cell = fga_broker.with_caller_role(AuthzCallerRole::Cell);
    let ledger_a = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("custody_a"))
            .await
            .expect("ledger a"),
    );
    let custody_a = BrokerCustodyService::new(
        Arc::new(KeyHierarchyService::new(
            "trust.example",
            Arc::new(LocalKmsProvider::new()),
        )),
        ledger_a as Arc<dyn DestructionLedgerStore>,
    )
    .await
    .expect("custody a");
    run_stack_workload(
        &turso_store,
        &turso_leases,
        &fga_broker,
        &fga_cell,
        &custody_a,
    )
    .await;

    // Stack B: Postgres/SqliteCompat + SpiceDB
    let pg_leases = SqliteCompatPlacementLeaseStore::new_in_memory()
        .await
        .expect("pg leases");
    let pg_store = SqliteCompatPostgresStore::new_in_memory(None)
        .await
        .expect("pg store");
    let spice_broker = TenantSpiceDbPermissionBackend::in_memory(AuthzCallerRole::Broker);
    let spice_cell = spice_broker.with_caller_role(AuthzCallerRole::Cell);
    let ledger_b = Arc::new(
        TursoLedgerStore::open_in_dir(&dir.path().join("custody_b"))
            .await
            .expect("ledger b"),
    );
    let custody_b = BrokerCustodyService::new(
        Arc::new(KeyHierarchyService::new(
            "trust.example",
            Arc::new(LocalKmsProvider::new()),
        )),
        ledger_b as Arc<dyn DestructionLedgerStore>,
    )
    .await
    .expect("custody b");
    run_stack_workload(
        &pg_store,
        &pg_leases,
        &spice_broker,
        &spice_cell,
        &custody_b,
    )
    .await;
}
