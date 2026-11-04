//! Keypair-slot Postgres CAS lease (ADR-0017 element 3/4).
//!
//! A pod leases exactly one slot of the pre-enrolled keypair pool via a
//! server-time (`now()`) compare-and-set, so at most one live node ever holds
//! a given slot — and therefore no two swarm members share a libp2p `PeerId`.
//! The lease is heartbeat-renewed; a renewal that finds its slot stolen or
//! expired is **fencing loss** and the node must stop using that identity.
//!
//! Postgres-only: the control plane has no SQLite equivalent (`now()`-based
//! row CAS), and clustering requires Postgres. The store goes through the
//! portable `Database` (which rewrites `?`→`$n` for Postgres and leaves
//! `now()`/interval SQL intact); it is only ever invoked on a Postgres
//! deployment.
//!
//! The dedicated control-plane connection pool the ADR prescribes (element
//! 4/12) lands here (ADR-0017 Phase 3 Slice 0): [`PostgresKeypairSlotLease::heartbeat`]
//! renews through [`Database::control_plane_guard`], so this per-node
//! liveness beat never queues behind fenced bulk writes on the main pool.
//! `acquire`/`release` are not the periodic liveness beat the pool split
//! protects and stay on the main pool.

use super::NodeId;
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use async_trait::async_trait;
use std::collections::HashMap;
use std::time::Duration;

#[cfg(all(test, feature = "test-support"))]
#[path = "lease_runtime_attach_tests.rs"]
mod runtime_attach_tests;

/// Per-process leaseholder identity. `node_id`/`node_epoch` are freshly
/// generated on every process start and never reused, so a restarted pod never
/// looks like its former self holding a slot. Both are typed values
/// (typed-payloads rule); serialization to TEXT happens only at the SQL
/// parameter boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseIdentity {
    pub node_id: NodeId,
    pub node_epoch: uuid::Uuid,
}

/// A successfully leased keypair-pool slot (index into the enrolled pool).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeasedSlot {
    pub slot_index: u32,
}

/// Keypair-slot lease failures.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// The backing database returned an error.
    #[error("clustering keypair-slot lease database error: {0}")]
    Database(#[from] DatabaseError),
    /// Every pool slot is currently held by a live node.
    #[error(
        "no free clustering keypair-slot could be leased: all {pool_size} enrolled pool slots \
         are held by live nodes (scale-up beyond the enrolled pool needs a pipeline enrollment run)"
    )]
    NoFreeSlot { pool_size: usize },
    /// A heartbeat renewal found the slot no longer held by this node
    /// (stolen after lease expiry, or epoch superseded).
    #[error("clustering keypair-slot {slot_index} lease lost (fencing): slot no longer held by this node")]
    FencingLoss { slot_index: u32 },
    /// `acquire` was called with an empty pool — a configuration error the
    /// caller should have screened out.
    #[error("clustering keypair-slot lease requested with an empty keypair pool")]
    EmptyPool,
    /// Owner DDL must not run through an attested restricted runtime handle.
    #[error(
        "keypair-slot owner schema initialization requires a non-attested owner database handle"
    )]
    OwnerBootstrapRequiresOwnerHandle,
    /// An attested runtime handle can operate only after exact main and
    /// control-plane pool verification by `attach_existing`.
    #[error(
        "attested keypair-slot runtime handle must pass attach_existing before lease operations"
    )]
    RuntimeAttachRequired,
}

/// Fail-closed refusal to attach a restricted keypair-slot runtime store.
#[derive(Debug, thiserror::Error)]
pub enum LeaseAttachError {
    #[error("keypair-slot lease attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("keypair-slot lease attachment requires the attested main database lane")]
    MainLaneRequired,
    #[error("keypair-slot lease attachment requires an attested main pool")]
    AttestedMainPoolRequired,
    #[error("keypair-slot lease attachment requires an independent attested control-plane pool")]
    AttestedControlPlanePoolRequired,
    #[error("keypair-slot lease main and control-plane pool identities differ")]
    PhysicalIdentityMismatch,
    #[error("the attested schema identifier is invalid")]
    InvalidSchema,
    #[error("the active PostgreSQL schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not verify the PostgreSQL {setting} bound for keypair-slot lease attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("the clustering keypair-slot relation is missing or has an invalid shape")]
    InvalidSchemaShape,
    #[error("the clustering keypair-slot runtime role or grants are unsafe")]
    UnsafeRuntimeRole,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// The keypair-slot lease store. A trait so a dedicated control-plane pool
/// implementation (Phase 3) or a test double can substitute without touching
/// callers.
#[async_trait]
pub trait KeypairSlotLease: Send + Sync {
    /// Create the backing table if it does not exist.
    async fn ensure_schema(&self) -> Result<(), LeaseError>;

