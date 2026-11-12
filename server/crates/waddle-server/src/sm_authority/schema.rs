//! Owner-side DDL for the durable session authority relations.

use crate::db::{Database, DatabaseError};

/// Create the authority relations if they are absent. This runs in the owner
/// phase, or when a store opens an unrestricted database; the restricted
/// runtime role verifies the result and never executes it.
pub async fn bootstrap_schema_owner(db: &Database) -> Result<(), DatabaseError> {
    let bigint = crate::db::i64_sql_type(db.driver());
    let statements = [
        format!(
            "CREATE TABLE IF NOT EXISTS sm_durable_sessions (
                session_id TEXT PRIMARY KEY,
                resume_token_sha256 TEXT NOT NULL,
                full_jid TEXT NOT NULL,
                state INTEGER NOT NULL CHECK (state IN (1, 2, 3, 4)),
                terminal_reason INTEGER,
                generation {bigint} NOT NULL,
                binding TEXT NOT NULL,
                bare_jid TEXT NOT NULL,
                auth_context_id TEXT NOT NULL,
                auth_context_version {bigint} NOT NULL,
                principal_auth_epoch {bigint} NOT NULL,
                owner_node_id TEXT,
                owner_node_epoch TEXT,
                claim_epoch {bigint},
                inbound_handled {bigint} NOT NULL,
                outbound_acked {bigint} NOT NULL,
                outbound_exposure_ceiling {bigint} NOT NULL,
                outbound_appended {bigint} NOT NULL,
                outbound_unacked_bytes {bigint} NOT NULL,
                max_resume_secs {bigint} NOT NULL,
                detached_at_ms {bigint},
                enable_operation TEXT NOT NULL,
                last_operation TEXT NOT NULL,
                updated_at_ms {bigint} NOT NULL
            )"
        ),
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_sm_durable_sessions_token \
         ON sm_durable_sessions (resume_token_sha256)"
            .to_owned(),
        "CREATE INDEX IF NOT EXISTS idx_sm_durable_sessions_detached \
         ON sm_durable_sessions (state, detached_at_ms)"
            .to_owned(),
        "CREATE INDEX IF NOT EXISTS idx_sm_durable_sessions_resource \
         ON sm_durable_sessions (full_jid)"
            .to_owned(),
        format!(
            "CREATE TABLE IF NOT EXISTS sm_durable_operations (
                operation_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                kind INTEGER NOT NULL CHECK (kind IN (1, 2, 3)),
                outcome INTEGER NOT NULL CHECK (outcome IN (1, 2)),
                generation {bigint} NOT NULL,
                ordinal {bigint},
                recorded_at_ms {bigint} NOT NULL
            )"
        ),
        "CREATE INDEX IF NOT EXISTS idx_sm_durable_operations_session \
         ON sm_durable_operations (session_id, kind, ordinal)"
            .to_owned(),
    ];
    let connection = db.guard().await?;
    for statement in statements {
        connection.execute(&statement, ()).await?;
    }
    Ok(())
}
