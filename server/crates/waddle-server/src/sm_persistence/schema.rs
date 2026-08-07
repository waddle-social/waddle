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
                blocklist_interested INTEGER NOT NULL DEFAULT 0,
                presence_available INTEGER NOT NULL,
                presence_show TEXT,
                presence_status TEXT,
                presence_priority INTEGER NOT NULL,
                replay_gap_through {bigint},
                promotion_attempts INTEGER NOT NULL DEFAULT 0,
                presence_payloads TEXT,
                bare_jid TEXT,
                auth_context_id TEXT,
                auth_context_version {bigint},
                principal_auth_epoch {bigint}
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
    add_column_if_missing(
        storage,
        "sm_sessions",
        "blocklist_interested INTEGER NOT NULL DEFAULT 0",
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
    // #1206: durable storage of the resource's own presence extension
    // payloads (XEP-0115 caps, XEP-0319 idle, ...) so a rehydrated session
    // relays them verbatim on probe instead of coming back caps-less.
    add_column_if_missing(storage, "sm_sessions", "presence_payloads TEXT").await?;
    add_column_if_missing(storage, "sm_sessions", "bare_jid TEXT").await?;
    add_column_if_missing(storage, "sm_sessions", "auth_context_id TEXT").await?;
    add_column_if_missing(
        storage,
        "sm_sessions",
        &format!("auth_context_version {bigint}"),
    )
    .await?;
    add_column_if_missing(
        storage,
        "sm_sessions",
        &format!("principal_auth_epoch {bigint}"),
    )
    .await?;
    // ADR-0017 Phase 3 Slice 5 / element 5 — schema-only groundwork,
    // mirroring `pending_delivery`'s identical column group (see that
    // migration's doc comment for the full rationale and the "deferred,
    // recorded as a deviation" note: nothing populates these yet).
    // `sm_unacked` already carries `original_receipt_at_ms`, so only these
    // three are net-new here. `recipient` for this table is the owning
    // stream itself (`sm_unacked.stream_id`, already part of the primary
    // key), so the dedup key collapses to `(stream_id, origin_stream_id,
    // inbound_seq)`.
    add_column_if_missing(storage, "sm_unacked", "origin_stream_id TEXT").await?;
    add_column_if_missing(storage, "sm_unacked", &format!("inbound_seq {bigint}")).await?;
    add_column_if_missing(storage, "sm_unacked", &format!("pair_sequence {bigint}")).await?;
    storage
        .execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_sm_unacked_dedup \
         ON sm_unacked (stream_id, origin_stream_id, inbound_seq) \
         WHERE origin_stream_id IS NOT NULL AND inbound_seq IS NOT NULL",
            (),
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
        ("sm_sessions", "auth_context_version"),
        ("sm_sessions", "principal_auth_epoch"),
        ("sm_unacked", "sequence"),
        ("sm_unacked", "original_receipt_at_ms"),
    ] {
        crate::db::widen_postgres_i64_column_to_bigint(&storage.db, table, column)
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    }

    Ok(())
}