    /// Lease any free or expired slot in `[0, pool_size)`. Returns the leased
    /// slot, or [`LeaseError::NoFreeSlot`] if all are held by live nodes.
    async fn acquire(
        &self,
        identity: &LeaseIdentity,
        pool_size: usize,
        lease_ttl: Duration,
    ) -> Result<LeasedSlot, LeaseError>;

    /// Renew the lease on a held slot. Returns [`LeaseError::FencingLoss`] when
    /// the freshness-gated CAS affects zero rows (lease lapsed or stolen).
    async fn heartbeat(
        &self,
        identity: &LeaseIdentity,
        slot: LeasedSlot,
        lease_ttl: Duration,
    ) -> Result<(), LeaseError>;

    /// Release a held slot on graceful drain (epoch-gated, best-effort).
    async fn release(&self, identity: &LeaseIdentity, slot: LeasedSlot) -> Result<(), LeaseError>;
}

/// Postgres implementation using `now()`-based row CAS.
pub struct PostgresKeypairSlotLease {
    db: Database,
    runtime_attach_verified: bool,
}

impl PostgresKeypairSlotLease {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            runtime_attach_verified: false,
        }
    }

    /// Initialize the keypair-slot table through this exact owner database.
    /// Attested runtime handles can attach only after the owner has prepared
    /// the schema and grants.
    pub async fn initialize_owner(db: Database) -> Result<Self, LeaseError> {
        if db.attested_pg_writer_capability().is_some() {
            return Err(LeaseError::OwnerBootstrapRequiresOwnerHandle);
        }
        let store = Self {
            db,
            runtime_attach_verified: false,
        };
        store.ensure_schema().await?;
        Ok(store)
    }

    /// Attach the exact attested main and independent control-plane pools to
    /// an owner-prepared table. This performs only bounded read-only catalog
    /// checks and no URL reopen or schema DDL.
    pub async fn attach_existing(db: Database) -> Result<Self, LeaseAttachError> {
        if db.driver() != crate::db::DatabaseDriver::Postgres {
            return Err(LeaseAttachError::PostgresRequired);
        }
        if !db.is_main_lane() {
            return Err(LeaseAttachError::MainLaneRequired);
        }
        let main_capability = db
            .attested_pg_writer_capability()
            .ok_or(LeaseAttachError::AttestedMainPoolRequired)?;
        db.verify_attested_pg_writer_capability(&main_capability)
            .map_err(DatabaseError::from)?;
        let (control_plane, control_plane_capability) = db
            .attested_control_plane_pool_view()
            .map_err(|_| LeaseAttachError::AttestedControlPlanePoolRequired)?;
        if main_capability.identity() != control_plane_capability.identity() {
            return Err(LeaseAttachError::PhysicalIdentityMismatch);
        }

        verify_attested_keypair_slot_pair(
            &db,
            &main_capability,
            &control_plane,
            &control_plane_capability,
        )
        .await?;
        Ok(Self {
            db,
            runtime_attach_verified: true,
        })
    }

    fn require_runtime_attach(&self) -> Result<(), LeaseError> {
        if self.db.attested_pg_writer_capability().is_some() && !self.runtime_attach_verified {
            return Err(LeaseError::RuntimeAttachRequired);
        }
        Ok(())
    }

    /// Try to claim one specific slot. Returns `true` on success (row inserted
    /// or a stale/free row stolen), `false` if the slot is held fresh.
    async fn try_acquire_slot(
        &self,
        identity: &LeaseIdentity,
        slot_index: u32,
        lease_ttl: Duration,
    ) -> Result<bool, LeaseError> {
        let conn = self.db.guard().await?;
        // Upsert CAS: insert a fresh hold, or steal an existing row only when
        // it is unheld, already expired, or its heartbeat has aged past the
        // TTL. A held-and-fresh slot fails the ON CONFLICT WHERE and affects
        // zero rows. Postgres row-locks the conflicting row, so two racing
        // acquirers of the same slot serialize and exactly one wins.
        let affected = conn
            .execute(
                r#"
                INSERT INTO clustering_keypair_slots
                    (slot_index, holder_node, holder_epoch, heartbeat, expired)
                VALUES (?, ?, ?, now(), false)
                ON CONFLICT (slot_index) DO UPDATE SET
                    holder_node = EXCLUDED.holder_node,
                    holder_epoch = EXCLUDED.holder_epoch,
                    heartbeat = now(),
                    expired = false
                WHERE clustering_keypair_slots.holder_node IS NULL
                   OR clustering_keypair_slots.expired
                   OR clustering_keypair_slots.heartbeat < now() - (? || ' milliseconds')::interval
                "#,
                // Storage boundary: typed identity serializes to TEXT here.
                crate::db_params![
                    i64::from(slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await?;
        Ok(affected == 1)
    }
}

#[async_trait]
impl KeypairSlotLease for PostgresKeypairSlotLease {
    async fn ensure_schema(&self) -> Result<(), LeaseError> {
        if self.db.attested_pg_writer_capability().is_some() {
            return Err(LeaseError::OwnerBootstrapRequiresOwnerHandle);
        }
        let conn = self.db.guard().await?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_keypair_slots (
                slot_index   INTEGER PRIMARY KEY,
                holder_node  TEXT,
                holder_epoch TEXT,
                heartbeat    TIMESTAMPTZ NOT NULL DEFAULT now(),
                expired      BOOLEAN NOT NULL DEFAULT TRUE
            )
            "#,
            (),
        )
        .await?;
        Ok(())
    }

    async fn acquire(
        &self,
        identity: &LeaseIdentity,
        pool_size: usize,
        lease_ttl: Duration,
    ) -> Result<LeasedSlot, LeaseError> {
        self.require_runtime_attach()?;
        if pool_size == 0 {
            return Err(LeaseError::EmptyPool);
        }
        // Scan slots, starting at an offset derived from this node's id so
        // concurrently-starting pods don't all contend on slot 0 first. The
        // CAS guarantees correctness regardless of scan order.
        let start = (stable_offset(identity.node_id.as_str()) % pool_size) as u32;
        let pool = pool_size as u32;
        for step in 0..pool {
            let slot_index = (start + step) % pool;
            if self
                .try_acquire_slot(identity, slot_index, lease_ttl)
                .await?
            {
                return Ok(LeasedSlot { slot_index });
            }
        }
        Err(LeaseError::NoFreeSlot { pool_size })
    }

    async fn heartbeat(
        &self,
        identity: &LeaseIdentity,
        slot: LeasedSlot,
        lease_ttl: Duration,
    ) -> Result<(), LeaseError> {
        self.require_runtime_attach()?;
        // Runs on the dedicated control-plane pool (ADR-0017 element 4/12,
        // Phase 3 Slice 0): this per-node liveness renewal must never queue
        // behind fenced bulk writes, backstop fencing SELECTs, or claims-read
        // storms on the main pool.
        let conn = self.db.control_plane_guard().await?;
        // Freshness-gated renewal CAS (ADR element 4): renew only while we
        // still hold the slot under our epoch and the lease has not lapsed.
        let affected = conn
            .execute(
                r#"
                UPDATE clustering_keypair_slots
                SET heartbeat = now()
                WHERE slot_index = ?
                  AND holder_node = ?
                  AND holder_epoch = ?
                  AND NOT expired
                  AND heartbeat >= now() - (? || ' milliseconds')::interval
                "#,
                crate::db_params![
                    i64::from(slot.slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await?;
        if affected == 0 {
            return Err(LeaseError::FencingLoss {
                slot_index: slot.slot_index,
            });
        }
        Ok(())
    }

    async fn release(&self, identity: &LeaseIdentity, slot: LeasedSlot) -> Result<(), LeaseError> {
        self.require_runtime_attach()?;
        let conn = self.db.guard().await?;
        // Epoch-gated release: mark the slot free+expired so a replacement can
        // acquire it immediately, but only if we still own it.
        conn.execute(
            r#"
            UPDATE clustering_keypair_slots
            SET holder_node = NULL, holder_epoch = NULL, expired = true
            WHERE slot_index = ? AND holder_node = ? AND holder_epoch = ?
            "#,
            crate::db_params![
                i64::from(slot.slot_index),
                identity.node_id.as_str().to_string(),
                identity.node_epoch.to_string(),
            ],
        )
        .await?;
        Ok(())
    }
}

const KEYPAIR_SLOT_TABLE: &str = "clustering_keypair_slots";

async fn verify_attested_keypair_slot_pair(
    db: &Database,
    main_capability: &AttestedPostgresWriterCapability,
    control_plane: &Database,
    control_plane_capability: &AttestedPostgresWriterCapability,
) -> Result<(), LeaseAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(LeaseAttachError::PostgresRequired);
    }
    let owned_capability = db
        .attested_pg_writer_capability()
        .ok_or(LeaseAttachError::AttestedMainPoolRequired)?;
    db.verify_attested_pg_writer_capability(&owned_capability)
        .map_err(DatabaseError::from)?;
    if !owned_capability.belongs_to(main_capability) {
        return Err(LeaseAttachError::AttestedMainPoolRequired);
    }
    let owned_control_capability = control_plane
        .attested_pg_writer_capability()
        .ok_or(LeaseAttachError::AttestedControlPlanePoolRequired)?;
    control_plane
        .verify_attested_pg_writer_capability(&owned_control_capability)
        .map_err(DatabaseError::from)?;
    if !owned_control_capability.belongs_to(control_plane_capability) {
        return Err(LeaseAttachError::AttestedControlPlanePoolRequired);
    }

    let identity = main_capability.identity();
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "keypair-slot schema")
        .map_err(|_| LeaseAttachError::InvalidSchema)?;
    let qualified_table = format!(
        "{}.{}",
        crate::db::physical_attestation::quote_identifier(schema),
        crate::db::physical_attestation::quote_identifier(KEYPAIR_SLOT_TABLE)
    );

    let mut main_tx = begin_attested_check(db, main_capability, &qualified_table).await?;
    let mut control_tx =
        begin_attested_check(control_plane, control_plane_capability, &qualified_table).await?;

    // Both pools now hold ACCESS SHARE against the same qualified relation.
    // The compatible locks keep owner DDL from changing the table between
    // their independent catalog snapshots.
    verify_keypair_slot_snapshot(&mut main_tx, schema, i64::from(identity.schema.oid)).await?;
    verify_keypair_slot_snapshot(&mut control_tx, schema, i64::from(identity.schema.oid)).await?;
    control_tx.rollback().await?;
    main_tx.rollback().await?;
    Ok(())
}

async fn begin_attested_check<'a>(
    db: &'a Database,
    capability: &AttestedPostgresWriterCapability,
    qualified_table: &str,
) -> Result<Transaction<'a>, LeaseAttachError> {
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
        (),
    )
    .await?;
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|error| match error {
            crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                LeaseAttachError::InvalidTimeoutSetting { setting }
            }
            crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                LeaseAttachError::Database(error)
            }
        })?;
    tx.execute(
        &format!("LOCK TABLE {qualified_table} IN ACCESS SHARE MODE"),
        (),
    )
    .await
    .map_err(|_| LeaseAttachError::InvalidSchemaShape)?;

    capability.attest_transaction(&mut tx).await?;
    Ok(tx)
}

