//! T1-F10-04: Stale-generation fencing on data writes, outbox/room_observation, SQL txs, and CommandRegistry.

use std::sync::Arc;
use std::time::Duration;

use tempfile::tempdir;
use waddle_broker_core::boot_clock::{BootClock, FakeBootClock};
use waddle_broker_core::holder_pop::HolderKeypair;
use waddle_broker_core::identity::{AccountId, CellRole, SpiffeId, TenantId};
use waddle_broker_core::placement::{FenceGeneration, PlacementLeaseStore, PlacementMode};
use waddle_broker_core::store::{TenantQueryScope, TenantStore, TenantStoreError};
use waddle_server::db::placement_fence::assert_transaction_placement_lease;
use waddle_server::db::turso_tenant_store::ServerTursoTenantStore;
use waddle_server::db::{Database, MigrationRunner};
use waddle_server::db_params;
use waddle_server::room_effect_outbox::check_room_effect_lease_fence;
use waddle_tenancy::data_ops::{TenantDataOps, TenantUserRecord};
use waddle_tenancy::placement_turso::TursoPlacementLeaseStore;
use waddle_tenancy::quiesce::LocalLeaseGuard;
use waddle_tenancy::turso_store::TursoPoolConfig;
use waddle_xmpp::commands::{Action, Command, CommandContext, CommandRegistry, CommandResult};
use xmpp_parsers::iq::Iq;

