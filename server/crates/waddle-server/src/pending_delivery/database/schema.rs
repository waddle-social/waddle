use super::*;

const LEGACY_MAM_CLEANUP_MARKER: &str = "legacy_mam_query_frames_v1";
const LEGACY_MAM_CLEANUP_SELECT_BATCH: i64 = 128;
const LEGACY_MAM_CLEANUP_DELETE_BATCH: usize = 128;

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
            claimed_at_ms {bigint}
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
            // Use a new name rather than changing the definition of the
            // legacy single-column `idx_pending_delivery_session` in place:
            // `IF NOT EXISTS` matches by name, so reusing it would silently
            // leave upgraded databases without the row-id ordering key.
            // Existing databases may retain the redundant legacy index;
            // fresh databases create only this composite access path.
            "CREATE INDEX IF NOT EXISTS idx_pending_delivery_session_row_id \
         ON pending_delivery (flushed_in_session, row_id)",
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
    Ok(())
}

async fn ensure_startup_migration_marker_table(
    storage: &DatabasePendingDeliveryStorage,
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
    storage: &DatabasePendingDeliveryStorage,
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
    storage: &DatabasePendingDeliveryStorage,
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
    storage: &DatabasePendingDeliveryStorage,
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
    storage: &DatabasePendingDeliveryStorage,
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
    storage: &DatabasePendingDeliveryStorage,
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
    storage: &DatabasePendingDeliveryStorage,
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
        let params = chunk
            .iter()
            .cloned()
            .map(crate::db::Value::from)
            .collect::<Vec<_>>();
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