async fn verify_keypair_slot_snapshot(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: i64,
) -> Result<(), LeaseAttachError> {
    verify_active_schema(tx, schema, schema_oid).await?;
    verify_table_shape(tx, schema).await?;
    verify_columns(tx, schema).await?;
    verify_primary_key(tx, schema).await?;
    verify_index_shape(tx, schema).await?;
    verify_runtime_acl(tx, schema).await?;
    Ok(())
}

async fn verify_active_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: i64,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.current_schema()::text, n.oid::bigint \
             FROM pg_catalog.pg_namespace n WHERE n.nspname = ?",
            crate::db_params![schema],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LeaseAttachError::ActiveSchemaMismatch);
    };
    let active: Option<String> = row.get(0)?;
    let actual_oid: i64 = row.get(1)?;
    if active.as_deref() != Some(schema) || actual_oid != schema_oid {
        return Err(LeaseAttachError::ActiveSchemaMismatch);
    }
    Ok(())
}

async fn verify_table_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' \
                    AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con \
                                    WHERE con.conrelid = c.oid AND con.contype <> 'p') \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint inbound \
                                    WHERE inbound.confrelid = c.oid AND inbound.contype = 'f') \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                                    WHERE tr.tgrelid = c.oid OR tr.tgconstrrelid = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                                    WHERE rw.ev_class = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                                    WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, KEYPAIR_SLOT_TABLE],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LeaseAttachError::InvalidSchemaShape);
    };
    if !row.get::<bool>(0)? {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    Ok(())
}

