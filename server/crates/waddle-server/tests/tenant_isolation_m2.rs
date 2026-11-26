use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use waddle_broker_core::boot_clock::{BootClock, FakeBootClock};
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{CellRole, SpiffeId, TenantId};
use waddle_broker_core::placement::{FenceGeneration, PlacementLeaseStore, PlacementMode};
use waddle_broker_core::quotas::TenantResourceQuotas;
use waddle_broker_core::store::TenantQueryScope;
use waddle_server::db::placement_fence::assert_transaction_placement_lease;
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::room_effect_outbox::check_room_effect_lease_fence;
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::quiesce::LocalLeaseGuard;
use waddle_tenancy::registry::TenantRegistry;
use waddle_tenancy::turso_store::{TursoPoolConfig, TursoTenantStore};
use waddle_xmpp::commands::{CommandContext, CommandRegistry, CommandResult};
use waddle_xmpp::xep::xep0004::{DataForm, FormType};
use waddle_xmpp::xep::xep0050::{Action, AdHocCommandCondition, Command};
use waddle_xmpp::XmppError;
use xmpp_parsers::iq::Iq;

#[tokio::test]
async fn migration_2026_is_idempotent_and_enforces_immutable_account_id_and_placement_fence() {
    let db = Database::in_memory("m2_migration_test")
        .await
        .expect("in-memory sqlite db");
    let runner = MigrationRunner::single();
    runner.run(&db).await.expect("first migration run");
    // Re-running must be idempotent.
    runner.run(&db).await.expect("second idempotent run");

    let conn = db.guard().await.expect("db guard");
    // Insert a user with an account_id and verify the immutability trigger blocks mutation.
    conn.execute(
        "INSERT INTO users (jid, username, xmpp_localpart, account_id, tenant_id, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        waddle_server::db_params![
            "alice@tenant-a.example",
            "alice",
            "alice",
            "11111111-1111-4111-8111-111111111111",
            "tenant-a.example"
        ],
    )
    .await
    .expect("insert user");

    let mut_err = conn
        .execute(
            "UPDATE users SET account_id = ?1 WHERE jid = ?2",
            waddle_server::db_params![
                "22222222-2222-4222-8222-222222222222",
                "alice@tenant-a.example"
            ],
        )
        .await
        .expect_err("users.account_id immutability trigger must fire");
    assert!(
        mut_err.to_string().contains("immutable"),
        "expected immutable error, got: {mut_err}"
    );

    // Insert a native_user and verify its immutability trigger also fires.
    conn.execute(
        "INSERT INTO native_users (id, username, domain, password_hash, salt, iterations, stored_key, server_key, account_id, tenant_id, created_at, updated_at)
         VALUES (1, 'bob', 'tenant-a.example', 'h', 's', 4096, X'0102', X'0304', '33333333-3333-4333-8333-333333333333', 'tenant-a.example', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        waddle_server::db_params![],
    )
    .await
    .expect("insert native_user");

    let native_err = conn
        .execute(
            "UPDATE native_users SET account_id = ?1 WHERE id = 1",
            waddle_server::db_params!["44444444-4444-4444-8444-444444444444"],
        )
        .await
        .expect_err("native_users.account_id immutability trigger must fire");
    assert!(
        native_err.to_string().contains("immutable"),
        "expected immutable error, got: {native_err}"
    );

    // Verify transactional placement fence against migration 2026's `tenant_placement_leases` table.
    let tenant = TenantId::parse("tenant-a.example").expect("valid");
    let spiffe =
        SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/shared-pool").expect("valid spiffe");
    let kp = HolderKeypair::generate().expect("kp");
    let holder = kp.sign_channel_binding(&spiffe, &[0x42; 32]);
    let scope_gen1 = TenantQueryScope {
        tenant_id: tenant.clone(),
        holder: holder.clone(),
        placement_generation: FenceGeneration::new(1),
    };
    let scope_gen2 = TenantQueryScope {
        tenant_id: tenant.clone(),
        holder: holder.clone(),
        placement_generation: FenceGeneration::new(2),
    };

    conn.execute(
        "INSERT INTO tenant_placement_leases
         (tenant_id, cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
          holder_tls_exporter_proof, generation, expires_at_ms, transfer_state, updated_at_ms)
         VALUES (?1, 'shared-pool', 'shared_pool', ?2, ?3, ?4, 2,
                 CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER) + 60000,
                 'active',
                 CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER))",
        waddle_server::db_params![
            tenant.as_str().to_owned(),
            holder.spiffe_id.as_str().to_owned(),
            holder.incarnation_pubkey.as_bytes().to_vec(),
            holder.tls_exporter_proof.as_bytes().to_vec()
        ],
    )
    .await
    .expect("insert active lease");

    drop(conn);
    let mut tx = db.begin().await.expect("begin tx");
    assert!(assert_transaction_placement_lease(&mut tx, &scope_gen2)
        .await
        .is_ok());
    assert!(assert_transaction_placement_lease(&mut tx, &scope_gen1)
        .await
        .is_err());
    tx.rollback().await.expect("rollback");
}

