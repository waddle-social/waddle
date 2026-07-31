use super::CallTeardownOutboxError;
use crate::db::Database;

pub(super) async fn initialize(db: &Database) -> Result<(), CallTeardownOutboxError> {
    let timestamp_type = crate::db::i64_sql_type(db.driver());
    let connection = db.guard().await?;
    connection
        .execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS call_teardown_outbox (\
                    intent_id TEXT PRIMARY KEY, \
                    call_id TEXT NOT NULL, \
                    identity TEXT NULL CHECK (identity IS NULL OR identity <> ''), \
                    room_jid TEXT NULL CHECK (room_jid IS NULL OR room_jid <> ''), \
                    action TEXT NOT NULL CHECK (action IN ('remove_participant','delete_room','muji_presence_clear','muji_room_sweep')), \
                    generation INTEGER NULL CHECK (generation IS NULL OR generation > 0), \
                    room_sid TEXT NULL, \
                    participant_sid TEXT NULL, \
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
                        OR (action = 'muji_room_sweep' AND identity IS NULL AND room_jid IS NOT NULL AND room_sid IS NOT NULL AND participant_sid IS NULL)\
                    )\
                )"
            ),
            (),
        )
        .await?;
    connection
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_call_teardown_outbox_status_due \
             ON call_teardown_outbox (status, next_attempt_at_ms, created_at_ms)",
            (),
        )
        .await?;
    Ok(())
}