async fn verify_columns(tx: &mut Transaction<'_>, schema: &str) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull, a.attgenerated::text, a.attidentity::text, \
                    pg_catalog.pg_get_expr(d.adbin, d.adrelid), \
                    a.attcollation = typ.typcollation \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_type typ ON typ.oid = a.atttypid \
             LEFT JOIN pg_catalog.pg_attrdef d \
               ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = ? AND c.relname = ? AND c.relkind = 'r' \
               AND a.attnum > 0 AND NOT a.attisdropped",
            crate::db_params![schema, KEYPAIR_SLOT_TABLE],
        )
        .await?;
    let mut actual = HashMap::new();
    while let Some(row) = rows.next().await? {
        actual.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<String>(3)?,
                row.get::<String>(4)?,
                row.get::<Option<String>>(5)?,
                row.get::<bool>(6)?,
            ),
        );
    }
    let expected = [
        ("slot_index", "integer", true, None),
        ("holder_node", "text", false, None),
        ("holder_epoch", "text", false, None),
        ("heartbeat", "timestamp with time zone", true, Some("now()")),
        ("expired", "boolean", true, Some("true")),
    ];
    if actual.len() != expected.len() {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    for (name, kind, not_null, default) in expected {
        let Some((actual_kind, actual_not_null, generated, identity, actual_default, collation)) =
            actual.get(name)
        else {
            return Err(LeaseAttachError::InvalidSchemaShape);
        };
        if actual_kind != kind
            || *actual_not_null != not_null
            || !generated.is_empty()
            || !identity.is_empty()
            || actual_default.as_deref() != default
            || !collation
        {
            return Err(LeaseAttachError::InvalidSchemaShape);
        }
    }
    Ok(())
}