#[tokio::test]
async fn end_to_end_tenant_registry_boot_clock_and_command_registry_fencing() {
    let dir = tempdir().expect("tempdir");
    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("placement.db"))
            .await
            .expect("open placement db"),
    );
    let tenant_store = Arc::new(
        TursoTenantStore::new(
            dir.path().join("tenants"),
            TursoPoolConfig {
                rlimit_nofile_override: Some(64),
                reserved_fds: 16,
            },
            Some(Arc::clone(&lease_store) as Arc<dyn PlacementLeaseStore>),
        )
        .expect("turso store"),
    );
    let registry = TenantRegistry::new(tenant_store, None, TenantResourceQuotas::default())
        .expect("tenant registry");

    let tenant = TenantId::parse("acme.example").expect("valid");
    registry.provision_tenant(&tenant).await.expect("provision");

    let spiffe =
        SpiffeId::parse("spiffe://waddle.internal/ns/cells/sa/shared-pool").expect("valid spiffe");
    let role = CellRole::parse("shared-pool").expect("role");
    let holder = HolderKeypair::generate()
        .expect("kp")
        .sign_channel_binding(&spiffe, &[0x77; 32]);
    let lease = lease_store
        .claim(&tenant, &role, PlacementMode::SharedPool, &holder, 10_000)
        .await
        .expect("claim");

    let scope = TenantQueryScope {
        tenant_id: tenant.clone(),
        holder,
        placement_generation: lease.generation,
    };
    let guard = registry.acquire_tenant(&scope).await.expect("acquire");
    assert_eq!(guard.tenant_id(), &tenant);

    // BootInstant local guard + room_effect_outbox fence.
    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(500)));
    let local_guard = LocalLeaseGuard::from_sent(
        Arc::clone(&clock) as Arc<dyn BootClock>,
        tenant.clone(),
        lease.generation,
        clock.now().expect("now"),
        Duration::from_secs(10),
        Duration::from_secs(2),
    )
    .expect("local guard");
    assert!(check_room_effect_lease_fence(Some(&local_guard)));

    // CommandRegistry wizard fenced to tenant + generation 1.
    let cmd_registry = CommandRegistry::for_tenant_with_clock(
        tenant.clone(),
        Some(lease.generation.get()),
        Arc::clone(&clock) as Arc<dyn BootClock>,
    );
    cmd_registry
        .register("acme:wizard", "Wizard", |ctx: CommandContext| async move {
            CommandResult::Executing {
                form: DataForm::new(FormType::Form),
                session_id: ctx.command.session_id.unwrap_or_default(),
                notes: vec![],
                actions: None,
            }
        })
        .await;

    let session_id = match cmd_registry
        .dispatch(CommandContext {
            from: "alice@acme.example/web".parse().expect("jid"),
            authenticated_user_id: Some("alice@acme.example".to_owned()),
            iq: Iq::Set {
                from: None,
                to: None,
                id: "iq-1".to_owned(),
                payload: "<query xmlns='urn:test'/>".parse().expect("xml"),
            },
            command: Command::new("acme:wizard").with_action(Action::Execute),
            tenant_id: Some(tenant.clone()),
            placement_generation: Some(lease.generation.get()),
            cancel_rx: None,
            chunk_tx: None,
        })
        .await
    {
        CommandResult::Executing { session_id, .. } => session_id,
        _ => panic!("expected executing"),
    };

    // Simulate host suspend past local lease validity -> room_effect fence drops immediately.
    clock.advance_suspend(Duration::from_secs(9));
    assert!(!check_room_effect_lease_fence(Some(&local_guard)));

    // Evict tenant on drain/lease loss -> wizard continuation returns <session-expired/>.
    assert_eq!(cmd_registry.evict_tenant(&tenant).await, 1);
    let res = cmd_registry
        .dispatch(CommandContext {
            from: "alice@acme.example/web".parse().expect("jid"),
            authenticated_user_id: Some("alice@acme.example".to_owned()),
            iq: Iq::Set {
                from: None,
                to: None,
                id: "iq-2".to_owned(),
                payload: "<query xmlns='urn:test'/>".parse().expect("xml"),
            },
            command: Command::new("acme:wizard")
                .with_action(Action::Next)
                .with_session_id(session_id),
            tenant_id: Some(tenant),
            placement_generation: Some(lease.generation.get()),
            cancel_rx: None,
            chunk_tx: None,
        })
        .await;
    assert!(matches!(
        res,
        CommandResult::Error(XmppError::AdHocCommand {
            condition: AdHocCommandCondition::SessionExpired,
            ..
        })
    ));
}
