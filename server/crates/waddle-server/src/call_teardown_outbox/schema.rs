use super::CallTeardownOutboxError;
use crate::db::ConnectionGuard;
use crate::db::Database;
use crate::db::DatabaseDriver;

pub(super) async fn initialize(db: &Database) -> Result<(), CallTeardownOutboxError> {
    let timestamp_type = crate::db::i64_sql_type(db.driver());
    let driver = db.driver();
    let connection = db.guard().await?;
    connection
        .execute(&table_sql("call_teardown_outbox", timestamp_type), ())
        .await?;
    add_non_blank_text_column_if_missing(&connection, driver, "producing_node").await?;
    add_non_blank_text_column_if_missing(&connection, driver, "session_binding").await?;
    add_non_blank_text_column_if_missing(&connection, driver, "occupant").await?;
    add_non_blank_text_column_if_missing(&connection, driver, "unbound_occupant").await?;
    drop(connection);
    upgrade_retirement_action(db).await?;
    let connection = db.guard().await?;
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
    connection: &ConnectionGuard,
    driver: DatabaseDriver,
    column: &'static str,
) -> Result<(), CallTeardownOutboxError> {
    match driver {
        DatabaseDriver::Postgres => {
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
        DatabaseDriver::Sqlite => {
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
    connection: &ConnectionGuard,
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

const ACTION_CHECK: &str = "action IN ('remove_participant','retire_displaced_participant','delete_room','muji_presence_clear','muji_room_sweep','call_thread_end_retry')";
const TARGET_CHECK: &str = "\
    (action = 'remove_participant' AND identity IS NOT NULL AND room_jid IS NULL) \
    OR (action = 'retire_displaced_participant' AND identity IS NOT NULL AND room_jid IS NULL AND (participant_sid IS NULL OR participant_sid <> '') AND (participant_sid IS NOT NULL OR occupant IS NOT NULL)) \
    OR (action = 'delete_room' AND identity IS NULL AND room_jid IS NULL AND participant_sid IS NULL) \
    OR (action = 'muji_presence_clear' AND identity IS NOT NULL AND room_jid IS NOT NULL) \
    OR (action = 'muji_room_sweep' AND identity IS NULL AND room_jid IS NOT NULL AND room_sid IS NOT NULL AND participant_sid IS NULL) \
    OR (action = 'call_thread_end_retry' AND identity IS NULL AND room_jid IS NOT NULL AND participant_sid IS NULL AND thread_id IS NOT NULL AND anchor_origin_id IS NOT NULL AND thread_started_at_ms IS NOT NULL AND thread_ended_at_ms IS NOT NULL)";

// Both arguments are compile-time identifiers/types supplied below, never input.
fn table_sql(table: &'static str, timestamp_type: &str) -> String {
    format!(
        "CREATE TABLE IF NOT EXISTS {table} (\
                    intent_id TEXT PRIMARY KEY, \
                    call_id TEXT NOT NULL, \
                    identity TEXT NULL CHECK (identity IS NULL OR identity <> ''), \
                    room_jid TEXT NULL CHECK (room_jid IS NULL OR room_jid <> ''), \
                    action TEXT NOT NULL CHECK ({ACTION_CHECK}), \
                    generation INTEGER NULL CHECK (generation IS NULL OR generation > 0), \
                    occupant TEXT NULL CHECK (occupant IS NULL OR occupant <> ''), \
                    unbound_occupant TEXT NULL CHECK (unbound_occupant IS NULL OR unbound_occupant <> ''), \
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
                    CHECK ({TARGET_CHECK}), \
                    CHECK ((thread_id IS NULL AND anchor_origin_id IS NULL AND thread_started_at_ms IS NULL AND thread_ended_at_ms IS NULL) OR action = 'call_thread_end_retry')\
                )"
    )
}

/// Expand the action set atomically. This outbox predates the migration ledger;
/// like its existing column upgrades, its schema is initialized by the store.
async fn upgrade_retirement_action(db: &Database) -> Result<(), CallTeardownOutboxError> {
    let mut transaction = db.begin_immediate().await?;
    match db.driver() {
        DatabaseDriver::Sqlite => {
            let mut rows = transaction.query(
                "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'call_teardown_outbox'",
                (),
            ).await?;
            let Some(row) = rows.next().await? else {
                return Err(crate::db::DatabaseError::Internal(sqlx::Error::RowNotFound).into());
            };
            let current: String = row.get(0)?;
            drop(rows);
            if !current.contains("participant_sid IS NOT NULL OR occupant IS NOT NULL") {
                let mut rows = transaction
                    .query(
                        "SELECT sql FROM sqlite_master WHERE tbl_name = 'call_teardown_outbox' \
                     AND type IN ('index', 'trigger') AND sql IS NOT NULL",
                        (),
                    )
                    .await?;
                let mut dependent_schema = Vec::new();
                while let Some(row) = rows.next().await? {
                    dependent_schema.push(row.get::<String>(0)?);
                }
                drop(rows);
                transaction
                    .execute(&table_sql("call_teardown_outbox_retirement", "INTEGER"), ())
                    .await?;
                transaction.execute(
                    "INSERT INTO call_teardown_outbox_retirement (\
                        intent_id, call_id, identity, room_jid, action, generation, occupant, \
                        room_sid, participant_sid, session_binding, thread_id, anchor_origin_id, \
                        thread_started_at_ms, thread_ended_at_ms, producing_node, status, \
                        attempt_count, last_error, next_attempt_at_ms, claimed_at_ms, claim_token, \
                        created_at_ms, updated_at_ms, unbound_occupant\
                     ) SELECT intent_id, call_id, identity, room_jid, action, generation, occupant, \
                        room_sid, participant_sid, session_binding, thread_id, anchor_origin_id, \
                        thread_started_at_ms, thread_ended_at_ms, producing_node, status, \
                        attempt_count, last_error, next_attempt_at_ms, claimed_at_ms, claim_token, \
                        created_at_ms, updated_at_ms, unbound_occupant \
                     FROM call_teardown_outbox",
                    (),
                ).await?;
                transaction
                    .execute("DROP TABLE call_teardown_outbox", ())
                    .await?;
                transaction.execute("ALTER TABLE call_teardown_outbox_retirement RENAME TO call_teardown_outbox", ()).await?;
                for statement in dependent_schema {
                    transaction.execute(&statement, ()).await?;
                }
            }
        }
        DatabaseDriver::Postgres => {
            // Serialize startup upgrades before inspecting/dropping constraints.
            transaction
                .execute(
                    "LOCK TABLE call_teardown_outbox IN ACCESS EXCLUSIVE MODE",
                    (),
                )
                .await?;
            let mut rows = transaction
                .query(
                    "SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint \
                 WHERE conrelid = 'call_teardown_outbox'::regclass AND contype = 'c'",
                    (),
                )
                .await?;
            let mut constraints = Vec::new();
            let mut current_constraints = 0;
            while let Some(row) = rows.next().await? {
                let name: String = row.get(0)?;
                let definition: String = row.get(1)?;
                if definition.contains("'remove_participant'") {
                    if definition.contains("'retire_displaced_participant'")
                        && (!definition.contains("identity")
                            || definition.contains("occupant IS NOT NULL"))
                    {
                        current_constraints += 1;
                    }
                    constraints.push(name);
                }
            }
            drop(rows);
            if current_constraints != 2 || constraints.len() != 2 {
                for name in constraints {
                    let quoted = name.replace('"', "\"\"");
                    transaction
                        .execute(
                            &format!(
                                "ALTER TABLE call_teardown_outbox DROP CONSTRAINT \"{quoted}\""
                            ),
                            (),
                        )
                        .await?;
                }
                transaction
                    .execute(
                        &format!(
                            "ALTER TABLE call_teardown_outbox \
                     ADD CONSTRAINT call_teardown_outbox_action_check CHECK ({ACTION_CHECK}), \
                     ADD CONSTRAINT call_teardown_outbox_target_check CHECK ({TARGET_CHECK})"
                        ),
                        (),
                    )
                    .await?;
            }
        }
    }
    transaction.commit().await?;
    Ok(())
}