async fn verify_primary_key(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             CROSS JOIN LATERAL pg_catalog.unnest(c.conkey) \
               WITH ORDINALITY k(attnum, position) \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             WHERE c.contype = 'p' AND NOT c.condeferrable AND NOT c.condeferred \
               AND c.convalidated \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_index i \
                           WHERE i.indexrelid = c.conindid AND i.indisvalid \
                             AND i.indisready AND i.indislive AND i.indimmediate) \
               AND n.nspname = ? AND t.relname = ? ORDER BY k.position",
            crate::db_params![schema, KEYPAIR_SLOT_TABLE],
        )
        .await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        columns.push(row.get::<String>(0)?);
    }
    if columns.iter().map(String::as_str).collect::<Vec<_>>() != ["slot_index"] {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    Ok(())
}

async fn verify_index_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.count(*)::bigint, \
                    COALESCE(pg_catalog.bool_and( \
                      i.indisprimary AND i.indisunique AND i.indisvalid AND i.indisready \
                      AND i.indislive AND i.indimmediate AND i.indpred IS NULL \
                      AND i.indexprs IS NULL AND am.amname = 'btree' \
                      AND i.indnkeyatts = 1 AND i.indnatts = 1 \
                      AND pg_catalog.array_to_string(ARRAY( \
                        SELECT a.attname::text \
                        FROM pg_catalog.unnest(i.indkey) WITH ORDINALITY k(attnum, position) \
                        JOIN pg_catalog.pg_attribute a \
                          ON a.attrelid = t.oid AND a.attnum = k.attnum \
                        WHERE k.position <= i.indnkeyatts ORDER BY k.position), ',') = 'slot_index' \
                      AND (SELECT COALESCE(pg_catalog.bool_and(k.option = 0), true) \
                           FROM pg_catalog.unnest(i.indoption) \
                             WITH ORDINALITY k(option, position) \
                           WHERE k.position <= i.indnkeyatts)), false) \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
             WHERE n.nspname = ? AND t.relname = ?",
            crate::db_params![schema, KEYPAIR_SLOT_TABLE],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(LeaseAttachError::InvalidSchemaShape)?;
    let index_count: i64 = row.get(0)?;
    let valid_primary_index: bool = row.get(1)?;
    if index_count != 1 || !valid_primary_index {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    Ok(())
}

