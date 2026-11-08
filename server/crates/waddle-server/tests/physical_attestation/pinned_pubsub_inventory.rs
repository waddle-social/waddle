//! Pinned relation-inventory witnesses for the combined PubSub fixture.

use std::time::Duration;

use anyhow::ensure;
use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgConnectOptions, PgPool};
use sqlx::Row;
use uuid::Uuid;
use waddle_server::db::physical_attestation::{
    attested_pool, pinned_attested_pool, DarkPhysicalAttestationPins, PhysicalAttestationConfig,
    PinnedPubSubInventoryError, PinnedPubSubRelationInventory, PinnedRuntimeAuthority,
    PinnedSelectedEntry, CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::DarkMigrationLedgerPins;
use waddle_server::pubsub::DatabasePubSubStorage;

pub(super) async fn pinned_pubsub_inventory(
    admin: &PgPool,
    runtime_options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
    installation_operation: Uuid,
    roles: (&str, &str, &str),
    relation_oids: [u32; 12],
) -> anyhow::Result<InventoryFixture> {
    // The administrator records object identities independently of the runtime
    // connection before introducing any of the drift cases below.
    let pins = sqlx::query(
        "SELECT db.datdba, n.nspowner, r.oid, i.oid, d.oid, trusted.oid, \
                lineage.oid, lineage_idx.indexrelid, attestor.oid, n.oid, \
                ledger.oid, ledger_idx.indexrelid, epoch.oid, epoch_idx.indexrelid, manifest.oid \
         FROM pg_catalog.pg_database db \
         JOIN pg_catalog.pg_namespace n ON n.nspname='guarded' \
         JOIN pg_catalog.pg_class lineage ON lineage.relnamespace=n.oid AND lineage.relname='_lineage' \
         JOIN pg_catalog.pg_index lineage_idx ON lineage_idx.indrelid=lineage.oid AND lineage_idx.indisprimary \
         JOIN pg_catalog.pg_namespace trusted ON trusted.nspname='attestor' \
         JOIN pg_catalog.pg_proc attestor ON attestor.pronamespace=trusted.oid AND attestor.proname='read_physical_identity' \
         JOIN pg_catalog.pg_roles r ON r.rolname=$1 \
         JOIN pg_catalog.pg_roles i ON i.rolname=$2 \
         JOIN pg_catalog.pg_roles d ON d.rolname=$3 \
         JOIN pg_catalog.pg_class ledger ON ledger.relnamespace=n.oid AND ledger.relname='_migrations' \
         JOIN pg_catalog.pg_index ledger_idx ON ledger_idx.indrelid=ledger.oid AND ledger_idx.indisprimary \
         JOIN pg_catalog.pg_class epoch ON epoch.relnamespace=n.oid AND epoch.relname='ingress_protocol_epoch' \
         JOIN pg_catalog.pg_index epoch_idx ON epoch_idx.indrelid=epoch.oid AND epoch_idx.indisprimary \
         JOIN pg_catalog.pg_class manifest ON manifest.relnamespace=n.oid AND manifest.relname='ingress_epoch_guard_manifest' \
         WHERE db.datname=pg_catalog.current_database()",
    )
    .bind(roles.0)
    .bind(roles.1)
    .bind(roles.2)
    .fetch_one(admin)
    .await?;
    let oid = |index| pins.get::<Oid, _>(index).0;
    let attestor = DarkPhysicalAttestationPins {
        database_owner_role_oid: oid(0),
        schema_owner_role_oid: oid(1),
        runtime_role_oid: oid(2),
        installer_role_oid: oid(3),
        definer_role_oid: oid(4),
        trusted_schema_oid: oid(5),
        lineage_relation_oid: oid(6),
        lineage_primary_index_oid: oid(7),
        attestor_function_oid: oid(8),
        installation_operation_id: installation_operation,
    };
    let ledger = DarkMigrationLedgerPins {
        schema_oid: oid(9),
        schema_owner_role_oid: oid(1),
        runtime_role_oid: oid(2),
        ledger_relation_oid: oid(10),
        ledger_primary_index_oid: oid(11),
    };
    let authority =
        PinnedRuntimeAuthority::new(config, attestor, ledger, oid(12), oid(13), oid(14))?;
    let fixture = InventoryFixture {
        inventory: PinnedPubSubRelationInventory::new(
            pinned_attested_pool(
                runtime_options.clone(),
                config.clone(),
                authority.clone(),
                1,
                Duration::from_secs(5),
            )?,
            relation_oids,
        )?,
        runtime_options: runtime_options.clone(),
        config: config.clone(),
        authority,
        relation_oids,
    };
    let legacy_pool = attested_pool(
        runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(5),
    )?;
    ensure!(
        matches!(
            PinnedPubSubRelationInventory::new(legacy_pool, relation_oids),
            Err(PinnedPubSubInventoryError::InvalidPins)
        ),
        "legacy pool was accepted as pinned"
    );
    let mut stale = relation_oids;
    stale[6] = u32::MAX;
    let stale_inventory = fixture.new_with_oids(stale)?;
    ensure!(
        matches!(
            DatabasePubSubStorage::start_pinned_relation_inventory(&stale_inventory)?
                .finish()
                .await,
            Err(PinnedPubSubInventoryError::CatalogDrift {
                relation: "pubsub_nodes"
            })
        ),
        "stale PubSub relation pin was accepted"
    );
    Ok(fixture)
}

pub(super) struct InventoryFixture {
    pub(super) inventory: PinnedPubSubRelationInventory,
    runtime_options: PgConnectOptions,
    config: PhysicalAttestationConfig,
    authority: PinnedRuntimeAuthority,
    relation_oids: [u32; 12],
}

impl InventoryFixture {
    pub(super) fn selected(&self) -> anyhow::Result<PinnedSelectedEntry> {
        Ok(PinnedSelectedEntry::new(pinned_attested_pool(
            self.runtime_options.clone(),
            self.config.clone(),
            self.authority.clone(),
            1,
            Duration::from_secs(5),
        )?)?)
    }

    fn new_with_oids(
        &self,
        relation_oids: [u32; 12],
    ) -> anyhow::Result<PinnedPubSubRelationInventory> {
        let pool = pinned_attested_pool(
            self.runtime_options.clone(),
            self.config.clone(),
            self.authority.clone(),
            1,
            Duration::from_secs(5),
        )?;
        Ok(PinnedPubSubRelationInventory::new(pool, relation_oids)?)
    }
}

async fn holds_relation(
    admin: &PgPool,
    pid: i32,
    relation: Oid,
    mode: &str,
) -> anyhow::Result<bool> {
    Ok(sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 \
         AND locktype='relation' AND relation=$2 AND mode=$3 AND granted)",
    )
    .bind(pid)
    .bind(relation)
    .bind(mode)
    .fetch_one(admin)
    .await?)
}

