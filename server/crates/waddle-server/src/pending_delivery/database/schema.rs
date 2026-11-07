use super::{debug, info, DatabaseDriver, PendingStorageError};
use crate::db::{CatalogFenceLock, Database, DatabaseError, IntoParams, Rows, Transaction, Value};

pub(super) const LEGACY_MAM_CLEANUP_MARKER: &str = "legacy_mam_query_frames_v1";
pub(super) const V1014_CLAIM_RESET_MARKER: &str = "ingress_v1014_pending_claim_reset_v1";
const LEGACY_MAM_CLEANUP_SELECT_BATCH: i64 = 128;
const LEGACY_MAM_CLEANUP_DELETE_BATCH: usize = 128;

/// Owner-side access keeps the existing per-statement pool acquisition and
/// error mapping without constructing a live pending-delivery storage object.
struct OwnerSchemaDb<'a> {
    db: &'a Database,
}

impl OwnerSchemaDb<'_> {
    async fn execute(
        &self,
        sql: &str,
        params: impl IntoParams,
    ) -> Result<u64, PendingStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))
    }

    async fn query(&self, sql: &str, params: impl IntoParams) -> Result<Rows, PendingStorageError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))
    }
}

pub async fn bootstrap_schema_owner(db: &Database) -> Result<(), PendingStorageError> {
    if db.driver() == DatabaseDriver::Postgres {
        initialize_postgres(db).await
    } else {
        initialize(&OwnerSchemaDb { db }).await
    }
}

// The schema identity is selected once, before changing search_path. Later
// phases verify both name and OID while holding catalog-X or catalog-S. All
// application relations and functions are explicitly qualified.
#[derive(Clone)]
pub(super) struct PostgresOwnerSchema {
    name: String,
    oid: i64,
    quoted: String,
    pub(super) recovery_gate: (i32, i32),
    reset_gate: i64,
}

const OWNER_TRANSACTION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
const OWNER_ROLLBACK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);
const RECEIPT_SCHEMA_ADVISORY_LOCK_KEY: i64 = 7_305_784_319_670_198;
const RECOVERY_GATE_NAME: &str = "pending_notification_recovery_ordinal";

fn owner_error(error: impl std::fmt::Display) -> PendingStorageError {
    PendingStorageError::Other(error.to_string())
}

impl PostgresOwnerSchema {
    async fn capture(tx: &mut Transaction<'_>) -> Result<Self, PendingStorageError> {
        // Resolve only the configured schema name before changing the path.
        // Even a catalog-qualified table comparison could otherwise resolve a
        // hostile operator from an earlier writable search-path entry.
        let name = {
            let mut rows = tx
                .query("SELECT pg_catalog.current_schema()::text", ())
                .await
                .map_err(owner_error)?;
            rows.next()
                .await
                .map_err(owner_error)?
                .ok_or_else(|| owner_error("pending-delivery target schema missing"))?
                .get::<Option<String>>(0)
                .map_err(owner_error)?
                .ok_or_else(|| owner_error("pending-delivery target schema missing"))?
        };
        tx.execute("SET LOCAL search_path = pg_catalog, pg_temp", ())
            .await
            .map_err(owner_error)?;
        let mut rows = tx
            .query(
                "SELECT n.nspname::text, n.oid::bigint, pg_catalog.hashtext(n.nspname::text), \
                    pg_catalog.hashtext(?::text), pg_catalog.hashtext(?::text) \
             FROM pg_catalog.pg_namespace n \
             WHERE n.nspname = ? \
               AND n.oid <> pg_catalog.pg_my_temp_schema() \
               AND NOT pg_catalog.pg_is_other_temp_schema(n.oid) \
               AND n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')",
                crate::db_params![RECOVERY_GATE_NAME, V1014_CLAIM_RESET_MARKER, name],
            )
            .await
            .map_err(owner_error)?;
        let row = rows.next().await.map_err(owner_error)?.ok_or_else(|| {
            PendingStorageError::Other(
                "pending-delivery persistent target schema missing".to_owned(),
            )
        })?;
        let name: String = row.get(0).map_err(owner_error)?;
        let oid = row.get(1).map_err(owner_error)?;
        let schema_hash: i32 = row.get(2).map_err(owner_error)?;
        let recovery_hash: i32 = row.get(3).map_err(owner_error)?;
        let reset_hash: i32 = row.get(4).map_err(owner_error)?;
        drop(rows);
        let quoted = format!("\"{}\"", name.replace('"', "\"\""));
        Ok(Self {
            name,
            oid,
            quoted,
            recovery_gate: (schema_hash, recovery_hash),
            // PostgreSQL's one-argument advisory lock converts hashtext's
            // signed int4 result to int8, preserving its sign.
            reset_gate: i64::from(reset_hash),
        })
    }

    pub(super) fn relation(&self, name: &str) -> String {
        format!("{}.\"{}\"", self.quoted, name)
    }

    async fn prepare(&self, tx: &mut Transaction<'_>) -> Result<(), PendingStorageError> {
        // pg_temp follows pg_catalog explicitly; qualified application names
        // avoid temporary-object and caller search-path substitution entirely.
        tx.execute("SET LOCAL search_path = pg_catalog, pg_temp", ())
            .await
            .map_err(owner_error)?;
        let mut rows = tx
            .query(
                "SELECT 1 FROM pg_catalog.pg_namespace WHERE oid = ?::bigint::oid AND nspname = ?",
                crate::db_params![self.oid, self.name.clone()],
            )
            .await
            .map_err(owner_error)?;
        if rows.next().await.map_err(owner_error)?.is_none() {
            return Err(PendingStorageError::Other(
                "pending-delivery target schema changed during bootstrap".to_owned(),
            ));
        }
        drop(rows);
        crate::db::read_only_timeout::bound_postgres_waits(tx)
            .await
            .map_err(owner_error)?;
        Ok(())
    }
}

pub(super) async fn begin_postgres_stage<'a>(
    db: &'a Database,
    schema: &PostgresOwnerSchema,
    locks: &[CatalogFenceLock],
) -> Result<Transaction<'a>, PendingStorageError> {
    let mut tx = db
        .begin_catalog_fenced_with_idle_bound(locks)
        .await
        .map_err(owner_error)?;
    if let Err(error) = schema.prepare(&mut tx).await {
        if !tx.rollback_bounded_discard(OWNER_ROLLBACK_WAIT).await {
            return Err(owner_error(
                DatabaseError::CatalogMutationRollbackUnconfirmed,
            ));
        }
        return Err(error);
    }
    Ok(tx)
}

