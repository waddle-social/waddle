use crate::db::{Database, DatabaseDriver};

use super::types::{storage, RevisionError};

/// Install the unselected authority schema once using an owner connection. The
/// retained generation and receipt rows intentionally outlive `sm_sessions`.
/// Existing tables cause an error; this installer does not certify their shape.
/// Selecting this schema also requires a catalog/owner/privilege qualification
/// on the serving pool, which is not supplied by this unselected module.
pub async fn bootstrap_revisioned_sm_owner_schema(db: &Database) -> Result<(), RevisionError> {
    if db.driver() == DatabaseDriver::Postgres && db.uses_attested_postgres_pool() {
        return Err(RevisionError::Unsupported);
    }
    let binary = match db.driver() {
        DatabaseDriver::Postgres => "BYTEA",
        DatabaseDriver::Sqlite => "BLOB",
    };
    let mut tx = db.begin_immediate().await.map_err(storage)?;
    tx.execute_batch(&format!(
        "CREATE TABLE sm_revision_generations (
            generation TEXT PRIMARY KEY,
            stream_id TEXT NOT NULL,
            occupancy_session TEXT NOT NULL,
            UNIQUE (stream_id, generation),
            UNIQUE (stream_id, occupancy_session)
        );
        CREATE TABLE sm_revision_authority (
            stream_id TEXT PRIMARY KEY,
            generation TEXT NOT NULL,
            occupancy_session TEXT NOT NULL,
            full_jid TEXT NOT NULL,
            revision BIGINT NOT NULL CHECK (revision >= 0),
            terminal INTEGER NOT NULL CHECK (terminal IN (0, 1)),
            FOREIGN KEY (stream_id, generation)
                REFERENCES sm_revision_generations (stream_id, generation)
        );
        CREATE TABLE sm_revision_operations (
            operation_id TEXT PRIMARY KEY,
            stream_id TEXT NOT NULL,
            generation TEXT NOT NULL,
            input_digest {binary} NOT NULL,
            result TEXT NOT NULL,
            FOREIGN KEY (stream_id, generation)
                REFERENCES sm_revision_generations (stream_id, generation)
        );"
    ))
    .await
    .map_err(storage)?;
    tx.commit().await.map_err(storage)
}
