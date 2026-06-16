//! Idempotent `CREATE TABLE IF NOT EXISTS` for `channel_space_links`,
//! mirroring the `spaces_metadata` schema migration shape.

use super::{ChannelSpaceLinkError, DatabaseChannelSpaceLinkStore};
use crate::space_identity::projected_space_node_from_jid_text;

pub(super) async fn initialize(
    store: &DatabaseChannelSpaceLinkStore,
) -> Result<(), ChannelSpaceLinkError> {
    let i64_type = crate::db::i64_sql_type(store.db.driver());
    let sql = format!(
        r#"
        CREATE TABLE IF NOT EXISTS channel_space_links (
            channel_jid TEXT PRIMARY KEY,
            space_jid TEXT NOT NULL,
            space_node TEXT NOT NULL,
            created_at {i64_type} NOT NULL
        )
        "#
    );
    store.execute(&sql, ()).await?;
    add_column_if_missing(store, "space_node TEXT NOT NULL DEFAULT ''").await?;
    backfill_space_node_from_jid(store).await?;

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
            "CREATE INDEX IF NOT EXISTS idx_channel_space_links_space_node \
             ON channel_space_links (space_node, created_at ASC, channel_jid ASC)",
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

async fn backfill_space_node_from_jid(
    store: &DatabaseChannelSpaceLinkStore,
) -> Result<(), ChannelSpaceLinkError> {
    let mut rows = store
        .query(
            "SELECT channel_jid, space_jid FROM channel_space_links WHERE space_node = ''",
            (),
        )
        .await?;
    let mut updates = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?
    {
        let channel_jid: String = row
            .get(0)
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        let space_jid: String = row
            .get(1)
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))?;
        updates.push((channel_jid, projected_space_node_from_jid_text(&space_jid)));
    }
    drop(rows);

    for (channel_jid, space_node) in updates {
        let space_node = space_node.into_string();
        store
            .execute(
                "UPDATE channel_space_links SET space_node = ? WHERE channel_jid = ?",
                crate::db_params![space_node, channel_jid],
            )
            .await?;
    }
    Ok(())
}

async fn add_column_if_missing(
    store: &DatabaseChannelSpaceLinkStore,
    definition: &'static str,
) -> Result<(), ChannelSpaceLinkError> {
    match store
        .execute(
            &format!("ALTER TABLE channel_space_links ADD COLUMN {definition}"),
            (),
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(ChannelSpaceLinkError::Storage(error))
            if error.contains("duplicate column") || error.contains("already exists") =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}