pub(super) async fn finish_postgres_stage<T>(
    tx: Transaction<'_>,
    result: Result<T, PendingStorageError>,
    stage: &'static str,
) -> Result<T, PendingStorageError> {
    match result {
        Err(error) => {
            if !tx.rollback_bounded_discard(OWNER_ROLLBACK_WAIT).await {
                return Err(owner_error(
                    DatabaseError::CatalogMutationRollbackUnconfirmed,
                ));
            }
            Err(error)
        }
        Ok(value) => match tokio::time::timeout(OWNER_TRANSACTION_WAIT, tx.commit()).await {
            Ok(Ok(())) => Ok(value),
            Ok(Err(_)) | Err(_) => {
                Err(PendingStorageError::OwnerBootstrapCommitIndeterminate { stage })
            }
        },
    }
}

pub(super) async fn capture_postgres_schema(
    db: &Database,
) -> Result<PostgresOwnerSchema, PendingStorageError> {
    let mut tx = db
        .begin_catalog_fenced_with_idle_bound(&[])
        .await
        .map_err(owner_error)?;
    let result = PostgresOwnerSchema::capture(&mut tx).await;
    if !tx.rollback_bounded_discard(OWNER_ROLLBACK_WAIT).await {
        return Err(owner_error(
            DatabaseError::CatalogMutationRollbackUnconfirmed,
        ));
    }
    result
}

async fn initialize_postgres(db: &Database) -> Result<(), PendingStorageError> {
    // Capture the target from this very checkout before imposing the safe path.
    let mut tx = db
        .begin_catalog_fenced_with_idle_bound(&[])
        .await
        .map_err(owner_error)?;
    let result = async {
        let schema = PostgresOwnerSchema::capture(&mut tx).await?;
        schema.prepare(&mut tx).await?;
        initialize_postgres_base(&mut tx, &schema).await?;
        Ok(schema)
    }
    .await;
    let schema = finish_postgres_stage(tx, result, "base").await?;
    initialize_postgres_receipts(db, &schema).await?;
    reset_postgres_claims(db, &schema).await?;
    cleanup_postgres_legacy_mam(db, &schema).await?;
    super::recovery_order::initialize_postgres_stage(db, &schema).await
}

async fn initialize_postgres_base(
    tx: &mut Transaction<'_>,
    schema: &PostgresOwnerSchema,
) -> Result<(), PendingStorageError> {
    let table = schema.relation("pending_delivery");
    let markers = schema.relation("pending_delivery_startup_migrations");
    tx.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {table} (\
        row_id TEXT PRIMARY KEY, recipient_jid TEXT NOT NULL, original_receipt_at BIGINT NOT NULL, \
        payload_kind TEXT NOT NULL, archive_stanza_by TEXT, archive_stanza_id TEXT, \
        transient_xml TEXT, flushed_in_session TEXT, outbound_sequence INTEGER, \
        notification_outboxed_at_ms BIGINT, claimed_at_ms BIGINT, claim_token TEXT, \
        claim_offered INTEGER NOT NULL DEFAULT 0)"
        ),
        (),
    )
    .await
    .map_err(owner_error)?;
    tx.execute(
        &format!("GRANT SELECT (row_id) ON {table} TO pg_monitor"),
        (),
    )
    .await
    .map_err(owner_error)?;
    widen_postgres_owner_column(tx, schema, "pending_delivery", "original_receipt_at").await?;
    for (name, definition) in [
        ("outbound_sequence", "INTEGER"),
        ("notification_outboxed_at_ms", "BIGINT"),
        ("claimed_at_ms", "BIGINT"),
        ("claim_token", "TEXT"),
        ("claim_offered", "INTEGER NOT NULL DEFAULT 1"),
        ("origin_stream_id", "TEXT"),
        ("inbound_seq", "BIGINT"),
        ("pair_sequence", "BIGINT"),
    ] {
        tx.execute(
            &format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS \"{name}\" {definition}"),
            (),
        )
        .await
        .map_err(owner_error)?;
    }
    for definition in [
        "UNIQUE INDEX IF NOT EXISTS idx_pending_delivery_dedup ON {table} (recipient_jid, origin_stream_id, inbound_seq) WHERE origin_stream_id IS NOT NULL AND inbound_seq IS NOT NULL",
        "INDEX IF NOT EXISTS idx_pending_delivery_recipient ON {table} (recipient_jid, row_id)",
        "INDEX IF NOT EXISTS idx_pending_delivery_session ON {table} (flushed_in_session)",
        "UNIQUE INDEX IF NOT EXISTS idx_pending_delivery_archived_unique ON {table} (recipient_jid, archive_stanza_id) WHERE payload_kind = 'archived'",
        "INDEX IF NOT EXISTS idx_pending_delivery_notification_outbox ON {table} (notification_outboxed_at_ms, row_id) WHERE payload_kind = 'archived' AND flushed_in_session IS NULL",
    ] {
        tx.execute(&format!("CREATE {}", definition.replace("{table}", &table)), ()).await.map_err(owner_error)?;
    }
    tx.execute(&format!("CREATE TABLE IF NOT EXISTS {markers} (name TEXT PRIMARY KEY, completed_at BIGINT NOT NULL)"), ()).await.map_err(owner_error)?;
    widen_postgres_owner_column(
        tx,
        schema,
        "pending_delivery_startup_migrations",
        "completed_at",
    )
    .await
}

async fn widen_postgres_owner_column(
    tx: &mut Transaction<'_>,
    schema: &PostgresOwnerSchema,
    table: &'static str,
    column: &'static str,
) -> Result<(), PendingStorageError> {
    let needs_widen = {
        let mut rows = tx.query(
            "SELECT a.atttypid <> 'pg_catalog.int8'::pg_catalog.regtype \
             FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.oid = ?::bigint::oid AND c.relname = ? AND a.attname = ? AND NOT a.attisdropped",
            crate::db_params![schema.oid, table, column],
        ).await.map_err(owner_error)?;
        match rows.next().await.map_err(owner_error)? {
            Some(row) => row.get::<bool>(0).map_err(owner_error)?,
            None => false,
        }
    };
    if needs_widen {
        tx.execute(
            &format!(
                "ALTER TABLE {} ALTER COLUMN \"{column}\" TYPE BIGINT",
                schema.relation(table)
            ),
            (),
        )
        .await
        .map_err(owner_error)?;
    }
    Ok(())
}

