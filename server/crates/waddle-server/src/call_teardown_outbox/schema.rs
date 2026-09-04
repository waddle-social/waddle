use super::CallTeardownOutboxError;
use crate::db::Database;

pub(super) async fn initialize(db: &Database) -> Result<(), CallTeardownOutboxError> {
    let timestamp_type = crate::db::i64_sql_type(db.driver());
    let driver = db.driver();
    let connection = db.guard().await?;
    connection
        .execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS call_teardown_outbox (\
                    intent_id TEXT PRIMARY KEY, \
                    call_id TEXT NOT NULL, \
                    identity TEXT NULL CHECK (identity IS NULL OR identity <> ''), \
                    room_jid TEXT NULL CHECK (room_jid IS NULL OR room_jid <> ''), \
                    action TEXT NOT NULL CHECK (action IN ('remove_participant','delete_room','muji_presence_clear','muji_room_sweep','call_thread_end_retry')), \
                    generation INTEGER NULL CHECK (generation IS NULL OR generation > 0), \
                    occupant TEXT NULL CHECK (occupant IS NULL OR occupant <> ''), \
                    room_sid TEXT NULL, \
                    participant_sid TEXT NULL, \
                    session_binding TEXT NULL CHECK (session_binding IS NULL OR session_binding <> ''), \
                    thread_id TEXT NULL CHECK (thread_id IS NULL OR thread_id <> ''), \
                    anchor_origin_id TEXT NULL CHECK (anchor_origin_id IS NULL OR anchor_origin_id <> ''), \
                    thread_started_at_ms {timestamp_type} NULL, \
                    thread_ended_at_ms {timestamp_type} NULL, \
                    producing_node TEXT NULL CHECK (producing_node IS NULL OR producing_node <> ''), \
                    status TEXT NOT NULL CHECK (status IN ('queued','in-progress','done','failed')), \
                    attempt_count INTEGER NOT NULL DEFAULT 0, \
                    last_error TEXT NULL, \
                    next_attempt_at_ms {timestamp_type} NULL, \
                    claimed_at_ms {timestamp_type} NULL, \
                    claim_token TEXT NULL, \
                    created_at_ms {timestamp_type} NOT NULL, \
                    updated_at_ms {timestamp_type} NOT NULL, \
                    CHECK (\
                        (action = 'remove_participant' AND identity IS NOT NULL AND room_jid IS NULL) \
                        OR (action = 'delete_room' AND identity IS NULL AND room_jid IS NULL AND participant_sid IS NULL) \
                        OR (action = 'muji_presence_clear' AND identity IS NOT NULL AND room_jid IS NOT NULL) \
                        OR (action = 'muji_room_sweep' AND identity IS NULL AND room_jid IS NOT NULL AND room_sid IS NOT NULL AND participant_sid IS NULL) \
                        OR (action = 'call_thread_end_retry' AND identity IS NULL AND room_jid IS NOT NULL AND participant_sid IS NULL AND thread_id IS NOT NULL AND anchor_origin_id IS NOT NULL AND thread_started_at_ms IS NOT NULL AND thread_ended_at_ms IS NOT NULL)\
                    ), \
                    CHECK ((thread_id IS NULL AND anchor_origin_id IS NULL AND thread_started_at_ms IS NULL AND thread_ended_at_ms IS NULL) OR action = 'call_thread_end_retry')\
                )"
            ),
            (),
        )
        .await?;
    add_non_blank_text_column_if_missing(&connection, driver, "producing_node").await?;
    add_non_blank_text_column_if_missing(&connection, driver, "session_binding").await?;
    add_non_blank_text_column_if_missing(&connection, driver, "occupant").await?;
    connection
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_call_teardown_outbox_status_due \
             ON call_teardown_outbox (status, next_attempt_at_ms, created_at_ms)",
            (),
        )
        .await?;
    // The enqueue-path dedupe SELECT filters on (status, call_id,
    // action, …); without its own index it degrades to scanning the
    // queued backlog on the websocket/webhook hot path (#1612 review
    // round 12).
    connection
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_call_teardown_outbox_dedupe \
             ON call_teardown_outbox (status, call_id, action)",
            (),
        )
        .await?;
    Ok(())
}

/// Adds a `TEXT NULL CHECK (<column> IS NULL OR <column> <> '')` column
/// to `call_teardown_outbox` when it is missing. Postgres delegates to
/// `ADD COLUMN IF NOT EXISTS`; SQLite has no such clause, so the column
/// is probed via `PRAGMA table_info` first. Either way every DDL
/// failure surfaces as the typed `DatabaseError` — no substring
/// matching on `error.to_string()`.
///
/// `column` MUST be a compile-time identifier constant: DDL cannot bind
/// parameters, so it is interpolated into the statement.
async fn add_non_blank_text_column_if_missing(
    connection: &crate::db::ConnectionGuard,
    driver: crate::db::DatabaseDriver,
    column: &'static str,
) -> Result<(), CallTeardownOutboxError> {
    match driver {
        crate::db::DatabaseDriver::Postgres => {
            connection
                .execute(
                    &format!(
                        "ALTER TABLE call_teardown_outbox ADD COLUMN IF NOT EXISTS \
                         {column} TEXT NULL CHECK ({column} IS NULL OR {column} <> '')"
                    ),
                    (),
                )
                .await?;
        }
        crate::db::DatabaseDriver::Sqlite => {
            if sqlite_column_present(connection, column).await? {
                return Ok(());
            }
            connection
                .execute(
                    &format!(
                        "ALTER TABLE call_teardown_outbox ADD COLUMN \
                         {column} TEXT NULL CHECK ({column} IS NULL OR {column} <> '')"
                    ),
                    (),
                )
                .await?;
        }
    }
    Ok(())
}

/// SQLite-only: returns `true` when `column` already exists on
/// `call_teardown_outbox`. `PRAGMA table_info` reports the column name
/// at index 1.
async fn sqlite_column_present(
    connection: &crate::db::ConnectionGuard,
    column: &str,
) -> Result<bool, CallTeardownOutboxError> {
    let mut rows = connection
        .query("PRAGMA table_info(call_teardown_outbox)", ())
        .await?;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}
