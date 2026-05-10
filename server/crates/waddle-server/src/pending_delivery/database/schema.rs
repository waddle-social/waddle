use super::*;

pub(super) async fn initialize(
    storage: &DatabasePendingDeliveryStorage,
) -> Result<(), PendingStorageError> {
    // `original_receipt_at` stores `timestamp_millis()` (i64 ms since
    // unix epoch). Postgres `INTEGER` is i32 (max ~2.1B), which
    // overflows on every insert from 2001-09-09 onward — every prod
    // write since the column was introduced in #339 has failed with
    // `numeric_value_out_of_range`, breaking XEP-0160 offline DM
    // delivery on SM session resume / detach promotion. Sibling
    // `sm_persistence/schema.rs` already uses this driver-aware
    // selector for the same reason. SQLite `INTEGER` is dynamic-width,
    // so the same DDL stays correct there.
    let bigint = match storage.db.driver() {
        DatabaseDriver::Postgres => "BIGINT",
        DatabaseDriver::Sqlite => "INTEGER",
    };
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
            outbound_sequence INTEGER
        )
        "#
            ),
            (),
        )
        .await?;
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
        // Constrain by `table_schema = current_schema()` so the probe
        // looks at the same table the unqualified `ALTER TABLE
        // pending_delivery` below would hit (which resolves via
        // `search_path`). Without this, in databases that contain
        // `pending_delivery` in multiple schemas, the probe could read
        // a sibling table's type and incorrectly skip the widening
        // (leaving the overflow bug in place) or trigger an ALTER
        // against a table whose type was already correct.
        let mut rows = storage
            .query(
                "SELECT data_type \
                 FROM information_schema.columns \
                 WHERE table_schema = current_schema() \
                   AND table_name = 'pending_delivery' \
                   AND column_name = 'original_receipt_at'",
                (),
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
        // `information_schema.columns.data_type` reports `integer` for
        // int4 and `bigint` for int8 — case is lowercase per the SQL
        // standard. Compare lowercase to be defensive against any
        // future Postgres surface changes.
        let needs_widen = current_type
            .as_deref()
            .is_some_and(|t| !t.eq_ignore_ascii_case("bigint"));
        if needs_widen {
            storage
                .execute(
                    "ALTER TABLE pending_delivery \
                     ALTER COLUMN original_receipt_at TYPE BIGINT",
                    (),
                )
                .await?;
        }
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
    Ok(())
}