pub(super) async fn exercise_pinned_pubsub(
    admin: &PgPool,
    runtime: &str,
    owner: &str,
    alternate_owner: &str,
    fixture: &InventoryFixture,
) -> anyhow::Result<()> {
    let inventory = &fixture.inventory;
    let relation_oids = fixture.relation_oids;
    ensure!(
        PinnedPubSubRelationInventory::relation_names_for_test()
            == DatabasePubSubStorage::runtime_attach_relation_names_for_test().as_slice(),
        "pinned inventory names differ from the PubSub runtime table inventory"
    );
    let epoch: Oid =
        sqlx::query_scalar("SELECT 'guarded.ingress_protocol_epoch'::pg_catalog.regclass::oid")
            .fetch_one(admin)
            .await?;
    let lineage: Oid = sqlx::query_scalar("SELECT 'guarded._lineage'::pg_catalog.regclass::oid")
        .fetch_one(admin)
        .await?;
    let ledger: Oid = sqlx::query_scalar("SELECT 'guarded._migrations'::pg_catalog.regclass::oid")
        .fetch_one(admin)
        .await?;
    let manifest: Oid = sqlx::query_scalar(
        "SELECT 'guarded.ingress_epoch_guard_manifest'::pg_catalog.regclass::oid",
    )
    .fetch_one(admin)
    .await?;
    let mut blocker = admin.begin().await?;
    sqlx::query("LOCK TABLE ONLY guarded._migrations IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *blocker)
        .await?;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let job = DatabasePubSubStorage::start_pinned_relation_inventory_paused_for_test(
        inventory, ready_tx, release_rx,
    )?;
    let waiting_pid = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_locks WHERE locktype='relation' \
                 AND relation=$1 AND mode='AccessShareLock' AND NOT granted LIMIT 1",
            )
            .bind(ledger)
            .fetch_optional(admin)
            .await?;
            if let Some(pid) = pid {
                return Ok::<_, sqlx::Error>(pid);
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??;
    ensure!(holds_relation(admin, waiting_pid, lineage, "AccessShareLock").await?);
    ensure!(!holds_relation(admin, waiting_pid, epoch, "AccessShareLock").await?);
    ensure!(!holds_relation(admin, waiting_pid, Oid(relation_oids[0]), "AccessShareLock").await?);
    blocker.rollback().await?;
    let (pid, read_only, read_committed) =
        tokio::time::timeout(Duration::from_secs(8), ready_rx).await??;
    ensure!(
        pid == waiting_pid && read_only && read_committed,
        "inventory left its exact read-only backend"
    );
    for relation in [lineage, ledger, epoch, manifest] {
        ensure!(
            holds_relation(admin, pid, relation, "AccessShareLock").await?,
            "missing ordered barrier"
        );
    }
    for relation in relation_oids {
        ensure!(
            holds_relation(admin, pid, Oid(relation), "AccessShareLock").await?,
            "missing PubSub lock"
        );
    }
    let exclusive: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .fetch_one(admin)
        .await?;
    ensure!(
        !exclusive,
        "catalog fence was not held during the store inspection"
    );
    release_tx
        .send(())
        .map_err(|_| anyhow::anyhow!("release diagnostic"))?;
    job.finish().await?;
    let idle: bool = sqlx::query_scalar(
        "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
    )
    .bind(pid)
    .fetch_one(admin)
    .await?;
    ensure!(idle, "success preceded acknowledgement of rollback");

    // The ordinary production entry point selects the same transaction path.
    DatabasePubSubStorage::start_pinned_relation_inventory(inventory)?
        .finish()
        .await?;

    // The worker retains its permit and transaction even after its caller
    // drops the result receiver; only release and acknowledged rollback allow
    // a subsequent diagnostic.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let abandoned = DatabasePubSubStorage::start_pinned_relation_inventory_paused_for_test(
        inventory, ready_tx, release_rx,
    )?;
    let (cancelled_pid, _, _) = tokio::time::timeout(Duration::from_secs(8), ready_rx).await??;
    drop(abandoned);
    ensure!(
        matches!(
            DatabasePubSubStorage::start_pinned_relation_inventory(inventory),
            Err(PinnedPubSubInventoryError::Overloaded)
        ),
        "dropping the receiver released admission while its transaction was active"
    );
    release_tx
        .send(())
        .map_err(|_| anyhow::anyhow!("release abandoned diagnostic"))?;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let idle: Option<bool> = sqlx::query_scalar(
                "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
            )
            .bind(cancelled_pid)
            .fetch_optional(admin)
            .await?;
            if idle == Some(true) {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await??;
    let next_job = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            match DatabasePubSubStorage::start_pinned_relation_inventory(inventory) {
                Ok(job) => return Ok(job),
                Err(PinnedPubSubInventoryError::Overloaded) => {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(error) => return Err(error),
            }
        }
    })
    .await??;
    next_job.finish().await?;

    let mut store_lock = admin.begin().await?;
    sqlx::query("LOCK TABLE ONLY guarded.pubsub_nodes IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *store_lock)
        .await?;
    let refusal = tokio::time::timeout(
        Duration::from_secs(8),
        DatabasePubSubStorage::start_pinned_relation_inventory(inventory)?.finish(),
    )
    .await?;
    ensure!(
        refusal.is_err(),
        "diagnostic accepted a locked PubSub store"
    );
    store_lock.rollback().await?;

    // The same pinned heap with a different owner is refused. The alternate
    // owner is a pre-existing fixture role without grants on this table.
    sqlx::query(&format!(
        "ALTER TABLE guarded.pubsub_nodes OWNER TO {alternate_owner}"
    ))
    .execute(admin)
    .await?;
    let owner_drift = DatabasePubSubStorage::start_pinned_relation_inventory(inventory)?
        .finish()
        .await;
    sqlx::query(&format!(
        "ALTER TABLE guarded.pubsub_nodes OWNER TO {owner}"
    ))
    .execute(admin)
    .await?;
    ensure!(
        matches!(
            owner_drift,
            Err(PinnedPubSubInventoryError::CatalogDrift {
                relation: "pubsub_nodes"
            })
        ),
        "a different PubSub relation owner was accepted: {owner_drift:?}"
    );

    // Give the diagnostic the actual independently observed OID of a view at
    // the expected name. This isolates its relkind check from a stale OID.
    sqlx::query("ALTER TABLE guarded.pubsub_nodes RENAME TO pubsub_nodes_saved")
        .execute(admin)
        .await?;
    sqlx::query("CREATE VIEW guarded.pubsub_nodes AS SELECT * FROM guarded.pubsub_nodes_saved")
        .execute(admin)
        .await?;
    sqlx::query(&format!("ALTER VIEW guarded.pubsub_nodes OWNER TO {owner}"))
        .execute(admin)
        .await?;
    let view_oid: Oid =
        sqlx::query_scalar("SELECT 'guarded.pubsub_nodes'::pg_catalog.regclass::oid")
            .fetch_one(admin)
            .await?;
    let mut wrong_kind = relation_oids;
    wrong_kind[6] = view_oid.0;
    let view_inventory = fixture.new_with_oids(wrong_kind)?;
    let kind_drift = DatabasePubSubStorage::start_pinned_relation_inventory(&view_inventory)?
        .finish()
        .await;
    sqlx::query("DROP VIEW guarded.pubsub_nodes")
        .execute(admin)
        .await?;
    sqlx::query("ALTER TABLE guarded.pubsub_nodes_saved RENAME TO pubsub_nodes")
        .execute(admin)
        .await?;
    ensure!(
        matches!(
            kind_drift,
            Err(PinnedPubSubInventoryError::CatalogDrift {
                relation: "pubsub_nodes"
            })
        ),
        "a view at the PubSub name was accepted: {kind_drift:?}"
    );

    exercise_fence_retry(admin, runtime, inventory).await?;
    exercise_bounded_rollback(admin, inventory).await?;
    DatabasePubSubStorage::start_pinned_relation_inventory(inventory)?
        .finish()
        .await?;
    Ok(())
}

