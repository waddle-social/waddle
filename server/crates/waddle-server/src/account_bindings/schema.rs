//! Owner-side DDL for the account resource relations.

use crate::db::{Database, DatabaseError};
use crate::sm_authority::bootstrap_durable_sm_owner_schema;

/// Create the account resource relations if they are absent. This runs in
/// the owner phase, or when a store opens an unrestricted database; the
/// restricted runtime role verifies the result and never executes it.
///
/// `sm_session_id` references the durable stream-management session a
/// binding belongs to. The reference is optional and its check is deferred
/// to commit, because the session row and the resource row are written in
/// one transaction in either order. The session relation is created first so
/// the reference has a target in a database that has not opened the durable
/// session store.
pub async fn bootstrap_schema_owner(db: &Database) -> Result<(), DatabaseError> {
    bootstrap_durable_sm_owner_schema(db).await?;
    let bigint = crate::db::i64_sql_type(db.driver());
    let statements = [
        format!(
            "CREATE TABLE IF NOT EXISTS account_resource_bindings (
                full_jid TEXT PRIMARY KEY,
                bare_jid TEXT NOT NULL,
                claim_identity TEXT NOT NULL,
                revision {bigint} NOT NULL,
                install_generation {bigint} NOT NULL,
                operation_seq {bigint} NOT NULL,
                state INTEGER NOT NULL CHECK (state IN (0, 1, 2, 3, 4, 5)),
                binding_ref TEXT,
                binding_claim_identity TEXT,
                logical_generation TEXT,
                occupancy_generation TEXT,
                endpoint_published INTEGER NOT NULL,
                actor_registered INTEGER NOT NULL,
                presence_available INTEGER NOT NULL,
                presence_show INTEGER,
                presence_status TEXT,
                presence_priority INTEGER NOT NULL,
                carbons_enabled INTEGER NOT NULL,
                roster_interested INTEGER NOT NULL,
                blocklist_interested INTEGER NOT NULL,
                sm_session_id TEXT,
                sm_generation {bigint},
                snapshot_revision {bigint} NOT NULL,
                detach_custody INTEGER,
                retiring_from INTEGER,
                retirement_source_revision {bigint},
                retirement_custody INTEGER,
                offline_flushed INTEGER NOT NULL,
                subscriptions_flushed INTEGER NOT NULL,
                updated_at_ms {bigint} NOT NULL,
                CONSTRAINT account_resource_bindings_sm_session_fk
                    FOREIGN KEY (sm_session_id)
                    REFERENCES sm_durable_sessions (session_id)
                    DEFERRABLE INITIALLY DEFERRED
            )"
        ),
        "CREATE INDEX IF NOT EXISTS idx_account_resource_bindings_account \
         ON account_resource_bindings (bare_jid)"
            .to_owned(),
        "CREATE INDEX IF NOT EXISTS idx_account_resource_bindings_sm_session \
         ON account_resource_bindings (sm_session_id)"
            .to_owned(),
        format!(
            "CREATE TABLE IF NOT EXISTS account_resource_operations (
                operation_id TEXT PRIMARY KEY,
                full_jid TEXT NOT NULL,
                kind INTEGER NOT NULL,
                binding_ref TEXT NOT NULL,
                expected_revision {bigint} NOT NULL,
                committed_revision {bigint} NOT NULL,
                operation_seq {bigint} NOT NULL,
                recorded_at_ms {bigint} NOT NULL
            )"
        ),
        "CREATE INDEX IF NOT EXISTS idx_account_resource_operations_resource \
         ON account_resource_operations (full_jid, operation_seq)"
            .to_owned(),
    ];
    let connection = db.guard().await?;
    for statement in statements {
        connection.execute(&statement, ()).await?;
    }
    Ok(())
}