async fn initialize_postgres_receipts(
    db: &Database,
    schema: &PostgresOwnerSchema,
) -> Result<(), PendingStorageError> {
    let mut tx = begin_postgres_stage(
        db,
        schema,
        &[CatalogFenceLock::Exclusive(
            RECEIPT_SCHEMA_ADVISORY_LOCK_KEY,
        )],
    )
    .await?;
    let result = async {
        let table = schema.relation("pending_delivery_sm_promotions");
        tx.execute(&format!("CREATE TABLE IF NOT EXISTS {table} (origin_stream_id TEXT NOT NULL, sequence BIGINT NOT NULL, created_at_ms BIGINT NOT NULL DEFAULT 0, PRIMARY KEY (origin_stream_id, sequence))"), ()).await.map_err(owner_error)?;
        tx.execute(&format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS created_at_ms BIGINT NOT NULL DEFAULT 0"), ()).await.map_err(owner_error)?;
        Ok(())
    }.await;
    finish_postgres_stage(tx, result, "receipts").await
}

async fn reset_postgres_claims(
    db: &Database,
    schema: &PostgresOwnerSchema,
) -> Result<(), PendingStorageError> {
    let mut tx = begin_postgres_stage(
        db,
        schema,
        &[CatalogFenceLock::Exclusive(schema.reset_gate)],
    )
    .await?;
    let result = async {
        let markers = schema.relation("pending_delivery_startup_migrations");
        let done = {
            let mut rows = tx.query(&format!("SELECT 1 FROM {markers} WHERE name = ? LIMIT 1"), crate::db_params![V1014_CLAIM_RESET_MARKER]).await.map_err(owner_error)?;
            rows.next().await.map_err(owner_error)?.is_some()
        };
        if done { return Ok(0); }
        let released = tx.execute(&format!("UPDATE {} SET flushed_in_session = NULL, outbound_sequence = NULL, claimed_at_ms = NULL", schema.relation("pending_delivery")), ()).await.map_err(owner_error)?;
        tx.execute(&format!("INSERT INTO {markers} (name, completed_at) VALUES (?, ?) ON CONFLICT (name) DO NOTHING"), crate::db_params![V1014_CLAIM_RESET_MARKER, chrono::Utc::now().timestamp_millis()]).await.map_err(owner_error)?;
        Ok(released)
    }.await;
    let released = finish_postgres_stage(tx, result, "v1014_claim_reset").await?;
    if released > 0 {
        info!(
            released,
            "pending_delivery startup migration released claims discarded by ingress V1014"
        );
    }
    Ok(())
}

async fn begin_postgres_read_stage<'a>(
    db: &'a Database,
    schema: &PostgresOwnerSchema,
) -> Result<Transaction<'a>, PendingStorageError> {
    let mut tx = db
        .begin_catalog_fenced_read_only_with_idle_bound(&[])
        .await
        .map_err(owner_error)?;
    if let Err(error) = schema.prepare(&mut tx).await {
        return finish_postgres_read_stage(tx, Err(error)).await;
    }
    Ok(tx)
}

async fn finish_postgres_read_stage<T>(
    tx: Transaction<'_>,
    result: Result<T, PendingStorageError>,
) -> Result<T, PendingStorageError> {
    if !tx.rollback_bounded_discard(OWNER_ROLLBACK_WAIT).await {
        return Err(owner_error(
            DatabaseError::CatalogMutationRollbackUnconfirmed,
        ));
    }
    result
}

async fn cleanup_postgres_legacy_mam(
    db: &Database,
    schema: &PostgresOwnerSchema,
) -> Result<(), PendingStorageError> {
    let markers = schema.relation("pending_delivery_startup_migrations");
    let mut tx = begin_postgres_read_stage(db, schema).await?;
    let result = async {
        let mut rows = tx
            .query(
                &format!("SELECT 1 FROM {markers} WHERE name = ? LIMIT 1"),
                crate::db_params![LEGACY_MAM_CLEANUP_MARKER],
            )
            .await
            .map_err(owner_error)?;
        Ok(rows.next().await.map_err(owner_error)?.is_some())
    }
    .await;
    let done = finish_postgres_read_stage(tx, result).await?;
    if done {
        return Ok(());
    }
    let table = schema.relation("pending_delivery");
    let mut after = None::<String>;
    let mut deleted = 0u64;
    loop {
        let mut tx = begin_postgres_read_stage(db, schema).await?;
        let result = async {
            let (sql, params) = if let Some(after) = &after {
                (format!("SELECT row_id, transient_xml FROM {table} WHERE payload_kind = 'transient' AND transient_xml IS NOT NULL AND transient_xml LIKE '%urn:xmpp:mam:2%' AND row_id > ? ORDER BY row_id LIMIT ?"), crate::db_params![after, LEGACY_MAM_CLEANUP_SELECT_BATCH])
            } else {
                (format!("SELECT row_id, transient_xml FROM {table} WHERE payload_kind = 'transient' AND transient_xml IS NOT NULL AND transient_xml LIKE '%urn:xmpp:mam:2%' ORDER BY row_id LIMIT ?"), crate::db_params![LEGACY_MAM_CLEANUP_SELECT_BATCH])
            };
            let mut rows = tx.query(&sql, params).await.map_err(owner_error)?;
            let mut candidates = Vec::new();
            while let Some(row) = rows.next().await.map_err(owner_error)? {
                candidates.push((row.get::<String>(0).map_err(owner_error)?, row.get::<String>(1).map_err(owner_error)?));
            }
            Ok(candidates)
        }.await;
        let candidates = finish_postgres_read_stage(tx, result).await?;
        let Some((last, _)) = candidates.last() else {
            break;
        };
        after = Some(last.clone());
        let ids = candidates
            .into_iter()
            .filter_map(|(id, xml)| {
                let element = xml.parse::<xmpp_parsers::minidom::Element>().ok()?;
                let message = xmpp_parsers::message::Message::try_from(element).ok()?;
                waddle_xmpp_core::mam::is_mam_query_response_message(&message).then_some(id)
            })
            .collect::<Vec<_>>();
        if ids.is_empty() {
            continue;
        }
        let mut tx = begin_postgres_stage(db, schema, &[]).await?;
        let result = async {
            let placeholders = vec!["?"; ids.len()].join(", ");
            let params = ids.into_iter().map(Value::from).collect::<Vec<_>>();
            tx.execute(
                &format!("DELETE FROM {table} WHERE row_id IN ({placeholders})"),
                params,
            )
            .await
            .map_err(owner_error)
        }
        .await;
        deleted += finish_postgres_stage(tx, result, "legacy_mam_delete_batch").await?;
    }
    let mut tx = begin_postgres_stage(db, schema, &[]).await?;
    let result = async {
        tx.execute(
            &format!(
                "INSERT INTO {} (name, completed_at) VALUES (?, ?) ON CONFLICT (name) DO NOTHING",
                schema.relation("pending_delivery_startup_migrations")
            ),
            crate::db_params![
                LEGACY_MAM_CLEANUP_MARKER,
                chrono::Utc::now().timestamp_millis()
            ],
        )
        .await
        .map_err(owner_error)?;
        Ok(())
    }
    .await;
    finish_postgres_stage(tx, result, "legacy_mam_marker").await?;
    if deleted > 0 {
        info!(
            deleted,
            "pending_delivery startup cleanup removed XEP-0313 MAM query frames"
        );
    }
    Ok(())
}

