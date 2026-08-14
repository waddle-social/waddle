use super::RoomEffectOutboxError;
use crate::db::{Database, DatabaseDriver};

/// Dedicated transaction-scoped Postgres advisory lock for room-effect outbox
/// bootstrap. It is distinct from the claims (`…991`), migrations (`…992`),
/// lineage (`…993`), room schema (`…994`), and destroy outbox (`…995`) keys.
const ROOM_EFFECT_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_996;

pub(super) async fn initialize(db: &Database) -> Result<(), RoomEffectOutboxError> {
    match db.driver() {
        DatabaseDriver::Postgres => postgres(db).await,
        DatabaseDriver::Sqlite => sqlite(db).await,
    }
}

async fn postgres(db: &Database) -> Result<(), RoomEffectOutboxError> {
    let mut tx = db.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await?;
    tx.query(
        "SELECT pg_advisory_xact_lock(?)",
        crate::db_params![ROOM_EFFECT_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY],
    )
    .await?;
    tx.execute("CREATE TABLE IF NOT EXISTS clustering_muc_room_effects (lifecycle_id TEXT NOT NULL, revision BIGINT NOT NULL CHECK (revision >= 1), ordinal BIGINT NOT NULL CHECK (ordinal >= 0), room_jid TEXT NOT NULL CHECK (room_jid <> ''), kind TEXT NOT NULL CHECK (kind <> ''), terminal BOOLEAN NOT NULL, payload_json TEXT NOT NULL, available_at_ms BIGINT NOT NULL, superseded BOOLEAN NOT NULL DEFAULT FALSE, origin_instance_id TEXT NOT NULL, producing_node TEXT NOT NULL, lease_token TEXT NULL, leased_at_ms BIGINT NULL, attempt_count BIGINT NOT NULL DEFAULT 0, last_error TEXT NULL, created_at_ms BIGINT NOT NULL, PRIMARY KEY (lifecycle_id, revision, ordinal))", ()).await?;
    tx.execute("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE i.indrelid = 'clustering_muc_room_effects'::regclass AND c.relname = 'clustering_muc_room_effects_due_idx') THEN CREATE INDEX clustering_muc_room_effects_due_idx ON clustering_muc_room_effects (available_at_ms, lifecycle_id); END IF; END $$", ()).await?;
    tx.commit().await?;
    Ok(())
}
async fn sqlite(db: &Database) -> Result<(), RoomEffectOutboxError> {
    let connection = db.guard().await?;
    connection.execute("CREATE TABLE IF NOT EXISTS clustering_muc_room_effects (lifecycle_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK (revision >= 1), ordinal INTEGER NOT NULL CHECK (ordinal >= 0), room_jid TEXT NOT NULL CHECK (room_jid <> ''), kind TEXT NOT NULL CHECK (kind <> ''), terminal BOOLEAN NOT NULL, payload_json TEXT NOT NULL, available_at_ms INTEGER NOT NULL, superseded BOOLEAN NOT NULL DEFAULT FALSE, origin_instance_id TEXT NOT NULL, producing_node TEXT NOT NULL, lease_token TEXT NULL, leased_at_ms INTEGER NULL, attempt_count INTEGER NOT NULL DEFAULT 0, last_error TEXT NULL, created_at_ms INTEGER NOT NULL, PRIMARY KEY (lifecycle_id, revision, ordinal))", ()).await?;
    connection.execute("CREATE INDEX IF NOT EXISTS clustering_muc_room_effects_due_idx ON clustering_muc_room_effects (available_at_ms, lifecycle_id)", ()).await?;
    Ok(())
}