async fn exercise_fence_retry(
    admin: &PgPool,
    runtime: &str,
    inventory: &PinnedPubSubRelationInventory,
) -> anyhow::Result<()> {
    let (before_tx, before_rx) = tokio::sync::oneshot::channel();
    let (proceed_tx, proceed_rx) = tokio::sync::oneshot::channel();
    let (rolled_back_tx, rolled_back_rx) = tokio::sync::oneshot::channel();
    let (retry_tx, retry_rx) = tokio::sync::oneshot::channel();
    let job = DatabasePubSubStorage::start_pinned_inventory_fence_paused_for_test(
        inventory,
        before_tx,
        proceed_rx,
        rolled_back_tx,
        retry_rx,
    )?;
    tokio::time::timeout(Duration::from_secs(8), before_rx).await??;
    let pid: i32 = sqlx::query_scalar(
        "SELECT pid FROM pg_catalog.pg_stat_activity WHERE usename=$1 AND xact_start IS NOT NULL \
         AND query='SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY' ORDER BY query_start DESC LIMIT 1",
    ).bind(runtime).fetch_one(admin).await?;
    let mut fence = admin.begin().await?;
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(CATALOG_MUTATION_ADVISORY_LOCK_KEY)
        .execute(&mut *fence)
        .await?;
    proceed_tx
        .send(())
        .map_err(|_| anyhow::anyhow!("release pre-fence pause"))?;
    tokio::time::timeout(Duration::from_secs(8), rolled_back_rx).await??;
    let idle: bool = sqlx::query_scalar(
        "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
    )
    .bind(pid)
    .fetch_one(admin)
    .await?;
    ensure!(
        idle,
        "contended fence retried without acknowledged rollback"
    );
    fence.rollback().await?;
    retry_tx
        .send(())
        .map_err(|_| anyhow::anyhow!("release retry pause"))?;
    job.finish().await?;
    Ok(())
}