async fn initialize(storage: &OwnerSchemaDb<'_>) -> Result<(), PendingStorageError> {
    // `original_receipt_at` stores `timestamp_millis()` (i64 ms since
    // unix epoch). Postgres `INTEGER` is i32 (max ~2.1B), which
    // overflows on every insert from 2001-09-09 onward — every prod
    // write since the column was introduced in #339 has failed with
    // `numeric_value_out_of_range`, breaking XEP-0160 offline DM
    // delivery on SM session resume / detach promotion. Sibling
    // `sm_persistence/schema.rs` already uses this driver-aware
    // selector for the same reason. SQLite `INTEGER` is dynamic-width,
    // so the same DDL stays correct there.
    let bigint = timestamp_millis_sql_type(storage.db.driver());
    storage
        .execute(
            &format!(
                r#"
        CREATE TABLE IF NOT EXISTS pending_delivery (
            row_id TEXT PRIMARY KEY,
            recipient_jid TEXT NOT NULL,
            original_receipt_at {bigint} NOT NULL,
            payload_kind TEXT NOT NULL,
            archive_stanza_by TEXT,
            archive_stanza_id TEXT,
            transient_xml TEXT,
            flushed_in_session TEXT,
            outbound_sequence INTEGER,
            notification_outboxed_at_ms {bigint},
            claimed_at_ms {bigint},
            claim_token TEXT,
            claim_offered INTEGER NOT NULL DEFAULT 0
        )
        "#
            ),
            (),
        )
        .await?;
    initialize_promotion_receipts(storage).await?;
    // Online migration for existing Postgres tables that were created
    // with the pre-fix `INTEGER` (i32) shape. Widening to BIGINT is
    // lossless — every value that actually made it in fits in i32
    // anyway (writes past i32::MAX rejected at the wire). SQLite
    // columns are dynamic-width, so the type doesn't need rewriting
    // there.
    //
    // Gate the ALTER on the current column type so subsequent restarts
    // (and racing replicas in a rolling deploy) do not re-issue an
    // `ALTER COLUMN ... TYPE BIGINT` that still takes ACCESS EXCLUSIVE
    // even when the column is already BIGINT.
    if matches!(storage.db.driver(), DatabaseDriver::Postgres) {
        // GC monitoring checks retained row identities without reading content.
        storage
            .execute(
                "GRANT SELECT (row_id) ON pending_delivery TO pg_monitor",
                (),
            )
            .await?;
        widen_postgres_timestamp_millis_column_to_bigint(
            storage,
            "pending_delivery",
            "original_receipt_at",
        )
        .await?;
    }
    // Idempotent column-add migration for the locked Q7b
    // outbound_sequence column. Tables created by an older
    // version of waddle-server (before PR #358) were missing this
    // column, and `CREATE TABLE IF NOT EXISTS` is a no-op when the
    // table already exists — so the SELECT/INSERT/UPDATE statements
    // below would fail with "no such column: outbound_sequence" at
    // first use without this ALTER. (Codex/Qodo review on PR #358.)
    //
    // Both backends support `ADD COLUMN IF NOT EXISTS` syntax in
    // recent versions (SQLite ≥ 3.35.0, Postgres ≥ 9.6); for older
    // SQLite we fall through to a tolerant ALTER + best-effort
    // ignore of the "duplicate column" error.
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS outbound_sequence INTEGER"
        }
        DatabaseDriver::Sqlite => {
            "ALTER TABLE pending_delivery ADD COLUMN outbound_sequence INTEGER"
        }
    };
    if let Err(error) = storage.execute(alter_sql, ()).await {
        // SQLite's `ALTER TABLE … ADD COLUMN` is not idempotent
        // and reports "duplicate column name" when the column
        // already exists. Treat that specific error as a no-op so
        // the migration stays idempotent for both freshly-created
        // tables (where the column exists from CREATE TABLE
        // above) and pre-existing older tables.
        let msg = error.to_string().to_lowercase();
        if msg.contains("duplicate column") || msg.contains("already exists") {
            debug!("pending_delivery.outbound_sequence column already present");
        } else {
            return Err(error);
        }
    }
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS notification_outboxed_at_ms BIGINT"
        }
        DatabaseDriver::Sqlite => {
            "ALTER TABLE pending_delivery ADD COLUMN notification_outboxed_at_ms INTEGER"
        }
    };
    if let Err(error) = storage.execute(alter_sql, ()).await {
        let msg = error.to_string().to_lowercase();
        if msg.contains("duplicate column") || msg.contains("already exists") {
            debug!("pending_delivery.notification_outboxed_at_ms column already present");
        } else {
            return Err(error);
        }
    }
    // #1124: claim recency stamp. `claim_for_session` /
    // `claim_batch_for_session` set it alongside `flushed_in_session`;
    // the release paths clear both together. The claim-expiry janitor
    // only releases claims older than its recency floor, so an
    // in-flight non-SM (`transient:`) flush — whose synthetic session
    // id is never in the live-set — cannot have its claims stolen
    // mid-flush by an overlapping janitor pass.
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS claimed_at_ms BIGINT"
        }
        DatabaseDriver::Sqlite => "ALTER TABLE pending_delivery ADD COLUMN claimed_at_ms INTEGER",
    };
    if let Err(error) = storage.execute(alter_sql, ()).await {
        let msg = error.to_string().to_lowercase();
        if msg.contains("duplicate column") || msg.contains("already exists") {
            debug!("pending_delivery.claimed_at_ms column already present");
        } else {
            return Err(error);
        }
    }
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS claim_token TEXT"
        }
        DatabaseDriver::Sqlite => "ALTER TABLE pending_delivery ADD COLUMN claim_token TEXT",
    };
    if let Err(error) = storage.execute(alter_sql, ()).await {
        let message = error.to_string().to_lowercase();
        if !message.contains("duplicate column") && !message.contains("already exists") {
            return Err(error);
        }
    }
    // Pre-reservation claims have unknown offer status. Preserve them rather
    // than infer that an already queued, unsequenced row was never offered.
    // Every new claim explicitly initializes this reservation marker to zero.
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS claim_offered INTEGER NOT NULL DEFAULT 1",
        DatabaseDriver::Sqlite => "ALTER TABLE pending_delivery ADD COLUMN claim_offered INTEGER NOT NULL DEFAULT 1",
    };
    if let Err(error) = storage.execute(alter_sql, ()).await {
        let message = error.to_string().to_lowercase();
        if !message.contains("duplicate column") && !message.contains("already exists") {
            return Err(error);
        }
    }
    // ADR-0017 Phase 3 Slice 5 / element 5 — schema-only groundwork
    // (PROPOSED column names/types, per the ADR Implementation Plan's "the
    // schema is specified in one place here to prevent dropping a column
    // group" instruction). Net-new on this table: the origin session's
    // SM-ID (or a server-assigned per-stream id for non-SM origins) and its
    // XEP-0198 inbound `h` value at the moment this stanza was ingested —
    // together the recipient-scoped dedup key `(recipient_jid,
    // origin_stream_id, inbound_seq)` — plus a per-`(origin_stream_id,
    // recipient)` ordering sequence number for sticky-failover gap
    // detection. All three stay NULL until a same-node/cross-node caller
    // populates them (no insert call site does yet — see this migration's
    // own doc note below); `original_receipt_at` (already present, above)
    // already covers the XEP-0203 ingress-timestamp requirement.
    //
    // **Deferred, recorded as a deviation**: populating these columns
    // requires threading the origin stream's id and `h` value through
    // every `pending_delivery` insert call site (Q6 promotion, direct
    // offline-DM fallback, MUC reflection fan-out) and building the
    // per-pair ordering/gap-detection consumer — cross-node stanza
    // ordering/dedup machinery the phase's own Non-goals exclude this
    // phase (no cross-node stanza routing GA; the cross-node
    // janitor-flush leg is deferred to Phase 4). Landing the columns now
    // (nullable, unpopulated) avoids a second migration once that
    // machinery lands, without pretending same-node-only Slice 5 needs
    // them populated yet.
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS origin_stream_id TEXT"
        }
        DatabaseDriver::Sqlite => "ALTER TABLE pending_delivery ADD COLUMN origin_stream_id TEXT",
    };
    if let Err(error) = storage.execute(alter_sql, ()).await {
        let msg = error.to_string().to_lowercase();
        if msg.contains("duplicate column") || msg.contains("already exists") {
            debug!("pending_delivery.origin_stream_id column already present");
        } else {
            return Err(error);
        }
    }
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS inbound_seq BIGINT"
        }
        DatabaseDriver::Sqlite => "ALTER TABLE pending_delivery ADD COLUMN inbound_seq INTEGER",
    };
    if let Err(error) = storage.execute(alter_sql, ()).await {
        let msg = error.to_string().to_lowercase();
        if msg.contains("duplicate column") || msg.contains("already exists") {
            debug!("pending_delivery.inbound_seq column already present");
        } else {
            return Err(error);
        }
    }
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            "ALTER TABLE pending_delivery ADD COLUMN IF NOT EXISTS pair_sequence BIGINT"
        }
        DatabaseDriver::Sqlite => "ALTER TABLE pending_delivery ADD COLUMN pair_sequence INTEGER",
    };
    if let Err(error) = storage.execute(alter_sql, ()).await {
        let msg = error.to_string().to_lowercase();
        if msg.contains("duplicate column") || msg.contains("already exists") {
            debug!("pending_delivery.pair_sequence column already present");
        } else {
            return Err(error);
        }
    }
    // Recipient-scoped dedup key (element 5, property 0: scoped by
    // recipient, never table-global — a fan-out of one origin stanza to N
    // recipients must insert N rows). Partial: only rows that actually
    // carry both dedup dimensions participate — legacy/not-yet-populated
    // rows (both NULL) never collide under this index regardless, but the
    // `WHERE` clause makes that explicit rather than relying on Postgres's
    // "NULLs are distinct" default.
    storage
        .execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_delivery_dedup \
         ON pending_delivery (recipient_jid, origin_stream_id, inbound_seq) \
         WHERE origin_stream_id IS NOT NULL AND inbound_seq IS NOT NULL",
            (),
        )
        .await?;
    storage
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_delivery_recipient \
         ON pending_delivery (recipient_jid, row_id)",
            (),
        )
        .await?;
    storage
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_delivery_session \
         ON pending_delivery (flushed_in_session)",
            (),
        )
        .await?;
    // UNIQUE partial index on (recipient_jid, archive_stanza_id)
    // for Archived rows. XEP-0359 stanza-ids are unique per
    // archive (recipient bare JID); two pending_delivery rows
    // pointing at the same MAM entry would replay the same
    // message twice. Both SQLite (since 3.8.0) and Postgres
    // support partial indexes; the WHERE clause limits the
    // constraint to Archived rows so multiple Transient inserts
    // for the same recipient remain allowed (the typed
    // PendingPayload::Transient variant has no archive id to
    // collide on).
    storage
        .execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_pending_delivery_archived_unique \
         ON pending_delivery (recipient_jid, archive_stanza_id) \
         WHERE payload_kind = 'archived'",
            (),
        )
        .await?;
    storage
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_pending_delivery_notification_outbox \
         ON pending_delivery (notification_outboxed_at_ms, row_id) \
         WHERE payload_kind = 'archived' AND flushed_in_session IS NULL",
            (),
        )
        .await?;
    ensure_startup_migration_marker_table(storage).await?;
    reset_claims_for_ingress_v1014_once(storage).await?;
    if !startup_migration_marker_completed(storage, LEGACY_MAM_CLEANUP_MARKER).await? {
        let deleted_mam_frames = delete_legacy_mam_query_frames(storage).await?;
        mark_startup_migration_completed(storage, LEGACY_MAM_CLEANUP_MARKER).await?;
        if deleted_mam_frames > 0 {
            info!(
                deleted = deleted_mam_frames,
                "pending_delivery startup cleanup removed XEP-0313 MAM query frames"
            );
        }
    }
    super::recovery_order::initialize(storage.db).await
}

