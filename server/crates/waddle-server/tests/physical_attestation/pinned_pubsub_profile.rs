//! Pinned transaction witnesses for the combined PubSub profile fixture.

use std::time::Duration;

use anyhow::ensure;
use sqlx::postgres::{types::Oid, PgConnectOptions, PgPool};
use sqlx::Row;
use uuid::Uuid;
use waddle_server::db::physical_attestation::{
    attested_pool, pinned_attested_pool, DarkPhysicalAttestationPins, PhysicalAttestationConfig,
    PinnedPubSubProfile, PinnedPubSubProfileError, PinnedRuntimeAuthority,
    CATALOG_MUTATION_ADVISORY_LOCK_KEY,
};
use waddle_server::db::DarkMigrationLedgerPins;
use waddle_server::pubsub::{DatabasePubSubStorage, RestrictedServingProfileV2};

pub(super) async fn pinned_pubsub_profile(
    admin: &PgPool,
    runtime_options: &PgConnectOptions,
    config: &PhysicalAttestationConfig,
    installation_operation: Uuid,
    roles: (&str, &str, &str),
    profile: &RestrictedServingProfileV2,
) -> anyhow::Result<PinnedPubSubProfile> {
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
    let make_pool = || {
        pinned_attested_pool(
            runtime_options.clone(),
            config.clone(),
            authority.clone(),
            1,
            Duration::from_secs(5),
        )
    };
    let legacy_pool = attested_pool(
        runtime_options.clone(),
        config.clone(),
        1,
        Duration::from_secs(5),
    )?;
    ensure!(
        matches!(
            PinnedPubSubProfile::new(legacy_pool, profile.clone()),
            Err(PinnedPubSubProfileError::InvalidPins)
        ),
        "legacy pool was accepted as pinned"
    );
    let mut stale = profile.clone();
    stale.epoch_relation_oid = stale.epoch_relation_oid.wrapping_add(1);
    ensure!(
        matches!(
            PinnedPubSubProfile::new(make_pool()?, stale),
            Err(PinnedPubSubProfileError::InvalidPins)
        ),
        "profile epoch pin was detached from its exact pool"
    );
    Ok(PinnedPubSubProfile::new(make_pool()?, profile.clone())?)
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
    profile: &PinnedPubSubProfile,
    pins: &RestrictedServingProfileV2,
) -> anyhow::Result<()> {
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
    let job = DatabasePubSubStorage::start_pinned_restricted_profile_v2_paused_for_test(
        profile, ready_tx, release_rx,
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
    ensure!(
        !holds_relation(
            admin,
            waiting_pid,
            Oid(pins.epoch_relation_oid),
            "AccessShareLock"
        )
        .await?
    );
    ensure!(
        !holds_relation(
            admin,
            waiting_pid,
            Oid(pins.v1.helper_relation_oids[0]),
            "ShareUpdateExclusiveLock"
        )
        .await?
    );
    blocker.rollback().await?;
    let (pid, read_only, read_committed) =
        tokio::time::timeout(Duration::from_secs(8), ready_rx).await??;
    ensure!(
        pid == waiting_pid && read_only && read_committed,
        "profile left its exact read-only backend"
    );
    for relation in [lineage, ledger, Oid(pins.epoch_relation_oid), manifest] {
        ensure!(
            holds_relation(admin, pid, relation, "AccessShareLock").await?,
            "missing ordered barrier"
        );
    }
    for relation in pins.v1.helper_relation_oids {
        ensure!(
            holds_relation(admin, pid, Oid(relation), "ShareUpdateExclusiveLock").await?,
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
    DatabasePubSubStorage::start_pinned_restricted_profile_v2(profile)?
        .finish()
        .await?;

    // The worker retains its permit and transaction even after its caller
    // drops the result receiver; only release and acknowledged rollback allow
    // a subsequent diagnostic.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let abandoned = DatabasePubSubStorage::start_pinned_restricted_profile_v2_paused_for_test(
        profile, ready_tx, release_rx,
    )?;
    let (cancelled_pid, _, _) = tokio::time::timeout(Duration::from_secs(8), ready_rx).await??;
    drop(abandoned);
    ensure!(
        matches!(
            DatabasePubSubStorage::start_pinned_restricted_profile_v2(profile),
            Err(PinnedPubSubProfileError::Overloaded)
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
            match DatabasePubSubStorage::start_pinned_restricted_profile_v2(profile) {
                Ok(job) => return Ok(job),
                Err(PinnedPubSubProfileError::Overloaded) => {
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
        DatabasePubSubStorage::start_pinned_restricted_profile_v2(profile)?.finish(),
    )
    .await?;
    ensure!(
        refusal.is_err(),
        "diagnostic accepted a locked PubSub store"
    );
    store_lock.rollback().await?;

    sqlx::query(&format!(
        "GRANT UPDATE(id) ON guarded.ingress_protocol_epoch TO {runtime}"
    ))
    .execute(admin)
    .await?;
    let refusal = DatabasePubSubStorage::start_pinned_restricted_profile_v2(profile)?
        .finish()
        .await;
    sqlx::query(&format!(
        "REVOKE UPDATE(id) ON guarded.ingress_protocol_epoch FROM {runtime}"
    ))
    .execute(admin)
    .await?;
    ensure!(refusal.is_err(), "diagnostic accepted runtime role drift");
    DatabasePubSubStorage::start_pinned_restricted_profile_v2(profile)?
        .finish()
        .await?;
    Ok(())
}
