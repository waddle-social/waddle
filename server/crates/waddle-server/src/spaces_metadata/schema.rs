//! Idempotent `CREATE TABLE IF NOT EXISTS` for `spaces_metadata`,
//! mirroring the inbox schema migration shape.

use super::{DatabaseSpacesMetadataStore, SpacesMetadataError};
use crate::space_identity::projected_space_node_from_jid_text;

pub(super) async fn initialize(
    store: &DatabaseSpacesMetadataStore,
) -> Result<(), SpacesMetadataError> {
    let i64_type = crate::db::i64_sql_type(store.db.driver());
    let sql = create_table_sql("spaces_metadata", i64_type);
    store.execute(&sql, ()).await?;
    add_column_if_missing(store, "space_node TEXT NOT NULL DEFAULT ''").await?;
    backfill_space_node_from_jid(store).await?;
    rebuild_legacy_space_jid_primary_key(store, i64_type).await?;

    crate::db::widen_postgres_i64_column_to_bigint(&store.db, "spaces_metadata", "created_at")
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    crate::db::widen_postgres_i64_column_to_bigint(&store.db, "spaces_metadata", "updated_at")
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;

    store
        .execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_spaces_metadata_space_node \
             ON spaces_metadata (space_node)",
            (),
        )
        .await?;
    store
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_spaces_metadata_created_at \
             ON spaces_metadata (created_at ASC, space_node ASC)",
            (),
        )
        .await?;
    store
        .execute(
            "CREATE INDEX IF NOT EXISTS idx_spaces_metadata_space_jid \
             ON spaces_metadata (space_jid)",
            (),
        )
        .await?;
    Ok(())
}

fn create_table_sql(table_name: &str, i64_type: &str) -> String {
    format!(
        r#"
        CREATE TABLE IF NOT EXISTS spaces_metadata (
            space_node TEXT PRIMARY KEY,
            space_jid TEXT NOT NULL,
            name TEXT NOT NULL,
            description TEXT,
            icon_url TEXT,
            created_at {i64_type} NOT NULL,
            updated_at {i64_type} NOT NULL
        )
        "#
    )
    .replace("spaces_metadata", table_name)
}

async fn backfill_space_node_from_jid(
    store: &DatabaseSpacesMetadataStore,
) -> Result<(), SpacesMetadataError> {
    let mut rows = store
        .query(
            "SELECT space_jid FROM spaces_metadata WHERE space_node = ''",
            (),
        )
        .await?;
    let mut updates = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?
    {
        let space_jid_raw: String = row
            .get(0)
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        let space_node = projected_space_node_from_jid_text(&space_jid_raw);
        updates.push((space_jid_raw, space_node));
    }
    drop(rows);

    for (space_jid, space_node) in updates {
        let space_node = space_node.into_string();
        store
            .execute(
                "UPDATE spaces_metadata SET space_node = ? WHERE space_jid = ?",
                crate::db_params![space_node, space_jid],
            )
            .await?;
    }
    Ok(())
}

async fn rebuild_legacy_space_jid_primary_key(
    store: &DatabaseSpacesMetadataStore,
    i64_type: &str,
) -> Result<(), SpacesMetadataError> {
    if primary_key_column(store).await?.as_deref() != Some("space_jid") {
        return Ok(());
    }

    let mut tx = store
        .db
        .begin_immediate()
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    tx.execute("DROP TABLE IF EXISTS spaces_metadata_new", ())
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    tx.execute("DROP TABLE IF EXISTS spaces_metadata_old", ())
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    tx.execute(&create_table_sql("spaces_metadata_new", i64_type), ())
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    let copy_sql = match store.db.driver() {
        crate::db::DatabaseDriver::Sqlite => {
            r#"
            INSERT OR IGNORE INTO spaces_metadata_new (
                space_node, space_jid, name, description, icon_url, created_at, updated_at
            )
            SELECT space_node, space_jid, name, description, icon_url, created_at, updated_at
            FROM spaces_metadata
            WHERE space_node <> ''
            "#
        }
        crate::db::DatabaseDriver::Postgres => {
            r#"
            INSERT INTO spaces_metadata_new (
                space_node, space_jid, name, description, icon_url, created_at, updated_at
            )
            SELECT space_node, space_jid, name, description, icon_url, created_at, updated_at
            FROM spaces_metadata
            WHERE space_node <> ''
            ON CONFLICT (space_node) DO NOTHING
            "#
        }
    };
    tx.execute(copy_sql, ())
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    tx.execute(
        "ALTER TABLE spaces_metadata RENAME TO spaces_metadata_old",
        (),
    )
    .await
    .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    tx.execute(
        "ALTER TABLE spaces_metadata_new RENAME TO spaces_metadata",
        (),
    )
    .await
    .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    tx.execute("DROP TABLE spaces_metadata_old", ())
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    tx.commit()
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    Ok(())
}

async fn primary_key_column(
    store: &DatabaseSpacesMetadataStore,
) -> Result<Option<String>, SpacesMetadataError> {
    match store.db.driver() {
        crate::db::DatabaseDriver::Sqlite => sqlite_primary_key_column(store).await,
        crate::db::DatabaseDriver::Postgres => postgres_primary_key_column(store).await,
    }
}

async fn sqlite_primary_key_column(
    store: &DatabaseSpacesMetadataStore,
) -> Result<Option<String>, SpacesMetadataError> {
    let mut rows = store
        .query("PRAGMA table_info(spaces_metadata)", ())
        .await?;
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?
    {
        let column_name: String = row
            .get(1)
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        let pk: i64 = row
            .get(5)
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
        if pk > 0 {
            return Ok(Some(column_name));
        }
    }
    Ok(None)
}

async fn postgres_primary_key_column(
    store: &DatabaseSpacesMetadataStore,
) -> Result<Option<String>, SpacesMetadataError> {
    let mut rows = store
        .query(
            r#"
            SELECT a.attname
            FROM pg_index i
            JOIN pg_attribute a
              ON a.attrelid = i.indrelid
             AND a.attnum = ANY(i.indkey)
            WHERE i.indrelid = 'spaces_metadata'::regclass
              AND i.indisprimary
            LIMIT 1
            "#,
            (),
        )
        .await?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?
    else {
        return Ok(None);
    };
    let column_name: String = row
        .get(0)
        .map_err(|error| SpacesMetadataError::Storage(error.to_string()))?;
    Ok(Some(column_name))
}

async fn add_column_if_missing(
    store: &DatabaseSpacesMetadataStore,
    definition: &'static str,
) -> Result<(), SpacesMetadataError> {
    match store
        .execute(
            &format!("ALTER TABLE spaces_metadata ADD COLUMN {definition}"),
            (),
        )
        .await
    {
        Ok(_) => Ok(()),
        Err(SpacesMetadataError::Storage(error))
            if error.contains("duplicate column") || error.contains("already exists") =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}