async fn exercise_bounded_rollback(
    admin: &PgPool,
    inventory: &PinnedPubSubRelationInventory,
) -> anyhow::Result<()> {
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let start = tokio::time::Instant::now();
    let job = DatabasePubSubStorage::start_pinned_inventory_rollback_stall_for_test(
        inventory,
        Duration::from_secs(3),
        ready_tx,
    )?;
    let old_pid = tokio::time::timeout(Duration::from_secs(8), ready_rx).await??;
    let result = tokio::time::timeout(Duration::from_secs(10), job.finish()).await?;
    ensure!(
        matches!(result, Err(PinnedPubSubInventoryError::RollbackUnconfirmed)),
        "a stalled rollback was treated as confirmed: {result:?}"
    );
    ensure!(
        start.elapsed() < Duration::from_secs(10),
        "bounded rollback returned too late"
    );

    // A failed acknowledgement must not return this physical connection to
    // the pool, even though the server may still be finishing the old query.
    let (new_ready_tx, new_ready_rx) = tokio::sync::oneshot::channel();
    let (new_release_tx, new_release_rx) = tokio::sync::oneshot::channel();
    let next = DatabasePubSubStorage::start_pinned_relation_inventory_paused_for_test(
        inventory,
        new_ready_tx,
        new_release_rx,
    )?;
    let (new_pid, _, _) = tokio::time::timeout(Duration::from_secs(10), new_ready_rx).await??;
    ensure!(
        new_pid != old_pid,
        "an unconfirmed backend was returned to the pool"
    );
    new_release_tx
        .send(())
        .map_err(|_| anyhow::anyhow!("release replacement backend"))?;
    next.finish().await?;
    tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            let connected: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",
            )
            .bind(old_pid)
            .fetch_one(admin)
            .await?;
            if !connected {
                return Ok::<_, sqlx::Error>(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await??;
    Ok(())
}
