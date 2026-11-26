//! T1-F08-05: Federated `BareJid` caches never leak state across tenants.

use std::sync::Arc;

use jid::BareJid;
use tempfile::tempdir;
use waddle_broker_core::identity::TenantId;
use waddle_broker_core::quotas::TenantResourceQuotas;
use waddle_broker_core::tenant_cache::TenantScopedCache;
use waddle_server::db::tenant_registry::ServerTenantRegistry;
use waddle_server::db::turso_tenant_store::ServerTursoTenantStore;
use waddle_tenancy::turso_store::TursoPoolConfig;

#[test]
fn federated_barejid_caches_never_leak_across_tenants() {
    let dir = tempdir().expect("tempdir");
    let store = ServerTursoTenantStore::new(
        dir.path().join("tenants"),
        TursoPoolConfig {
            rlimit_nofile_override: Some(64),
            reserved_fds: 16,
        },
        None,
    )
    .expect("construct store");
    let registry = ServerTenantRegistry::new(
        Arc::clone(store.inner()) as _,
        None,
        TenantResourceQuotas::default(),
    )
    .expect("construct registry");

    let tenant_acme = TenantId::parse("acme.example").expect("valid acme tenant");
    let tenant_globex = TenantId::parse("globex.example").expect("valid globex tenant");
    let remote_jid: BareJid = "bob@remote.example".parse().expect("valid remote bare jid");

    let fed_cache: TenantScopedCache<BareJid, String> = TenantScopedCache::new();
    fed_cache.insert(
        tenant_acme.clone(),
        remote_jid.clone(),
        "caps:acme:sha256-aaa".to_owned(),
    );
    fed_cache.insert(
        tenant_globex.clone(),
        remote_jid.clone(),
        "caps:globex:sha256-bbb".to_owned(),
    );

    assert_eq!(
        fed_cache.get_cloned(&tenant_acme, &remote_jid).as_deref(),
        Some("caps:acme:sha256-aaa")
    );
    assert_eq!(
        fed_cache.get_cloned(&tenant_globex, &remote_jid).as_deref(),
        Some("caps:globex:sha256-bbb")
    );

    // Clearing Acme's federated cache must leave Globex's entry untouched.
    assert_eq!(fed_cache.clear_tenant(&tenant_acme), 1);
    assert!(fed_cache.get_cloned(&tenant_acme, &remote_jid).is_none());
    assert_eq!(
        fed_cache.get_cloned(&tenant_globex, &remote_jid).as_deref(),
        Some("caps:globex:sha256-bbb")
    );
    assert_eq!(fed_cache.len_for_tenant(&tenant_acme), 0);
    assert_eq!(fed_cache.len_for_tenant(&tenant_globex), 1);

    // ServerTenantRegistry's metadata cache must also enforce composite TenantKey isolation.
    let meta_cache = registry.inner().metadata_cache();
    meta_cache.insert(
        tenant_acme.clone(),
        remote_jid.to_string(),
        "disco-acme".to_owned(),
    );
    meta_cache.insert(
        tenant_globex.clone(),
        remote_jid.to_string(),
        "disco-globex".to_owned(),
    );
    assert_eq!(
        meta_cache
            .get_cloned(&tenant_acme, &remote_jid.to_string())
            .as_deref(),
        Some("disco-acme")
    );
    assert_eq!(
        meta_cache
            .get_cloned(&tenant_globex, &remote_jid.to_string())
            .as_deref(),
        Some("disco-globex")
    );
}