/// Release pending rows claimed by SM sessions discarded by the V1014
/// ingress cutover. `pending_delivery` owns its schema, so this transition
/// lives here rather than in the global migration ledger.
///
/// The marker check, exact three-column release update, and marker insert are
/// one transaction. Postgres serializes concurrent replicas with a
/// transaction-scoped advisory lock; SQLite's `BEGIN IMMEDIATE` provides the
/// equivalent single-writer exclusion. Re-checking the marker under that lock
/// prevents a later replica from clearing a claim acquired after the first
/// replica completed the cutover reset.
async fn reset_claims_for_ingress_v1014_once(
    storage: &OwnerSchemaDb<'_>,
) -> Result<(), PendingStorageError> {
    let mut tx = storage
        .db
        .begin_immediate()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    if matches!(tx.driver(), DatabaseDriver::Postgres) {
        tx.execute(
            "SELECT pg_advisory_xact_lock(hashtext(?))",
            crate::db_params![V1014_CLAIM_RESET_MARKER],
        )
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    }

    let mut rows = tx
        .query(
            "SELECT 1 FROM pending_delivery_startup_migrations WHERE name = ? LIMIT 1",
            crate::db_params![V1014_CLAIM_RESET_MARKER],
        )
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    let already_completed = rows
        .next()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?
        .is_some();
    drop(rows);
    if already_completed {
        tx.commit()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        return Ok(());
    }

    let released = tx
        .execute(
            "UPDATE pending_delivery SET flushed_in_session = NULL, \
                                         outbound_sequence = NULL, \
                                         claimed_at_ms = NULL",
            (),
        )
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    let marker_sql = match tx.driver() {
        DatabaseDriver::Postgres => {
            "INSERT INTO pending_delivery_startup_migrations (name, completed_at) \
             VALUES (?, ?) ON CONFLICT (name) DO NOTHING"
        }
        DatabaseDriver::Sqlite => {
            "INSERT OR IGNORE INTO pending_delivery_startup_migrations (name, completed_at) \
             VALUES (?, ?)"
        }
    };
    tx.execute(
        marker_sql,
        crate::db_params![
            V1014_CLAIM_RESET_MARKER,
            chrono::Utc::now().timestamp_millis()
        ],
    )
    .await
    .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    tx.commit()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    if released > 0 {
        info!(
            released,
            "pending_delivery startup migration released claims discarded by ingress V1014"
        );
    }
    Ok(())
}

