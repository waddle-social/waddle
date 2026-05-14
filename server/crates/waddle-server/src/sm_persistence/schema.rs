use super::*;

async fn add_column_if_missing(
    storage: &DatabaseSmPersistence,
    table: &str,
    column_def: &str,
) -> Result<(), SmPersistenceError> {
    let alter_sql = match storage.db.driver() {
        DatabaseDriver::Postgres => {
            format!("ALTER TABLE {table} ADD COLUMN IF NOT EXISTS {column_def}")
        }
        DatabaseDriver::Sqlite => format!("ALTER TABLE {table} ADD COLUMN {column_def}"),
    };
    if let Err(error) = storage.execute(&alter_sql, ()).await {
        let msg = error.to_string().to_lowercase();
        if msg.contains("duplicate column") || msg.contains("already exists") {
            debug!(table, column_def, "column already present; skipping ALTER");
            return Ok(());
        }
        return Err(error);
    }
    Ok(())
}

pub(super) async fn initialize(storage: &DatabaseSmPersistence) -> Result<(), SmPersistenceError> {
    // Driver-aware bigint type: Postgres INTEGER is i32 (overflows
    // for `timestamp_millis()` after Jan 2038); BIGINT is i64.
    // SQLite INTEGER is dynamically sized so the same DDL works.
    let bigint = crate::db::i64_sql_type(storage.db.driver());
    storage
        .execute(
            &format!(
                r#"
            CREATE TABLE IF NOT EXISTS sm_sessions (
                stream_id TEXT PRIMARY KEY,
                user_id TEXT NOT NULL,
                full_jid TEXT NOT NULL,
                inbound_count {bigint} NOT NULL,
                outbound_count {bigint} NOT NULL,
                last_acked {bigint} NOT NULL,
                max_resume_secs {bigint},
                detached_at_ms {bigint} NOT NULL,
                max_resume_duration_ms {bigint} NOT NULL,
                carbons_enabled INTEGER NOT NULL,
                roster_interested INTEGER NOT NULL,
                presence_available INTEGER NOT NULL,
                presence_show TEXT,
                presence_status TEXT,
                presence_priority INTEGER NOT NULL,
                replay_gap_through {bigint},
                promotion_attempts INTEGER NOT NULL DEFAULT 0
            )
            "#
            ),
            (),
        )
        .await?;
    storage
        .execute(
            &format!(
                r#"
            CREATE TABLE IF NOT EXISTS sm_unacked (
                stream_id TEXT NOT NULL,
                sequence {bigint} NOT NULL,
                stanza_xml TEXT NOT NULL,
                original_receipt_at_ms {bigint} NOT NULL,
                PRIMARY KEY (stream_id, sequence)
            )
            "#
            ),
            (),
        )
        .await?;
    add_column_if_missing(
        storage,
        "sm_sessions",
        &format!("replay_gap_through {bigint}"),
    )
    .await?;
    widen_existing_postgres_i64_columns(storage).await?;
    // Index on detached_at_ms + max_resume_duration_ms for the
    // janitor's expired-session sweep. We can't compute the
    // expiry timestamp directly in SQL portably, so the janitor
    // filters in Rust over an index-supported scan.
    storage
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_sm_sessions_detached \
         ON sm_sessions (detached_at_ms)",
            (),
        )
        .await?;
    add_column_if_missing(
        storage,
        "sm_sessions",
        "promotion_attempts INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    Ok(())
}

async fn widen_existing_postgres_i64_columns(
    storage: &DatabaseSmPersistence,
) -> Result<(), SmPersistenceError> {
    for (table, column) in [
        ("sm_sessions", "inbound_count"),
        ("sm_sessions", "outbound_count"),
        ("sm_sessions", "last_acked"),
        ("sm_sessions", "replay_gap_through"),
        ("sm_sessions", "max_resume_secs"),
        ("sm_sessions", "detached_at_ms"),
        ("sm_sessions", "max_resume_duration_ms"),
        ("sm_unacked", "sequence"),
        ("sm_unacked", "original_receipt_at_ms"),
    ] {
        crate::db::widen_postgres_i64_column_to_bigint(&storage.db, table, column)
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    }

    Ok(())
}
