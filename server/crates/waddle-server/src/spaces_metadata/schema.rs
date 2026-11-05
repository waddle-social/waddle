//! Idempotent `CREATE TABLE IF NOT EXISTS` for `spaces_metadata`,
//! mirroring the inbox schema migration shape.

use std::collections::HashMap;

use super::{DatabaseSpacesMetadataStore, SpacesMetadataError};
use crate::db::DatabaseDriver;
use crate::db::{DatabaseError, Transaction};
use crate::space_identity::projected_space_node_from_jid_text;

pub(super) async fn initialize(
    store: &DatabaseSpacesMetadataStore,
) -> Result<(), SpacesMetadataError> {
    match store.db.driver() {
        DatabaseDriver::Sqlite => initialize_sqlite(store).await,
        DatabaseDriver::Postgres => {
            crate::spaces_metadata::runtime_attach::initialize_raw_space_store(
                &store.db,
                crate::spaces_metadata::runtime_attach::SpaceStoreKind::SpacesMetadata,
            )
            .await
            .map_err(|error| SpacesMetadataError::Storage(error.to_string()))
        }
    }
}

async fn initialize_sqlite(store: &DatabaseSpacesMetadataStore) -> Result<(), SpacesMetadataError> {
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

pub(crate) async fn initialize_postgres_in_transaction(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), DatabaseError> {
    let table = qualified_relation(schema, "spaces_metadata");
    tx.execute(&create_table_sql(&table, "BIGINT"), ()).await?;
    tx.execute(
        &format!(
            "ALTER TABLE {table} ADD COLUMN IF NOT EXISTS space_node TEXT NOT NULL DEFAULT ''"
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

    if postgres_primary_key_column_in_transaction(tx, schema)
        .await?
        .as_deref()
        == Some("space_jid")
    {
        verify_legacy_space_metadata_source(tx, schema).await?;
        reject_existing_rebuild_scratch(tx, schema).await?;
        rebuild_legacy_space_jid_primary_key_postgres(tx, schema).await?;
    }

    widen_postgres_i64_column(tx, schema, "spaces_metadata", "created_at").await?;
    widen_postgres_i64_column(tx, schema, "spaces_metadata", "updated_at").await?;
    tx.execute(
        &format!(
            "CREATE UNIQUE INDEX IF NOT EXISTS {} ON {table} (space_node)",
            crate::db::physical_attestation::quote_identifier("idx_spaces_metadata_space_node",)
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE INDEX IF NOT EXISTS {} ON {table} (created_at ASC, space_node ASC)",
            crate::db::physical_attestation::quote_identifier("idx_spaces_metadata_created_at",)
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE INDEX IF NOT EXISTS {} ON {table} (space_jid)",
            crate::db::physical_attestation::quote_identifier("idx_spaces_metadata_space_jid",)
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
            &format!("SELECT space_jid FROM {table} WHERE space_node IS NULL OR space_node = ''"),
            (),
        )
        .await?;
    let mut updates = Vec::new();
    while let Some(row) = rows.next().await? {
        let space_jid: String = row.get(0)?;
        updates.push((
            space_jid.clone(),
            projected_space_node_from_jid_text(&space_jid).into_string(),
        ));
    }
    drop(rows);

    for (space_jid, space_node) in updates {
        tx.execute(
            &format!(
                "UPDATE {table} SET space_node = ? \
                 WHERE space_jid = ? AND (space_node IS NULL OR space_node = '')"
            ),
            crate::db_params![space_node, space_jid],
        )
        .await?;
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

async fn postgres_primary_key_column_in_transaction(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<Option<String>, DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON c.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             CROSS JOIN LATERAL pg_catalog.unnest(i.indkey) \
               WITH ORDINALITY k(attnum, position) \
             JOIN pg_catalog.pg_attribute a \
               ON a.attrelid = c.oid AND a.attnum = k.attnum \
             WHERE i.indisprimary AND n.nspname = ? \
               AND c.relname = 'spaces_metadata' ORDER BY k.position LIMIT 1",
            crate::db_params![schema],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get(0)?)),
        None => Ok(None),
    }
}

async fn rebuild_legacy_space_jid_primary_key_postgres(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), DatabaseError> {
    let old = qualified_relation(schema, "spaces_metadata");
    let new = qualified_relation(schema, "spaces_metadata_new");
    let backup = qualified_relation(schema, "spaces_metadata_old");
    tx.execute(&create_table_sql(&new, "BIGINT"), ()).await?;
    tx.execute(
        &format!(
            "INSERT INTO {new} (space_node, space_jid, name, description, \
             icon_url, created_at, updated_at) \
             SELECT space_node, space_jid, name, description, icon_url, \
                    created_at, updated_at \
             FROM {old} WHERE space_node IS NOT NULL AND space_node <> ''"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!("ALTER TABLE {old} RENAME TO spaces_metadata_old"),
        (),
    )
    .await?;
    tx.execute(&format!("ALTER TABLE {new} RENAME TO spaces_metadata"), ())
        .await?;
    tx.execute(&format!("DROP TABLE {backup}"), ()).await?;
    Ok(())
}

/// A legacy rebuild replaces the original table, so only migrate the exact
/// historical schema we know how to copy. Unknown columns, constraints,
/// indexes, triggers, or dependencies stay in place for an explicit repair.
async fn verify_legacy_space_metadata_source(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' \
                    AND NOT c.relispartition AND c.reloftype = 0 \
                    AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                    AND c.relowner = n.nspowner \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                                    WHERE tr.tgrelid = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                                    WHERE rw.ev_class = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy pol \
                                    WHERE pol.polrelid = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                                    WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint fk \
                                    WHERE fk.contype = 'f' AND fk.confrelid = c.oid) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = 'spaces_metadata'",
            crate::db_params![schema],
        )
        .await?;
    let source_is_plain_table = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    drop(rows);
    if !source_is_plain_table {
        return Err(legacy_source_shape_error(
            "source table has an unsupported relation or dependency",
        ));
    }

    let mut rows = tx
        .query(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull, a.attcollation = typ.typcollation, \
                    a.attgenerated::text, a.attidentity::text, \
                    pg_catalog.pg_get_expr(d.adbin, d.adrelid) \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_type typ ON typ.oid = a.atttypid \
             LEFT JOIN pg_catalog.pg_attrdef d \
               ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = ? AND c.relname = 'spaces_metadata' \
               AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            crate::db_params![schema],
        )
        .await?;
    let mut columns = HashMap::new();
    while let Some(row) = rows.next().await? {
        columns.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<bool>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
                row.get::<Option<String>>(6)?,
            ),
        );
    }
    drop(rows);
    if columns.len() != 7 {
        return Err(legacy_source_shape_error(
            "source table has unknown or missing columns",
        ));
    }
    for name in [
        "space_node",
        "space_jid",
        "name",
        "description",
        "icon_url",
        "created_at",
        "updated_at",
    ] {
        let Some((kind, not_null, default_collation, generated, identity, default)) =
            columns.get(name)
        else {
            return Err(legacy_source_shape_error(
                "source table has unknown or missing columns",
            ));
        };
        let default_is_supported = match name {
            "space_node" => default
                .as_deref()
                .is_none_or(|expression| expression == "''::text"),
            _ => default.is_none(),
        };
        let expected_type = match name {
            "space_node" | "space_jid" | "name" | "description" | "icon_url" => kind == "text",
            "created_at" | "updated_at" => {
                matches!(kind.as_str(), "smallint" | "integer" | "bigint")
            }
            _ => false,
        };
        let expected_nullability = match name {
            "description" | "icon_url" => !*not_null,
            _ => *not_null,
        };
        if !expected_type
            || !expected_nullability
            || !*default_collation
            || !generated.is_empty()
            || !identity.is_empty()
            || !default_is_supported
        {
            return Err(legacy_source_shape_error(
                "source table has an unsupported column definition",
            ));
        }
    }

    let mut rows = tx
        .query(
            "SELECT con.contype::text, con.convalidated, con.condeferrable, \
                    pg_catalog.array_to_string(ARRAY( \
                      SELECT a.attname::text \
                      FROM pg_catalog.unnest(con.conkey) \
                        WITH ORDINALITY k(attnum, position) \
                      JOIN pg_catalog.pg_attribute a \
                        ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
                      ORDER BY k.position), ',') \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = 'spaces_metadata'",
            crate::db_params![schema],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(legacy_source_shape_error("source primary key is missing"));
    };
    let constraint = (
        row.get::<String>(0)?,
        row.get::<bool>(1)?,
        row.get::<bool>(2)?,
        row.get::<String>(3)?,
    );
    if rows.next().await?.is_some()
        || constraint.0 != "p"
        || !constraint.1
        || constraint.2
        || constraint.3 != "space_jid"
    {
        return Err(legacy_source_shape_error(
            "source has unsupported constraints",
        ));
    }
    drop(rows);

    let mut rows = tx
        .query(
            "SELECT ix.relname::text, i.indisprimary, i.indisunique, \
                    i.indisvalid AND i.indisready AND i.indislive AND i.indimmediate, \
                    i.indpred IS NULL, i.indexprs IS NULL, am.amname = 'btree', \
                    i.indnkeyatts::bigint, i.indnatts::bigint, \
                    pg_catalog.array_to_string(ARRAY( \
                      SELECT a.attname::text \
                      FROM pg_catalog.unnest(i.indkey) \
                        WITH ORDINALITY k(attnum, position) \
                      JOIN pg_catalog.pg_attribute a \
                        ON a.attrelid = t.oid AND a.attnum = k.attnum \
                      WHERE k.position <= i.indnkeyatts \
                      ORDER BY k.position), ','), \
                    (SELECT COALESCE(pg_catalog.bool_and(k.option = 0), true) \
                     FROM pg_catalog.unnest(i.indoption) \
                       WITH ORDINALITY k(option, position) \
                     WHERE k.position <= i.indnkeyatts), \
                    (SELECT COALESCE(pg_catalog.bool_and( \
                       opc.opcdefault AND opc.opcintype = a.atttypid), true) \
                     FROM pg_catalog.unnest(i.indclass) \
                       WITH ORDINALITY k(opclass_oid, position) \
                     JOIN pg_catalog.pg_opclass opc ON opc.oid = k.opclass_oid \
                     JOIN LATERAL pg_catalog.unnest(i.indkey) \
                       WITH ORDINALITY key_column(attnum, position) \
                       ON key_column.position = k.position \
                     JOIN pg_catalog.pg_attribute a \
                       ON a.attrelid = t.oid AND a.attnum = key_column.attnum \
                     WHERE k.position <= i.indnkeyatts), \
                    (SELECT COALESCE(pg_catalog.bool_and( \
                       k.collation_oid = a.attcollation \
                       AND a.attcollation = typ.typcollation), true) \
                     FROM pg_catalog.unnest(i.indcollation) \
                       WITH ORDINALITY k(collation_oid, position) \
                     JOIN LATERAL pg_catalog.unnest(i.indkey) \
                       WITH ORDINALITY key_column(attnum, position) \
                       ON key_column.position = k.position \
                     JOIN pg_catalog.pg_attribute a \
                       ON a.attrelid = t.oid AND a.attnum = key_column.attnum \
                     JOIN pg_catalog.pg_type typ ON typ.oid = a.atttypid \
                     WHERE k.position <= i.indnkeyatts) \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
             WHERE t.relnamespace = (SELECT oid FROM pg_catalog.pg_namespace \
                                     WHERE nspname = ?) \
               AND t.relname = 'spaces_metadata'",
            crate::db_params![schema],
        )
        .await?;
    let mut primary_index_count = 0;
    let mut legacy_created_at_index_count = 0;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(0)?;
        let primary: bool = row.get(1)?;
        let unique: bool = row.get(2)?;
        let valid: bool = row.get(3)?;
        let no_predicate: bool = row.get(4)?;
        let no_expression: bool = row.get(5)?;
        let btree: bool = row.get(6)?;
        let key_count: i64 = row.get(7)?;
        let total_count: i64 = row.get(8)?;
        let columns: String = row.get(9)?;
        let ascending: bool = row.get(10)?;
        let default_opclasses: bool = row.get(11)?;
        let default_collations: bool = row.get(12)?;
        let common_shape = valid
            && no_predicate
            && no_expression
            && btree
            && key_count == total_count
            && ascending
            && default_opclasses
            && default_collations;
        match name.as_str() {
            "spaces_metadata_pkey" if primary => {
                primary_index_count += 1;
                if !common_shape || !unique || key_count != 1 || columns != "space_jid" {
                    return Err(legacy_source_shape_error(
                        "source primary index is unsupported",
                    ));
                }
            }
            "idx_spaces_metadata_created_at" if !primary => {
                legacy_created_at_index_count += 1;
                if !common_shape || unique || key_count != 2 || columns != "created_at,space_jid" {
                    return Err(legacy_source_shape_error(
                        "source created-at index is unsupported",
                    ));
                }
            }
            _ => {
                return Err(legacy_source_shape_error(
                    "source table has an unknown index",
                ));
            }
        }
    }
    if primary_index_count != 1 || legacy_created_at_index_count > 1 {
        return Err(legacy_source_shape_error(
            "source indexes do not match the supported legacy shape",
        ));
    }
    Ok(())
}