async fn ensure_startup_migration_marker_table(
    storage: &OwnerSchemaDb<'_>,
) -> Result<(), PendingStorageError> {
    let completed_at_type = timestamp_millis_sql_type(storage.db.driver());
    storage
        .execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS pending_delivery_startup_migrations (\
                name TEXT PRIMARY KEY, \
                completed_at {completed_at_type} NOT NULL\
             )"
            ),
            (),
        )
        .await?;
    if matches!(storage.db.driver(), DatabaseDriver::Postgres) {
        widen_postgres_timestamp_millis_column_to_bigint(
            storage,
            "pending_delivery_startup_migrations",
            "completed_at",
        )
        .await?;
    }
    Ok(())
}

fn timestamp_millis_sql_type(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Postgres => "BIGINT",
        DatabaseDriver::Sqlite => "INTEGER",
    }
}

async fn widen_postgres_timestamp_millis_column_to_bigint(
    storage: &OwnerSchemaDb<'_>,
    table: &'static str,
    column: &'static str,
) -> Result<(), PendingStorageError> {
    // Constrain by `table_schema = current_schema()` so the probe
    // looks at the same table the unqualified `ALTER TABLE` below
    // would hit via `search_path`.
    let mut rows = storage
        .query(
            "SELECT data_type \
             FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = ? \
               AND column_name = ?",
            crate::db_params![table, column],
        )
        .await?;
    let current_type: Option<String> = match rows
        .next()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?
    {
        Some(row) => row
            .get(0)
            .map_err(|error| PendingStorageError::Other(error.to_string()))?,
        None => None,
    };
    let needs_widen = current_type
        .as_deref()
        .is_some_and(|t| !t.eq_ignore_ascii_case("bigint"));
    if needs_widen {
        storage
            .execute(
                &format!("ALTER TABLE {table} ALTER COLUMN {column} TYPE BIGINT"),
                (),
            )
            .await?;
    }
    Ok(())
}

async fn startup_migration_marker_completed(
    storage: &OwnerSchemaDb<'_>,
    marker: &str,
) -> Result<bool, PendingStorageError> {
    let mut rows = storage
        .query(
            "SELECT 1 FROM pending_delivery_startup_migrations \
             WHERE name = ? \
             LIMIT 1",
            crate::db_params![marker],
        )
        .await?;
    Ok(rows
        .next()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?
        .is_some())
}

async fn mark_startup_migration_completed(
    storage: &OwnerSchemaDb<'_>,
    marker: &str,
) -> Result<(), PendingStorageError> {
    let sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            "INSERT INTO pending_delivery_startup_migrations (name, completed_at) \
             VALUES (?, ?) \
             ON CONFLICT (name) DO NOTHING"
        }
        DatabaseDriver::Sqlite => {
            "INSERT OR IGNORE INTO pending_delivery_startup_migrations (name, completed_at) \
             VALUES (?, ?)"
        }
    };
    storage
        .execute(
            sql,
            crate::db_params![marker, chrono::Utc::now().timestamp_millis()],
        )
        .await?;
    Ok(())
}

async fn delete_legacy_mam_query_frames(
    storage: &OwnerSchemaDb<'_>,
) -> Result<u64, PendingStorageError> {
    let mut after_row_id = None;
    let mut deleted = 0;
    loop {
        let candidates = legacy_mam_query_frame_candidate_batch(storage, after_row_id).await?;
        let Some((last_row_id, _)) = candidates.last() else {
            break;
        };
        after_row_id = Some(last_row_id.clone());

        let mut row_ids = Vec::new();
        for (row_id, xml) in candidates {
            let Ok(element) = xml.parse::<xmpp_parsers::minidom::Element>() else {
                continue;
            };
            let Ok(message) = xmpp_parsers::message::Message::try_from(element) else {
                continue;
            };
            if waddle_xmpp_core::mam::is_mam_query_response_message(&message) {
                row_ids.push(row_id);
            }
        }
        deleted += delete_legacy_mam_query_frame_rows(storage, &row_ids).await?;
    }

    Ok(deleted)
}

