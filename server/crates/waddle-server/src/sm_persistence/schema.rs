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

async fn ensure_unacked_purpose(storage: &DatabaseSmPersistence) -> Result<(), SmPersistenceError> {
    let application = unacked_purpose_wire_str(SmUnackedStanzaPurpose::Application);
    match storage.db.driver() {
        DatabaseDriver::Sqlite => {
            // SQLite cannot add a non-null column to a populated table unless
            // the ADD supplies a non-null default, and it cannot subsequently
            // promote a nullable column to NOT NULL without rebuilding the
            // table. Do both the backfill and constraint atomically.
            add_column_if_missing(
                storage,
                "sm_unacked",
                &format!("purpose TEXT NOT NULL DEFAULT '{application}'"),
            )
            .await
        }
        DatabaseDriver::Postgres => {
            ensure_postgres_unacked_purpose(&storage.db).await?;
            Ok(())
        }
    }
}

/// Add/backfill/enforce the PostgreSQL replay-purpose column when needed.
///
/// Returns `true` when migration DDL ran and `false` when the column was
/// already `NOT NULL`. The metadata gate is operationally important:
/// PostgreSQL takes an `ACCESS EXCLUSIVE` table lock even for a redundant
/// `ALTER COLUMN ... SET NOT NULL`, so fully migrated replicas must take a
/// metadata-only path without table DDL.
pub(crate) async fn ensure_postgres_unacked_purpose(
    db: &crate::db::Database,
) -> Result<bool, SmPersistenceError> {
    let mut transaction = db
        .begin()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    // Serialize first-upgrade replicas, then re-read metadata while holding
    // the transaction-scoped lock. The two signed int4 keys encode
    // 0x77616464 ("wadd") / 0x736d7072 ("smpr") and are reserved for this
    // schema transition. A
    // transaction-scoped advisory lock is cancellation-safe: rollback on
    // drop releases it instead of returning a session lock to the pool.
    transaction
        .execute("SELECT pg_advisory_xact_lock(2002871396, 1936552050)", ())
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let mut rows = transaction
        .query(
            "SELECT is_nullable \
             FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = 'sm_unacked' \
               AND column_name = 'purpose'",
            (),
        )
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    let nullability: Option<String> = match rows
        .next()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?
    {
        Some(row) => Some(
            row.get(0)
                .map_err(|error| SmPersistenceError::Other(error.to_string()))?,
        ),
        None => None,
    };

    let needs_migration = match nullability.as_deref() {
        Some(value) if value.eq_ignore_ascii_case("NO") => false,
        Some(value) if value.eq_ignore_ascii_case("YES") => true,
        None => {
            transaction
                .execute(
                    "ALTER TABLE sm_unacked ADD COLUMN IF NOT EXISTS purpose TEXT",
                    (),
                )
                .await
                .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
            true
        }
        Some(value) => {
            return Err(SmPersistenceError::Other(format!(
                "unexpected sm_unacked.purpose nullability metadata: {value}"
            )));
        }
    };

    if needs_migration {
        // Repair both the legacy missing-column shape and a partially-applied
        // nullable migration. Deliberately leave no permanent default: every
        // current writer supplies its typed purpose explicitly.
        transaction
            .execute(
                "UPDATE sm_unacked SET purpose = ? WHERE purpose IS NULL",
                crate::db_params![
                    unacked_purpose_wire_str(SmUnackedStanzaPurpose::Application).to_string()
                ],
            )
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
        transaction
            .execute(
                "ALTER TABLE sm_unacked ALTER COLUMN purpose SET NOT NULL",
                (),
            )
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    }
    transaction
        .commit()
        .await
        .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    Ok(needs_migration)
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
                presence_payloads TEXT
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
                purpose TEXT NOT NULL,
                PRIMARY KEY (stream_id, sequence)
            )
            "#
            ),
            (),
        )
        .await?;
    // Replay purpose was added after `sm_unacked` first shipped. A bare
    // `CREATE TABLE IF NOT EXISTS` does not alter those existing tables.
    ensure_unacked_purpose(storage).await?;
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
        ("sm_unacked", "sequence"),
        ("sm_unacked", "original_receipt_at_ms"),
    ] {
        crate::db::widen_postgres_i64_column_to_bigint(&storage.db, table, column)
            .await
            .map_err(|error| SmPersistenceError::Other(error.to_string()))?;
    }

    Ok(())
}
