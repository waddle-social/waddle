//! Dual Postgres/SQLite schema for the `is_question` community-enrichment
//! judgment outbox (issue #1831 Phase 1).
//!
//! Two tables:
//! - `message_judgment_outbox`: the durable queue of messages awaiting a
//!   judgment call, drained asynchronously by [`super::drain::drain_once`].
//! - `message_judgments`: the append-only results table. Never updated or
//!   deleted; inserts are idempotent under replay via a unique index on
//!   `(stanza_id, judgment_name, model_version)` (see
//!   [`super::store::insert_judgment`]).
//!
//! Neither table carries clustering lease/ownership/fencing columns — this
//! is a single background drain worker, not a clustered actor with lease
//! contention (unlike `room_effect_outbox`, which this module otherwise
//! mirrors the shape of).

use super::MessageJudgmentOutboxError;
use crate::db::{Database, DatabaseDriver};

/// Dedicated transaction-scoped Postgres advisory lock for message-judgment
/// outbox bootstrap. Distinct from the clustering claims lock
/// (`…991`), migration-ledger lock (`…992`), lineage lock (`…993`), MUC
/// room-schema lock (`…994`), MUC destroy-completion-outbox lock (`…995`),
/// and room-effect-outbox lock (`…996`).
const MESSAGE_JUDGMENT_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_997;

pub(super) async fn initialize(db: &Database) -> Result<(), MessageJudgmentOutboxError> {
    match db.driver() {
        DatabaseDriver::Postgres => postgres(db).await,
        DatabaseDriver::Sqlite => sqlite(db).await,
    }
}

async fn postgres(db: &Database) -> Result<(), MessageJudgmentOutboxError> {
    let mut tx = db.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await?;
    tx.query(
        "SELECT pg_advisory_xact_lock(?)",
        crate::db_params![MESSAGE_JUDGMENT_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY],
    )
    .await?;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS message_judgment_outbox ( \
            id TEXT PRIMARY KEY, \
            waddle_id TEXT NOT NULL, \
            stanza_id TEXT NOT NULL, \
            stanza_by TEXT NOT NULL, \
            body_snapshot TEXT NOT NULL, \
            available_at_ms BIGINT NOT NULL, \
            attempt_count BIGINT NOT NULL DEFAULT 0, \
            last_error TEXT, \
            done BOOLEAN NOT NULL DEFAULT FALSE, \
            created_at_ms BIGINT NOT NULL \
        )",
        (),
    )
    .await?;
    tx.execute(
        "CREATE INDEX IF NOT EXISTS message_judgment_outbox_due_idx \
         ON message_judgment_outbox (done, available_at_ms)",
        (),
    )
    .await?;
    tx.execute(
        "CREATE TABLE IF NOT EXISTS message_judgments ( \
            id TEXT PRIMARY KEY, \
            waddle_id TEXT NOT NULL, \
            stanza_id TEXT NOT NULL, \
            stanza_by TEXT NOT NULL, \
            judgment_name TEXT NOT NULL, \
            taxonomy_version TEXT NOT NULL, \
            model_version TEXT NOT NULL, \
            probability DOUBLE PRECISION NOT NULL, \
            cost_usd DOUBLE PRECISION NOT NULL, \
            decided_at_ms BIGINT NOT NULL, \
            created_at_ms BIGINT NOT NULL \
        )",
        (),
    )
    .await?;
    tx.execute(
        "CREATE UNIQUE INDEX IF NOT EXISTS message_judgments_identity_idx \
         ON message_judgments (stanza_id, judgment_name, model_version)",
        (),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn sqlite(db: &Database) -> Result<(), MessageJudgmentOutboxError> {
    let connection = db.guard().await?;
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS message_judgment_outbox ( \
                id TEXT PRIMARY KEY, \
                waddle_id TEXT NOT NULL, \
                stanza_id TEXT NOT NULL, \
                stanza_by TEXT NOT NULL, \
                body_snapshot TEXT NOT NULL, \
                available_at_ms INTEGER NOT NULL, \
                attempt_count INTEGER NOT NULL DEFAULT 0, \
                last_error TEXT, \
                done BOOLEAN NOT NULL DEFAULT FALSE, \
                created_at_ms INTEGER NOT NULL \
            )",
            (),
        )
        .await?;
    connection
        .execute(
            "CREATE INDEX IF NOT EXISTS message_judgment_outbox_due_idx \
             ON message_judgment_outbox (done, available_at_ms)",
            (),
        )
        .await?;
    connection
        .execute(
            "CREATE TABLE IF NOT EXISTS message_judgments ( \
                id TEXT PRIMARY KEY, \
                waddle_id TEXT NOT NULL, \
                stanza_id TEXT NOT NULL, \
                stanza_by TEXT NOT NULL, \
                judgment_name TEXT NOT NULL, \
                taxonomy_version TEXT NOT NULL, \
                model_version TEXT NOT NULL, \
                probability REAL NOT NULL, \
                cost_usd REAL NOT NULL, \
                decided_at_ms INTEGER NOT NULL, \
                created_at_ms INTEGER NOT NULL \
            )",
            (),
        )
        .await?;
    connection
        .execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS message_judgments_identity_idx \
             ON message_judgments (stanza_id, judgment_name, model_version)",
            (),
        )
        .await?;
    Ok(())
}
