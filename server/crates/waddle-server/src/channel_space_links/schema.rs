//! Idempotent `CREATE TABLE IF NOT EXISTS` for `channel_space_links`,
//! mirroring the `spaces_metadata` schema migration shape.

use super::{ChannelSpaceLinkError, DatabaseChannelSpaceLinkStore};
use crate::db::{DatabaseError, Transaction};
use crate::space_identity::projected_space_node_from_jid_text;

pub(super) async fn initialize(
    store: &DatabaseChannelSpaceLinkStore,
) -> Result<(), ChannelSpaceLinkError> {
    match store.db.driver() {
        crate::db::DatabaseDriver::Sqlite => initialize_sqlite(store).await,
        crate::db::DatabaseDriver::Postgres => {
            crate::spaces_metadata::runtime_attach::initialize_raw_space_store(
                &store.db,
                crate::spaces_metadata::runtime_attach::SpaceStoreKind::ChannelSpaceLinks,
            )
            .await
            .map_err(|error| ChannelSpaceLinkError::Storage(error.to_string()))
        }
    }
}

async fn initialize_sqlite(
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

pub(crate) async fn initialize_postgres_in_transaction(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), DatabaseError> {
    let table = qualified_relation(schema, "channel_space_links");
    tx.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {table} (\
               channel_jid TEXT PRIMARY KEY, \
               space_jid TEXT NOT NULL, \
               space_node TEXT NOT NULL, \
               created_at BIGINT NOT NULL\
             )"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS \
             space_node TEXT NOT NULL DEFAULT ''"
        ),
        (),
    )
    .await?;
    backfill_space_node_from_jid_postgres(tx, &table).await?;
    tx.execute(
        &format!("ALTER TABLE {table} ALTER COLUMN space_node SET NOT NULL"),
        (),
    )
    .await?;
    widen_postgres_i64_column(tx, schema, "channel_space_links", "created_at").await?;
    tx.execute(
        &format!(
            "CREATE INDEX IF NOT EXISTS {} ON {table} \
             (space_jid, created_at ASC, channel_jid ASC)",
            crate::db::physical_attestation::quote_identifier("idx_channel_space_links_space")
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE INDEX IF NOT EXISTS {} ON {table} \
             (space_node, created_at ASC, channel_jid ASC)",
            crate::db::physical_attestation::quote_identifier("idx_channel_space_links_space_node",)
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE INDEX IF NOT EXISTS {} ON {table} \
             (created_at ASC, channel_jid ASC)",
            crate::db::physical_attestation::quote_identifier("idx_channel_space_links_created_at",)
        ),
        (),
    )
    .await?;
    Ok(())
}

async fn backfill_space_node_from_jid_postgres(
    tx: &mut Transaction<'_>,
    table: &str,
) -> Result<(), DatabaseError> {
    let mut rows = tx
        .query(
            &format!(
                "SELECT channel_jid, space_jid FROM {table} \
                 WHERE space_node IS NULL OR space_node = ''"
            ),
            (),
        )
        .await?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next().await? {
        let channel_jid: String = row.get(0)?;
        let space_jid: String = row.get(1)?;
        updates.push((
            channel_jid,
            space_jid.clone(),
            projected_space_node_from_jid_text(&space_jid).into_string(),
        ));
    }
    drop(rows);

    for (channel_jid, space_jid, space_node) in updates {
        let affected = tx
            .execute(
                &format!(
                    "UPDATE {table} SET space_node = ? \
                     WHERE channel_jid = ? \
                       AND (space_node IS NULL OR space_node = '') \
                       AND space_jid = ?"
                ),
                crate::db_params![space_node, channel_jid.as_str(), space_jid.as_str()],
            )
            .await?;
        if affected == 0 {
            let mut rows = tx
                .query(
                    &format!("SELECT space_node FROM {table} WHERE channel_jid = ?"),
                    crate::db_params![channel_jid.as_str()],
                )
                .await?;
            let still_blank = match rows.next().await? {
                Some(row) => row
                    .get::<Option<String>>(0)?
                    .is_none_or(|node| node.is_empty()),
                None => false,
            };
            drop(rows);
            if still_blank {
                return Err(DatabaseError::QueryFailed(
                    "channel-space link backfill changed during the owner transaction".to_owned(),
                ));
            }
        }
    }

    let mut rows = tx
        .query(
            &format!(
                "SELECT COUNT(*)::bigint FROM {table} \
                 WHERE space_node IS NULL OR space_node = ''"
            ),
            (),
        )
        .await?;
    let unfilled: i64 = rows
        .next()
        .await?
        .ok_or_else(|| {
            DatabaseError::QueryFailed("space-node backfill recheck returned no row".to_owned())
        })?
        .get(0)?;
    if unfilled != 0 {
        return Err(DatabaseError::QueryFailed(
            "space-node backfill left incomplete rows".to_owned(),
        ));
    }
    Ok(())
}

async fn widen_postgres_i64_column(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
    column: &'static str,
) -> Result<(), DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT data_type FROM information_schema.columns \
             WHERE table_schema = ? AND table_name = ? AND column_name = ?",
            crate::db_params![schema, table, column],
        )
        .await?;
    let current_type: Option<String> = match rows.next().await? {
        Some(row) => Some(row.get(0)?),
        None => None,
    };
    drop(rows);
    if current_type
        .as_deref()
        .is_some_and(|data_type| !data_type.eq_ignore_ascii_case("bigint"))
    {
        tx.execute(
            &format!(
                "ALTER TABLE {} ALTER COLUMN {column} TYPE BIGINT",
                qualified_relation(schema, table)
            ),
            (),
        )
        .await?;
    }
    Ok(())
}

fn qualified_relation(schema: &str, relation: &str) -> String {
    format!(
        "{}.{}",
        crate::db::physical_attestation::quote_identifier(schema),
        crate::db::physical_attestation::quote_identifier(relation)
    )
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