async fn reject_existing_rebuild_scratch(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), DatabaseError> {
    let mut rows = tx
        .query(
            "SELECT c.relname::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? \
               AND c.relname IN ('spaces_metadata_new', 'spaces_metadata_old') \
             LIMIT 1",
            crate::db_params![schema],
        )
        .await?;
    if rows.next().await?.is_some() {
        return Err(legacy_source_shape_error(
            "rebuild scratch relation already exists",
        ));
    }
    Ok(())
}

fn legacy_source_shape_error(reason: &str) -> DatabaseError {
    DatabaseError::QueryFailed(format!(
        "refusing to rebuild unsupported legacy spaces_metadata: {reason}"
    ))
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

fn create_table_sql(table_name: &str, i64_type: &str) -> String {
    format!(
        r#"
        CREATE TABLE IF NOT EXISTS {table_name} (
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
        DatabaseDriver::Sqlite => {
            r#"
            INSERT OR IGNORE INTO spaces_metadata_new (
                space_node, space_jid, name, description, icon_url, created_at, updated_at
            )
            SELECT space_node, space_jid, name, description, icon_url, created_at, updated_at
            FROM spaces_metadata
            WHERE space_node <> ''
            "#
        }
        DatabaseDriver::Postgres => {
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
        DatabaseDriver::Sqlite => sqlite_primary_key_column(store).await,
        DatabaseDriver::Postgres => postgres_primary_key_column(store).await,
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
