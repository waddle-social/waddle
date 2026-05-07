use super::*;

pub(super) async fn initialize(
    storage: &DatabasePendingDeliveryStorage,
) -> Result<(), PendingStorageError> {
    storage
        .execute(
            r#"
        CREATE TABLE IF NOT EXISTS pending_delivery (
            row_id TEXT PRIMARY KEY,
            recipient_jid TEXT NOT NULL,
            original_receipt_at INTEGER NOT NULL,
            payload_kind TEXT NOT NULL,
            archive_stanza_by TEXT,
            archive_stanza_id TEXT,
            transient_xml TEXT,
            flushed_in_session TEXT,
            outbound_sequence INTEGER
        )
        "#,
            (),
        )
        .await?;
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
