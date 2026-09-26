//! Dual Postgres/SQLite schema for `extension_job_outbox` (issue #1831 Phase
//! B): the generic, host-owned durable-job queue that replaces
//! `message_judgment_outbox`. Generic across extensions and job kinds —
//! `extension_id` + `job_kind` identify which loaded/granted extension
//! processes a row, so more than one extension's jobs can share this one
//! table without their rows ever being confused (see
//! [`super::store::claim_due_batch`]'s fairness doc for why claiming is
//! partitioned by `extension_id`).
//!
//! Carries `lease_token`/`leased_at_ms` from row one (unlike
//! `message_judgment_outbox`, which only grew a lease in Phase 2): this
//! table's jobs run guest WASM code with no host-side time bound (no
//! wasmtime epoch interruption/fuel limit — see `runtime::loader`), so a
//! lease can go stale mid-job even more easily than a bounded HTTP call
//! ever could. See [`super::store`]'s module docs for the three guardrails
//! this schema exists to support: attempt-on-claim, lease-checked finalize,
//! and fair per-extension claiming.

use super::ExtensionJobOutboxError;
use crate::db::{Database, DatabaseDriver};

/// Dedicated transaction-scoped Postgres advisory lock for
/// `extension_job_outbox` schema bootstrap. Distinct from the clustering
/// claims lock (`…991`), migration-ledger lock (`…992`), lineage lock
/// (`…993`), MUC room-schema lock (`…994`), MUC destroy-completion-outbox
/// lock (`…995`), room-effect-outbox lock (`…996`), and the retired
/// `message_judgment_outbox` lock (`…997`).
const EXTENSION_JOB_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_998;

pub(super) async fn initialize(db: &Database) -> Result<(), ExtensionJobOutboxError> {
    match db.driver() {
        DatabaseDriver::Postgres => postgres(db).await,
        DatabaseDriver::Sqlite => sqlite(db).await,
    }
}

const COLUMN_DEFS_SHARED: &str = "id TEXT PRIMARY KEY, \
     extension_id TEXT NOT NULL, \
     job_kind TEXT NOT NULL, \
     waddle_id TEXT NOT NULL, \
     room TEXT NULL, \
     target_stanza_id TEXT NOT NULL, \
     target_stanza_by TEXT NOT NULL, \
     body_snapshot TEXT NOT NULL, \
     attempt_count BIGINT NOT NULL DEFAULT 0, \
     last_error TEXT NULL, \
     done BOOLEAN NOT NULL DEFAULT FALSE, \
     lease_token TEXT NULL, \
     leased_at_ms BIGINT NULL";

async fn postgres(db: &Database) -> Result<(), ExtensionJobOutboxError> {
    let mut tx = db.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await?;
    tx.query(
        "SELECT pg_advisory_xact_lock(?)",
        crate::db_params![EXTENSION_JOB_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY],
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS extension_job_outbox ( \
                {COLUMN_DEFS_SHARED}, \
                available_at_ms BIGINT NOT NULL, \
                created_at_ms BIGINT NOT NULL \
            )"
        ),
        (),
    )
    .await?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS extension_job_outbox_due_idx \
         ON extension_job_outbox (done, available_at_ms)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS extension_job_outbox_extension_due_idx \
         ON extension_job_outbox (extension_id, done, available_at_ms)",
        (),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn sqlite(db: &Database) -> Result<(), ExtensionJobOutboxError> {
    let connection = db.guard().await?;
    connection
        .execute(
            &format!(
                "CREATE TABLE IF NOT EXISTS extension_job_outbox ( \
                    {COLUMN_DEFS_SHARED}, \
                    available_at_ms INTEGER NOT NULL, \
                    created_at_ms INTEGER NOT NULL \
                )"
            ),
            (),
        )
        .await?;
    connection
        .execute(
            "CREATE INDEX IF NOT EXISTS extension_job_outbox_due_idx \
             ON extension_job_outbox (done, available_at_ms)",
            (),
        )
        .await?;
    connection
        .execute(
            "CREATE INDEX IF NOT EXISTS extension_job_outbox_extension_due_idx \
             ON extension_job_outbox (extension_id, done, available_at_ms)",
            (),
        )
        .await?;
    Ok(())
}