async fn verify_runtime_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            r#"SELECT runtime.rolcanlogin AND NOT runtime.rolsuper
                    AND NOT runtime.rolcreaterole AND NOT runtime.rolcreatedb
                    AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls
                    AND runtime.oid <> database.datdba
                    AND runtime.oid <> owner.oid AND NOT owner.rolcanlogin
                    AND owner.oid <> database.datdba
                    AND NOT owner.rolsuper AND NOT owner.rolcreaterole
                    AND NOT owner.rolcreatedb AND NOT owner.rolreplication
                    AND NOT owner.rolbypassrls
                    AND session_user::text = runtime.rolname
                    AND current_user::text = runtime.rolname
                    AND NOT pg_catalog.has_database_privilege(
                      runtime.oid, database.oid, 'CREATE,TEMP')
                    AND NOT pg_catalog.has_parameter_privilege(
                      runtime.oid, 'session_replication_role', 'SET')
                    AND pg_catalog.current_setting('session_replication_role') = 'origin'
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                                    WHERE m.member = runtime.oid OR m.roleid = runtime.oid)
                    AND pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'USAGE')
                    AND NOT pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'CREATE')
                    AND ns.nspowner = owner.oid AND relation.relowner = owner.oid
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.aclexplode(
                        COALESCE(ns.nspacl, pg_catalog.acldefault('n', ns.nspowner))
                      ) acl WHERE acl.grantee = 0
                        OR (acl.grantee <> owner.oid AND (
                          acl.is_grantable OR acl.privilege_type <> 'USAGE'))
                    )
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT')
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT')
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'UPDATE')
                    AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'DELETE')
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid, 'TRUNCATE, REFERENCES, TRIGGER')
                    AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000
                      THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN')
                      ELSE true END
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid,
                      'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION,
                       UPDATE WITH GRANT OPTION, DELETE WITH GRANT OPTION,
                       TRUNCATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION,
                       TRIGGER WITH GRANT OPTION')
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.aclexplode(
                        COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))
                      ) acl WHERE acl.grantee NOT IN (owner.oid, runtime.oid)
                        OR (acl.grantee = runtime.oid AND (
                          acl.is_grantable OR acl.privilege_type NOT IN
                            ('SELECT', 'INSERT', 'UPDATE')))
                    )
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.pg_attribute a
                      WHERE a.attrelid = relation.oid AND a.attnum > 0
                        AND NOT a.attisdropped AND a.attacl IS NOT NULL
                    )
             FROM pg_catalog.pg_roles runtime
             JOIN pg_catalog.pg_class relation ON relation.relname = ?
             JOIN pg_catalog.pg_namespace ns ON ns.oid = relation.relnamespace
             JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner
             JOIN pg_catalog.pg_database database
               ON database.datname = pg_catalog.current_database()
             WHERE runtime.rolname = current_user AND ns.nspname = ?"#,
            crate::db_params![KEYPAIR_SLOT_TABLE, schema],
        )
        .await?;
    let safe = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !safe {
        return Err(LeaseAttachError::UnsafeRuntimeRole);
    }
    Ok(())
}

