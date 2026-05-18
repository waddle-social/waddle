//! Idempotent `CREATE TABLE IF NOT EXISTS` for `channel_space_links`,
//! mirroring the `spaces_metadata` schema migration shape.

use super::{ChannelSpaceLinkError, DatabaseChannelSpaceLinkStore};

pub(super) async fn initialize(
    store: &DatabaseChannelSpaceLinkStore,
) -> Result<(), ChannelSpaceLinkError> {
    let i64_type = crate::db::i64_sql_type(store.db.driver());
    let sql = format!(
        r#"
        CREATE TABLE IF NOT EXISTS channel_space_links (
            channel_jid TEXT PRIMARY KEY,
            space_jid TEXT NOT NULL,
            created_at {i64_type} NOT NULL
        )
        "#
    );
    store.execute(&sql, ()).await?;

    crate::db::widen_postgres_i64_column_to_bigint(&store.db, "channel_space_links", "created_at")
        .await
        .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;

    store
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_channel_space_links_space \
             ON channel_space_links (space_jid, created_at ASC, channel_jid ASC)",
            (),
        )
        .await?;
    store
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_channel_space_links_created_at \
             ON channel_space_links (created_at ASC, channel_jid ASC)",
            (),
        )
        .await?;
    Ok(())
}
