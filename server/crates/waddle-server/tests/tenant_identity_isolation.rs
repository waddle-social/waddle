//! T1-F06-05: Identical localparts across two tenants hold distinct immutable `AccountId` UUIDs.

use std::sync::Arc;

use tempfile::tempdir;
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, CellRole, SpiffeId, TenantId};
use waddle_broker_core::placement::{FenceGeneration, PlacementLeaseStore, PlacementMode};
use waddle_broker_core::quotas::TenantResourceQuotas;
use waddle_broker_core::store::TenantQueryScope;
use waddle_server::db::tenant_registry::ServerTenantRegistry;
use waddle_server::db::turso_tenant_store::ServerTursoTenantStore;
use waddle_tenancy::data_ops::{TenantDataOps, TenantUserRecord};
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::turso_store::TursoPoolConfig;

#[tokio::test]
async fn identical_localparts_across_tenants_hold_distinct_immutable_account_ids() {
    let dir = tempdir().expect("tempdir");
    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("placement.db"))
            .await
            .expect("open placement lease db"),
    );
    let server_store = ServerTursoTenantStore::new(
        dir.path().join("tenants"),
        TursoPoolConfig {
            rlimit_nofile_override: Some(64),
            reserved_fds: 16,
        },
        Some(Arc::clone(&lease_store) as Arc<dyn PlacementLeaseStore>),
    )
    .expect("construct ServerTursoTenantStore");
    let registry = ServerTenantRegistry::new(
        server_store.clone().into_store_arc(),
        None,
        TenantResourceQuotas::default(),
    )
    .expect("construct ServerTenantRegistry");

    let tenant_acme = TenantId::parse("acme.example").expect("valid acme tenant");
    let tenant_globex = TenantId::parse("globex.example").expect("valid globex tenant");
    registry
        .provision_tenant(&tenant_acme)
        .await
        .expect("provision acme");
    registry
        .provision_tenant(&tenant_globex)
        .await
        .expect("provision globex");

    let spiffe =
        SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/shared-pool").expect("valid spiffe");
    let role = CellRole::parse("shared-pool").expect("valid role");
    let holder = HolderKeypair::generate()
        .expect("holder keypair")
        .sign_channel_binding(&spiffe, &[0x21; 32]);

    let lease_acme = lease_store
        .claim(
            &tenant_acme,
            &role,
            PlacementMode::SharedPool,
            &holder,
            10_000,
        )
        .await
        .expect("claim acme lease");
    let lease_globex = lease_store
        .claim(
            &tenant_globex,
            &role,
            PlacementMode::SharedPool,
            &holder,
            10_000,
        )
        .await
        .expect("claim globex lease");

    let scope_acme = TenantQueryScope {
        tenant_id: tenant_acme.clone(),
        holder: holder.clone(),
        placement_generation: lease_acme.generation,
    };
    let scope_globex = TenantQueryScope {
        tenant_id: tenant_globex.clone(),
        holder,
        placement_generation: lease_globex.generation,
    };

    let guard_acme = registry
        .acquire_tenant(&scope_acme)
        .await
        .expect("acquire acme");
    let guard_globex = registry
        .acquire_tenant(&scope_globex)
        .await
        .expect("acquire globex");
    assert_eq!(guard_acme.tenant_id(), &tenant_acme);
    assert_eq!(guard_globex.tenant_id(), &tenant_globex);

    let acme_account = AccountId::new_v4();
    let globex_account = AccountId::new_v4();
    assert_ne!(acme_account, globex_account);

    let store_ops = server_store.inner();
    store_ops
        .upsert_user(
            &scope_acme,
            &TenantUserRecord {
                account_id: acme_account,
                jid: "alice@acme.example".to_owned(),
                username: "alice".to_owned(),
                xmpp_localpart: "alice".to_owned(),
                display_name: Some("Alice (Acme)".to_owned()),
            },
        )
        .await
        .expect("upsert alice in acme");
    store_ops
        .upsert_user(
            &scope_globex,
            &TenantUserRecord {
                account_id: globex_account,
                jid: "alice@globex.example".to_owned(),
                username: "alice".to_owned(),
                xmpp_localpart: "alice".to_owned(),
                display_name: Some("Alice (Globex)".to_owned()),
            },
        )
        .await
        .expect("upsert alice in globex");

    let fetched_acme = store_ops
        .get_user_by_localpart(&scope_acme, "alice")
        .await
        .expect("query acme")
        .expect("alice exists in acme");
    let fetched_globex = store_ops
        .get_user_by_localpart(&scope_globex, "alice")
        .await
        .expect("query globex")
        .expect("alice exists in globex");

    assert_eq!(fetched_acme.account_id, acme_account);
    assert_eq!(fetched_globex.account_id, globex_account);
    assert_ne!(fetched_acme.account_id, fetched_globex.account_id);
    assert_eq!(fetched_acme.display_name.as_deref(), Some("Alice (Acme)"));
    assert_eq!(
        fetched_globex.display_name.as_deref(),
        Some("Alice (Globex)")
    );

    let mutate_err = store_ops
        .update_user_account_id_for_test(&scope_acme, "alice@acme.example", AccountId::new_v4())
        .await
        .expect_err("account_id trigger must reject mutation");
    assert!(
        mutate_err.to_string().contains("immutable"),
        "expected immutability trigger error, got: {mutate_err}"
    );
    assert_eq!(
        guard_acme.scope().placement_generation,
        FenceGeneration::new(1)
    );
}
