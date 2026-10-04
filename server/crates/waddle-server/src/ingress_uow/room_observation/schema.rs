use crate::db::{Database, DatabaseDriver};

use super::ObservationError;

/// Explicit startup initialization. These tables intentionally have no FK to
/// ingress envelope rows: cleanup of an archive source cannot erase pending
/// observation work or a result awaiting durable room publication.
pub async fn initialize_room_observations(db: &Database) -> Result<(), ObservationError> {
    let mut tx = match db.driver() {
        DatabaseDriver::Sqlite => db.begin_immediate().await?,
        DatabaseDriver::Postgres => db.begin().await?,
    };
    if db.driver() == DatabaseDriver::Postgres {
        // Serialize startup DDL across pods without taking a global schema
        // lock shared with unrelated components.
        let mut rows = tx
            .query("SELECT pg_advisory_xact_lock(6316133403883761747)", ())
            .await?;
        rows.next().await?.ok_or(ObservationError::Database)?;
    }
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_observers (
    plugin_id TEXT PRIMARY KEY,
    generation BIGINT NOT NULL,
    identity TEXT NOT NULL,
    scope_json TEXT NOT NULL,
    max_concurrent BIGINT NOT NULL
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_sources (
    source_key TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    sender_jid TEXT NOT NULL,
    root_stanza_id TEXT NOT NULL,
    revision_stanza_id TEXT NOT NULL,
    root_origin_id TEXT,
    revision BIGINT NOT NULL,
    source_json TEXT NOT NULL,
    retracted BIGINT NOT NULL DEFAULT 0,
    captured_at_ms BIGINT NOT NULL DEFAULT 0,
    UNIQUE (room_jid, root_stanza_id)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_source_revisions (
    room_jid TEXT NOT NULL,
    room_stanza_id TEXT NOT NULL,
    source_key TEXT NOT NULL,
    PRIMARY KEY (room_jid, room_stanza_id)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_observation_work (
    id TEXT PRIMARY KEY,
    source_key TEXT NOT NULL,
    message_key TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    identity TEXT NOT NULL,
    room_jid TEXT NOT NULL,
    revision BIGINT NOT NULL,
    source_json TEXT NOT NULL,
    body TEXT NOT NULL,
    status TEXT NOT NULL,
    attempt BIGINT NOT NULL DEFAULT 0,
    due_at_ms BIGINT NOT NULL,
    lease_id TEXT,
    lease_until_ms BIGINT,
    lease_node_id TEXT,
    lease_node_incarnation TEXT,
    terminal_category TEXT,
    usage_json TEXT,
    settled_at_ms BIGINT NULL,
    UNIQUE (plugin_id, generation, room_jid, source_key, revision)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE INDEX IF NOT EXISTS extension_room_observation_work_due
    ON extension_room_observation_work(plugin_id, room_jid, status, due_at_ms)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_observation_receipts (
    plugin_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    room_jid TEXT NOT NULL,
    message_key TEXT NOT NULL,
    category TEXT NOT NULL,
    recorded_at_ms BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (plugin_id, generation, room_jid, message_key)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_publications (
    id TEXT PRIMARY KEY,
    work_id TEXT NOT NULL,
    output_index BIGINT NOT NULL,
    source_key TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    identity TEXT NOT NULL,
    room_jid TEXT NOT NULL,
    revision BIGINT NOT NULL,
    source_json TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL,
    settled_at_ms BIGINT NULL,
    UNIQUE (work_id, output_index)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE INDEX IF NOT EXISTS extension_room_publications_pending
    ON extension_room_publications(plugin_id, room_jid, status)"#,
        (),
    )
    .await?;
    // Retention indexes match V1023; the tables always exist in their
    // migrated shape by the time startup DDL runs.
    for index in [
        "CREATE INDEX IF NOT EXISTS extension_room_observation_work_settled ON extension_room_observation_work (status, settled_at_ms)",
        "CREATE INDEX IF NOT EXISTS extension_room_observation_work_source ON extension_room_observation_work (source_key)",
        "CREATE INDEX IF NOT EXISTS extension_room_publications_settled ON extension_room_publications (status, settled_at_ms)",
        "CREATE INDEX IF NOT EXISTS extension_room_publications_source ON extension_room_publications (source_key)",
        "CREATE INDEX IF NOT EXISTS extension_room_observation_receipts_recorded ON extension_room_observation_receipts (recorded_at_ms)",
        "CREATE INDEX IF NOT EXISTS extension_room_sources_captured ON extension_room_sources (captured_at_ms)",
        "CREATE INDEX IF NOT EXISTS extension_room_source_revisions_source ON extension_room_source_revisions (source_key)",
    ] {
        tx.execute(index, ()).await?;
    }
    tx.commit().await?;
    Ok(())
}
