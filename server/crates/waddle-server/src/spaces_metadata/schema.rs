//! Idempotent `CREATE TABLE IF NOT EXISTS` for `spaces_metadata`,
//! mirroring the inbox schema migration shape.

use super::{DatabaseSpacesMetadataStore, SpacesMetadataError};

pub(super) async fn initialize(
    store: &DatabaseSpacesMetadataStore,
) -> Result<(), SpacesMetadataError> {
    let i64_type = crate::db::i64_sql_type(store.db.driver());
    let sql = format!(
        r#"
        CREATE TABLE IF NOT EXISTS spaces_metadata (
            space_jid TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            description TEXT,
            icon_url TEXT,
            created_at {i64_type} NOT NULL,
            updated_at {i64_type} NOT NULL
        )
        "#
    );
    store.execute(&sql, ()).await?;

    crate::db::widen_postgres_i64_column_to_bigint(&store.db, "spaces_metadata", "created_at")
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    crate::db::widen_postgres_i64_column_to_bigint(&store.db, "spaces_metadata", "updated_at")
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;

    store
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_spaces_metadata_created_at \
             ON spaces_metadata (created_at ASC, space_jid ASC)",
            (),
        )
        .await?;
    Ok(())
}