#[tokio::test]
async fn stale_generation_fencing_blocks_turso_sql_outbox_and_command_workers() {
    let dir = tempdir().expect("tempdir");
    let lease_store = Arc::new(
        TursoPlacementLeaseStore::open(&dir.path().join("placement.db"))
            .await
            .expect("open placement db"),
    );
    let tenant_store = ServerTursoTenantStore::new(
        dir.path().join("tenants"),
        TursoPoolConfig {
            rlimit_nofile_override: Some(64),
            reserved_fds: 16,
        },
        Some(Arc::clone(&lease_store) as Arc<dyn PlacementLeaseStore>),
    )
    .expect("construct store");

    let tenant = TenantId::parse("acme.example").expect("valid tenant");
    tenant_store
        .provision_tenant(&tenant)
        .await
        .expect("provision tenant");

    let spiffe_a =
        SpiffeId::parse("spiffe://waddle.internal/ns/cell-a/sa/dedicated").expect("spiffe a");
    let spiffe_b =
        SpiffeId::parse("spiffe://waddle.internal/ns/cell-b/sa/dedicated").expect("spiffe b");
    let role = CellRole::parse("dedicated").expect("role");
    let holder_a = HolderKeypair::generate()
        .expect("kp a")
        .sign_channel_binding(&spiffe_a, &[0x11; 32]);
    let holder_b = HolderKeypair::generate()
        .expect("kp b")
        .sign_channel_binding(&spiffe_b, &[0x22; 32]);

    let lease1 = lease_store
        .claim(
            &tenant,
            &role,
            PlacementMode::DedicatedCell,
            &holder_a,
            10_000,
        )
        .await
        .expect("claim gen 1");
    let scope_gen1 = TenantQueryScope {
        tenant_id: tenant.clone(),
        holder: holder_a.clone(),
        placement_generation: lease1.generation,
    };
    tenant_store
        .open_tenant(&scope_gen1)
        .await
        .expect("open gen 1");

    // Transfer lease to holder_b at generation 2.
    let lease2 = lease_store
        .transfer(&tenant, &holder_a, &holder_b, lease1.generation, 10_000)
        .await
        .expect("transfer gen 2");
    assert_eq!(lease2.generation, FenceGeneration::new(2));
    let scope_gen2 = TenantQueryScope {
        tenant_id: tenant.clone(),
        holder: holder_b.clone(),
        placement_generation: lease2.generation,
    };
    tenant_store
        .open_tenant(&scope_gen2)
        .await
        .expect("open gen 2");

    // 1. Turso data write + write fence reject stale generation 1.
    let stale_write = tenant_store
        .inner()
        .upsert_user(
            &scope_gen1,
            &TenantUserRecord {
                account_id: AccountId::new_v4(),
                jid: "stale@acme.example".to_owned(),
                username: "stale".to_owned(),
                xmpp_localpart: "stale".to_owned(),
                display_name: None,
            },
        )
        .await
        .expect_err("stale generation write must fail");
    assert!(
        matches!(
            stale_write,
            TenantStoreError::StalePlacementGeneration { .. }
        ),
        "expected StalePlacementGeneration, got {stale_write:?}"
    );
    tenant_store
        .verify_write_fence(&scope_gen2)
        .await
        .expect("active generation 2 write fence passes");

    // 2. Server SQLite transaction placement fence rejects gen 1 while allowing gen 2.
    let db = Database::in_memory("fence_sql")
        .await
        .expect("in-memory sqlite db");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrations");
    let conn = db.guard().await.expect("db guard");
    conn.execute(
        "INSERT INTO tenant_placement_leases
         (tenant_id, cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
          holder_tls_exporter_proof, generation, expires_at_ms, transfer_state, updated_at_ms)
         VALUES (?1, 'dedicated', 'dedicated_cell', ?2, ?3, ?4, 2,
                 CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER) + 60000,
                 'active',
                 CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER))",
        db_params![
            tenant.as_str().to_owned(),
            holder_b.spiffe_id.as_str().to_owned(),
            holder_b.incarnation_pubkey.as_bytes().to_vec(),
            holder_b.tls_exporter_proof.as_bytes().to_vec()
        ],
    )
    .await
    .expect("seed active gen 2 lease row");
    drop(conn);

    let mut tx = db.begin().await.expect("begin tx");
    assert!(assert_transaction_placement_lease(&mut tx, &scope_gen1)
        .await
        .is_err());
    assert!(assert_transaction_placement_lease(&mut tx, &scope_gen2)
        .await
        .is_ok());
    tx.rollback().await.expect("rollback");

    // 3. Outbox / room_observation local guard fails closed upon revocation or clock jump.
    let clock = Arc::new(FakeBootClock::new(Duration::from_secs(10)));
    let guard = LocalLeaseGuard::from_sent(
        Arc::clone(&clock) as Arc<dyn BootClock>,
        tenant.clone(),
        lease1.generation,
        clock.now().expect("now"),
        Duration::from_secs(10),
        Duration::from_secs(2),
    )
    .expect("local lease guard");
    assert!(check_room_effect_lease_fence(Some(&guard)));
    guard.revoke_locally();
    assert!(!check_room_effect_lease_fence(Some(&guard)));

    // 4. CommandRegistry rejects gen 1 workers once generation advances to 2.
    let cmd_reg = CommandRegistry::for_tenant_with_clock(
        tenant.clone(),
        Some(1),
        clock as Arc<dyn BootClock>,
    );
    cmd_reg
        .register("acme:op", "Op", |ctx| async {
            CommandResult::Completed {
                session_id: ctx.command.session_id,
                form: None,
                notes: vec![],
            }
        })
        .await;
    cmd_reg.set_placement_generation(Some(2)).await;
    let stale_cmd = cmd_reg
        .dispatch(CommandContext {
            from: "alice@acme.example/web".parse().expect("jid"),
            authenticated_user_id: Some("alice@acme.example".to_owned()),
            iq: Iq::Set {
                from: None,
                to: None,
                id: "iq-stale-1".to_owned(),
                payload: "<query xmlns='urn:test'/>".parse().expect("xml"),
            },
            command: Command::new("acme:op").with_action(Action::Execute),
            tenant_id: Some(tenant),
            placement_generation: Some(1),
            cancel_rx: None,
            chunk_tx: None,
        })
        .await;
    assert!(matches!(stale_cmd, CommandResult::Error(_)));
}