async fn legacy_mam_query_frame_candidate_batch(
    storage: &OwnerSchemaDb<'_>,
    after_row_id: Option<String>,
) -> Result<Vec<(String, String)>, PendingStorageError> {
    let (sql, params) = match after_row_id {
        Some(row_id) => (
            "SELECT row_id, transient_xml \
             FROM pending_delivery \
             WHERE payload_kind = 'transient' \
               AND transient_xml IS NOT NULL \
               AND transient_xml LIKE '%urn:xmpp:mam:2%' \
               AND row_id > ? \
             ORDER BY row_id \
             LIMIT ?",
            crate::db_params![row_id, LEGACY_MAM_CLEANUP_SELECT_BATCH],
        ),
        None => (
            "SELECT row_id, transient_xml \
             FROM pending_delivery \
             WHERE payload_kind = 'transient' \
               AND transient_xml IS NOT NULL \
               AND transient_xml LIKE '%urn:xmpp:mam:2%' \
             ORDER BY row_id \
             LIMIT ?",
            crate::db_params![LEGACY_MAM_CLEANUP_SELECT_BATCH],
        ),
    };
    let mut rows = storage.query(sql, params).await?;
    let mut candidates = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?
    {
        let row_id: String = row
            .get(0)
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        let xml: String = row
            .get(1)
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        candidates.push((row_id, xml));
    }
    Ok(candidates)
}

async fn delete_legacy_mam_query_frame_rows(
    storage: &OwnerSchemaDb<'_>,
    row_ids: &[String],
) -> Result<u64, PendingStorageError> {
    if row_ids.is_empty() {
        return Ok(0);
    }

    let mut tx = storage
        .db
        .begin()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    let mut deleted = 0;
    for chunk in row_ids.chunks(LEGACY_MAM_CLEANUP_DELETE_BATCH) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!("DELETE FROM pending_delivery WHERE row_id IN ({placeholders})");
        let params = chunk.iter().cloned().map(Value::from).collect::<Vec<_>>();
        deleted += tx
            .execute(&sql, params)
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    }
    tx.commit()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    Ok(deleted)
}

#[cfg(test)]
mod slice5_dedup_index_tests {
    use crate::pending_delivery::database::{DatabasePendingDeliveryStorage, QuotaPolicy};

    /// ADR-0017 Phase 3 Slice 5 / element 5: the recipient-scoped dedup
    /// index (`idx_pending_delivery_dedup`) rejects a second row sharing
    /// `(recipient_jid, origin_stream_id, inbound_seq)`, but a different
    /// recipient with the SAME `(origin_stream_id, inbound_seq)` pair is
    /// unconstrained (property 0: one origin stanza legitimately fans out
    /// to many recipients). Exercises the migration directly via raw SQL
    /// since no application call site populates these columns yet (see
    /// this migration's own doc comment).
    #[tokio::test]
    async fn dedup_index_rejects_same_recipient_duplicate_but_allows_fan_out() {
        let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
            .await
            .expect("open in-memory storage (runs the migration)");

        let insert_row = |row_id: &'static str, recipient: &'static str| {
            let storage = &storage;
            async move {
                storage
                    .execute(
                        "INSERT INTO pending_delivery \
                         (row_id, recipient_jid, original_receipt_at, payload_kind, \
                          origin_stream_id, inbound_seq) \
                         VALUES (?, ?, ?, 'transient', ?, ?)",
                        crate::db_params![
                            row_id.to_string(),
                            recipient.to_string(),
                            0i64,
                            "origin-stream-1".to_string(),
                            7i64,
                        ],
                    )
                    .await
            }
        };

        insert_row("row-1", "alice@example.com")
            .await
            .expect("first insert for alice succeeds");
        insert_row("row-2", "alice@example.com").await.expect_err(
            "a second row for the SAME recipient sharing (origin_stream_id, inbound_seq) \
                 must violate the dedup unique index",
        );
        insert_row("row-3", "bob@example.com").await.expect(
            "a DIFFERENT recipient sharing the same (origin_stream_id, inbound_seq) pair \
                 must be allowed (fan-out property)",
        );
    }
}

#[cfg(test)]
mod ingress_v1014_claim_reset_tests {
    use crate::pending_delivery::DatabasePendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::QuotaPolicy;

    use super::*;

    #[tokio::test]
    async fn sqlite_reset_preserves_rows_and_clears_exact_claim_columns_once() {
        let storage = DatabasePendingDeliveryStorage::open(None, QuotaPolicy::Unlimited)
            .await
            .expect("open storage");
        storage
            .execute(
                "DELETE FROM pending_delivery_startup_migrations WHERE name = ?",
                crate::db_params![V1014_CLAIM_RESET_MARKER],
            )
            .await
            .expect("rewind marker for legacy fixture");
        for (row_id, session) in [
            ("claimed-sm", "sm-session"),
            ("claimed-transient", "transient:flush"),
        ] {
            storage
                .execute(
                    "INSERT INTO pending_delivery (\
                        row_id, recipient_jid, original_receipt_at, payload_kind, transient_xml, \
                        flushed_in_session, outbound_sequence, claimed_at_ms\
                     ) VALUES (?, 'alice@example.com', 1, 'transient', \
                        '<message xmlns=\"jabber:client\"/>', ?, 7, 9)",
                    crate::db_params![row_id, session],
                )
                .await
                .expect("seed claimed pending row");
        }

        let db = storage.database();
        let owner_schema = OwnerSchemaDb { db: &db };
        reset_claims_for_ingress_v1014_once(&owner_schema)
            .await
            .expect("run one-time claim reset");
        let mut rows = storage
            .query(
                "SELECT COUNT(*), \
                        SUM(CASE WHEN flushed_in_session IS NULL THEN 1 ELSE 0 END), \
                        SUM(CASE WHEN outbound_sequence IS NULL THEN 1 ELSE 0 END), \
                        SUM(CASE WHEN claimed_at_ms IS NULL THEN 1 ELSE 0 END) \
                 FROM pending_delivery WHERE row_id IN ('claimed-sm', 'claimed-transient')",
                (),
            )
            .await
            .expect("query reset rows");
        let row = rows.next().await.expect("advance").expect("aggregate row");
        for index in 0..4 {
            let count: i64 = row.get(index).expect("decode count");
            assert_eq!(count, 2);
        }
        drop(rows);

        storage
            .execute(
                "UPDATE pending_delivery SET flushed_in_session = 'new-session', \
                     outbound_sequence = 8, claimed_at_ms = 10 WHERE row_id = 'claimed-sm'",
                (),
            )
            .await
            .expect("claim after migration");
        reset_claims_for_ingress_v1014_once(&owner_schema)
            .await
            .expect("idempotent rerun");
        let mut rows = storage
            .query(
                "SELECT flushed_in_session, outbound_sequence, claimed_at_ms \
                 FROM pending_delivery WHERE row_id = 'claimed-sm'",
                (),
            )
            .await
            .expect("query post-marker claim");
        let row = rows.next().await.expect("advance").expect("claimed row");
        assert_eq!(
            row.get::<Option<String>>(0).expect("session"),
            Some("new-session".into())
        );
        assert_eq!(row.get::<Option<i64>>(1).expect("sequence"), Some(8));
        assert_eq!(row.get::<Option<i64>>(2).expect("claimed at"), Some(10));
    }
}