/// A cheap, stable, non-cryptographic offset from a node id, so pods spread
/// their initial slot scan instead of all starting at 0.
fn stable_offset(node_id: &str) -> usize {
    // FNV-1a over the bytes — deterministic and dependency-free.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in node_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use std::time::Duration;

    // These tests share one `clustering_keypair_slots` table, so serialize
    // them and wipe the table at each start. They are skipped unless
    // `WADDLE_TEST_POSTGRES_URL` points at a Postgres (the control-plane CAS
    // has no SQLite equivalent). CI runs them under the clustering feature +
    // the Nix-spawned Postgres.
    fn ident() -> LeaseIdentity {
        LeaseIdentity {
            node_id: NodeId::generate(),
            node_epoch: uuid::Uuid::new_v4(),
        }
    }

    async fn clean_store() -> Option<PostgresKeypairSlotLease> {
        let url = waddle_ws_test_support::require_test_postgres_url()?;
        // `heartbeat` now renews through the control-plane pool, so the test
        // database needs one provisioned exactly like production's global
        // database does.
        let db = Database::from_config(
            "clustering-lease-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let store = PostgresKeypairSlotLease::new(db);
        store.ensure_schema().await.expect("ensure schema");
        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute("DELETE FROM clustering_keypair_slots", ())
            .await
            .expect("clean slots");
        Some(store)
    }

    const TTL: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn two_nodes_lease_distinct_slots() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let slot_a = store.acquire(&a, 2, TTL).await.expect("a acquires");
        let slot_b = store.acquire(&b, 2, TTL).await.expect("b acquires");
        assert_ne!(slot_a.slot_index, slot_b.slot_index);
    }

    #[tokio::test]
    async fn single_slot_pool_admits_exactly_one() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        store.acquire(&a, 1, TTL).await.expect("a acquires slot 0");
        let err = store.acquire(&b, 1, TTL).await.expect_err("b is refused");
        assert!(matches!(err, LeaseError::NoFreeSlot { pool_size: 1 }));
    }

    #[tokio::test]
    async fn heartbeat_renews_and_release_frees() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let slot = store.acquire(&a, 1, TTL).await.expect("a acquires");
        store.heartbeat(&a, slot, TTL).await.expect("a renews");
        // While A holds it, B cannot take it.
        assert!(store.acquire(&b, 1, TTL).await.is_err());
        // After A releases, B acquires immediately.
        store.release(&a, slot).await.expect("a releases");
        let slot_b = store.acquire(&b, 1, TTL).await.expect("b acquires freed");
        assert_eq!(slot_b.slot_index, slot.slot_index);
    }

    #[tokio::test]
    async fn expired_slot_is_stolen_and_old_holder_fences() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let short_ttl = Duration::from_secs(1);
        let slot = store.acquire(&a, 1, short_ttl).await.expect("a acquires");
        // Let the lease lapse, then B steals the expired slot.
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let slot_b = store
            .acquire(&b, 1, short_ttl)
            .await
            .expect("b steals expired slot");
        assert_eq!(slot_b.slot_index, slot.slot_index);
        // A's renewal now fails — fencing loss.
        let err = store
            .heartbeat(&a, slot, short_ttl)
            .await
            .expect_err("a fences");
        assert!(matches!(err, LeaseError::FencingLoss { .. }));
    }

    #[tokio::test]
    async fn sub_second_ttl_uses_millisecond_precision() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        // A 300ms TTL must NOT floor to '0 seconds' (which would make every
        // slot instantly stealable and every renewal fence).
        let ttl = Duration::from_millis(300);
        let slot = store.acquire(&a, 1, ttl).await.expect("a acquires");
        // Immediately renewable within the sub-second window, and the fresh
        // slot is not stealable by B.
        store
            .heartbeat(&a, slot, ttl)
            .await
            .expect("a renews in-window");
        assert!(store.acquire(&b, 1, ttl).await.is_err());
        // After the window lapses, B steals it.
        tokio::time::sleep(Duration::from_millis(450)).await;
        let slot_b = store.acquire(&b, 1, ttl).await.expect("b steals expired");
        assert_eq!(slot_b.slot_index, slot.slot_index);
    }

    #[test]
    fn stable_offset_is_deterministic() {
        assert_eq!(stable_offset("node-a"), stable_offset("node-a"));
    }
}