/// Bootstrap receipts independently of the ingress ledger. Serialize both table
/// creation and additive migration: `IF NOT EXISTS` alone races in PostgreSQL's
/// type catalog when replicas start together.
async fn initialize_promotion_receipts(
    storage: &OwnerSchemaDb<'_>,
) -> Result<(), PendingStorageError> {
    if storage.db.driver() == DatabaseDriver::Postgres {
        let schema = capture_postgres_schema(storage.db).await?;
        return initialize_postgres_receipts(storage.db, &schema).await;
    }
    let mut tx = storage
        .db
        .begin_immediate()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    if matches!(storage.db.driver(), DatabaseDriver::Postgres) {
        tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        tx.query(
            "SELECT pg_advisory_xact_lock(?)",
            crate::db_params![RECEIPT_SCHEMA_ADVISORY_LOCK_KEY],
        )
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    }
    tx.execute(
        "CREATE TABLE IF NOT EXISTS pending_delivery_sm_promotions (\
            origin_stream_id TEXT NOT NULL, \
            sequence BIGINT NOT NULL, \
            created_at_ms BIGINT NOT NULL DEFAULT 0, \
            PRIMARY KEY (origin_stream_id, sequence)\
        )",
        (),
    )
    .await
    .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    // Existing receipts predate this stamp. Zero means unknown age and only
    // selects them for source verification; age never authorizes deletion.
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => "ALTER TABLE pending_delivery_sm_promotions ADD COLUMN IF NOT EXISTS created_at_ms BIGINT NOT NULL DEFAULT 0",
        DatabaseDriver::Sqlite => "ALTER TABLE pending_delivery_sm_promotions ADD COLUMN created_at_ms BIGINT NOT NULL DEFAULT 0",
    };
    if matches!(storage.db.driver(), DatabaseDriver::Sqlite) {
        let mut columns = tx
            .query("PRAGMA table_info(pending_delivery_sm_promotions)", ())
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        let mut has_timestamp = false;
        while let Some(column) = columns
            .next()
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?
        {
            let name: String = column
                .get(1)
                .map_err(|error| PendingStorageError::Other(error.to_string()))?;
            has_timestamp |= name == "created_at_ms";
        }
        if !has_timestamp {
            tx.execute(alter_sql, ())
                .await
                .map_err(|error| PendingStorageError::Other(error.to_string()))?;
        }
    } else {
        tx.execute(alter_sql, ())
            .await
            .map_err(|error| PendingStorageError::Other(error.to_string()))?;
    }
    tx.commit()
        .await
        .map_err(|error| PendingStorageError::Other(error.to_string()))
}

#[cfg(test)]
mod tests {
    use crate::db::{Database, DatabaseConfig};
    use crate::pending_delivery::DatabasePendingDeliveryStorage;
    use waddle_xmpp::pending_delivery::QuotaPolicy;

    use super::*;

    #[tokio::test]
    async fn postgres_concurrent_receipt_bootstrap_is_serialized() {
        let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
            return;
        };
        let schema = format!("receipt_bootstrap_{}", uuid::Uuid::new_v4().simple());
        let admin = Database::from_config(
            "receipt_bootstrap_admin",
            &DatabaseConfig::new(DatabaseDriver::Postgres, database_url.clone()),
        )
        .await
        .expect("admin database");
        admin
            .guard()
            .await
            .expect("guard")
            .execute(&format!("CREATE SCHEMA {schema}"), ())
            .await
            .expect("create schema");
        let mut scoped_url = url::Url::parse(&database_url).expect("database URL");
        scoped_url
            .query_pairs_mut()
            .append_pair("options", &format!("-csearch_path={schema}"));
        let first =
            DatabasePendingDeliveryStorage::open(Some(scoped_url.as_str()), QuotaPolicy::Unlimited)
                .await
                .expect("initial storage");
        let second =
            DatabasePendingDeliveryStorage::open(Some(scoped_url.as_str()), QuotaPolicy::Unlimited)
                .await
                .expect("second database pool");
        first
            .execute("DROP TABLE pending_delivery_sm_promotions", ())
            .await
            .expect("reset receipt bootstrap only");
        // Existing neighbouring tables deliberately remain, isolating the new
        // receipt DDL from their independent legacy initialization paths.
        let first_db = first.database();
        let second_db = second.database();
        let first_owner_schema = OwnerSchemaDb { db: &first_db };
        let second_owner_schema = OwnerSchemaDb { db: &second_db };
        let (left, right) = tokio::join!(
            initialize_promotion_receipts(&first_owner_schema),
            initialize_promotion_receipts(&second_owner_schema)
        );
        left.expect("first concurrent bootstrap");
        right.expect("second concurrent bootstrap");
        first.execute("INSERT INTO pending_delivery_sm_promotions (origin_stream_id, sequence, created_at_ms) VALUES ('source', 1, 1)", ()).await.expect("initialized receipt table usable");
        admin
            .guard()
            .await
            .expect("guard")
            .execute(&format!("DROP SCHEMA {schema} CASCADE"), ())
            .await
            .expect("drop schema");
    }
}
