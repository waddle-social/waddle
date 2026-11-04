//! Read-only validation shared by the durable space-store attachments.

use std::collections::HashMap;

use thiserror::Error;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

#[derive(Debug, Error)]
pub enum SpaceStoreAttachError {
    #[error("runtime attachment for space stores requires PostgreSQL")]
    PostgresRequired,
    #[error("PostgreSQL space-store attachment requires a physically attested pool")]
    AttestedPostgresPoolRequired,
    #[error("PostgreSQL space-store attachment requires the main database lane")]
    MainLaneRequired,
    #[error("the attested PostgreSQL schema identifier is invalid")]
    InvalidSchema,
    #[error("the active PostgreSQL schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not verify the PostgreSQL {setting} bound for space-store attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("failed to inspect space-store relation {table}")]
    Relation {
        table: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("space-store relation {table} is not an ordinary table")]
    InvalidTable { table: &'static str },
    #[error("space-store relation {table} contains an unexpected column")]
    UnexpectedColumn { table: &'static str },
    #[error("space-store relation {table} has an invalid column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("space-store relation {table} has an invalid primary key")]
    InvalidPrimaryKey { table: &'static str },
    #[error("space-store index {index} is missing or has an invalid definition")]
    InvalidIndex { index: &'static str },
    #[error("space-store relation {table} contains an unexpected index")]
    UnexpectedIndex { table: &'static str },
    #[error("the PostgreSQL runtime role or space-store grants are unsafe")]
    UnsafeRuntimeRole,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

#[derive(Clone, Copy)]
pub(crate) enum ColumnKind {
    Text,
    Bigint,
}

#[derive(Clone, Copy)]
pub(crate) enum ColumnDefault {
    None,
    /// A legacy owner bootstrap adds `space_node TEXT NOT NULL DEFAULT ''`
    /// before backfilling it. Fresh schemas have no default, so both shapes
    /// are part of the supported owner contract for that column.
    OptionalEmptyText,
}

#[derive(Clone, Copy)]
pub(crate) struct ColumnContract {
    pub(crate) name: &'static str,
    pub(crate) kind: ColumnKind,
    pub(crate) not_null: bool,
    pub(crate) generated: bool,
    pub(crate) identity: bool,
    pub(crate) default: ColumnDefault,
}

impl ColumnContract {
    pub(crate) const fn plain(name: &'static str, kind: ColumnKind, not_null: bool) -> Self {
        Self {
            name,
            kind,
            not_null,
            generated: false,
            identity: false,
            default: ColumnDefault::None,
        }
    }

    pub(crate) const fn optional_empty_text_default(
        name: &'static str,
        kind: ColumnKind,
        not_null: bool,
    ) -> Self {
        Self {
            name,
            kind,
            not_null,
            generated: false,
            identity: false,
            default: ColumnDefault::OptionalEmptyText,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct IndexContract {
    pub(crate) name: &'static str,
    pub(crate) unique: bool,
    pub(crate) columns: &'static [&'static str],
}

pub(crate) struct TableContract {
    pub(crate) name: &'static str,
    pub(crate) columns: &'static [ColumnContract],
    pub(crate) primary_key: &'static [&'static str],
    pub(crate) indexes: &'static [IndexContract],
}

/// Validate one owner-created table on the exact attested PostgreSQL main
/// pool. The transaction is read-only, bounded, and holds qualified relation
/// locks until the schema snapshot has been checked.
pub(crate) async fn verify_attested_store(
    db: &Database,
    contract: &TableContract,
) -> Result<(), SpaceStoreAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(SpaceStoreAttachError::PostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(SpaceStoreAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(SpaceStoreAttachError::AttestedPostgresPoolRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;

    let identity = capability.identity();
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "space-store schema")
        .map_err(|_| SpaceStoreAttachError::InvalidSchema)?;
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);

    let mut tx = db.begin().await?;
    // A pooled BEGIN can inherit a transaction started by a previous
    // borrower. Requiring two distinct pre-query settings makes that state
    // fail closed, then opens the schema snapshot only after relation locks.
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
        (),
    )
    .await?;
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|error| match error {
            crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                SpaceStoreAttachError::InvalidTimeoutSetting { setting }
            }
            crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                SpaceStoreAttachError::Database(error)
            }
        })?;

    let lock_sql = format!(
        "LOCK TABLE {quoted_schema}.\"{}\" IN ACCESS SHARE MODE",
        contract.name
    );
    tx.execute(&lock_sql, ())
        .await
        .map_err(|source| SpaceStoreAttachError::Relation {
            table: contract.name,
            source,
        })?;

    // This proves the same physical database, schema OID, lineage, and login
    // attached the transaction used for every catalog inspection below.
    capability.attest_transaction(&mut tx).await?;
    verify_active_schema(&mut tx, schema, i64::from(identity.schema.oid)).await?;
    verify_table(&mut tx, schema, contract.name).await?;
    verify_columns(&mut tx, schema, contract).await?;
    verify_primary_key(&mut tx, schema, contract).await?;
    verify_indexes(&mut tx, schema, contract).await?;
    verify_runtime_acl(&mut tx, schema, contract.name).await?;

    // No mutation is performed. Roll back so the operation's read-only
    // boundary is explicit and no session state can be committed accidentally.
    tx.rollback().await?;
    Ok(())
}

async fn verify_active_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: i64,
) -> Result<(), SpaceStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.current_schema()::text, n.oid::bigint \
             FROM pg_catalog.pg_namespace n WHERE n.nspname = ?",
            crate::db_params![schema],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(SpaceStoreAttachError::ActiveSchemaMismatch);
    };
    let active: Option<String> = row.get(0)?;
    let actual_oid: i64 = row.get(1)?;
    if active.as_deref() != Some(schema) || actual_oid != schema_oid {
        return Err(SpaceStoreAttachError::ActiveSchemaMismatch);
    }
    Ok(())
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
) -> Result<(), SpaceStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' \
                    AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con \
                                    WHERE con.conrelid = c.oid AND con.contype <> 'p') \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                                    WHERE tr.tgrelid = c.oid AND NOT tr.tgisinternal) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                                    WHERE rw.ev_class = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                                    WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, table],
        )
        .await
        .map_err(|source| SpaceStoreAttachError::Relation { table, source })?;
    let Some(row) = rows.next().await? else {
        return Err(SpaceStoreAttachError::InvalidTable { table });
    };
    if !row.get::<bool>(0)? {
        return Err(SpaceStoreAttachError::InvalidTable { table });
    }
    Ok(())
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), SpaceStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull, a.attgenerated::text, a.attidentity::text, \
                    pg_catalog.pg_get_expr(d.adbin, d.adrelid) \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_catalog.pg_attrdef d \
               ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = ? AND c.relname = ? AND c.relkind = 'r' \
               AND a.attnum > 0 AND NOT a.attisdropped",
            crate::db_params![schema, contract.name],
        )
        .await
        .map_err(|source| SpaceStoreAttachError::Relation {
            table: contract.name,
            source,
        })?;
    let mut actual = HashMap::new();
    while let Some(row) = rows.next().await? {
        actual.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<String>(3)?,
                row.get::<String>(4)?,
                row.get::<Option<String>>(5)?,
            ),
        );
    }
    if actual.len() != contract.columns.len() {
        return Err(SpaceStoreAttachError::UnexpectedColumn {
            table: contract.name,
        });
    }
    for column in contract.columns {
        let Some((kind, not_null, generated, identity, default)) = actual.get(column.name) else {
            return Err(SpaceStoreAttachError::InvalidColumn {
                table: contract.name,
                column: column.name,
            });
        };
        let expected_kind = match column.kind {
            ColumnKind::Text => "text",
            ColumnKind::Bigint => "bigint",
        };
        let default_matches = match column.default {
            ColumnDefault::None => default.is_none(),
            ColumnDefault::OptionalEmptyText => default
                .as_deref()
                .is_none_or(|expression| expression == "''::text"),
        };
        if kind != expected_kind
            || *not_null != column.not_null
            || !generated.is_empty() != column.generated
            || !identity.is_empty() != column.identity
            || !default_matches
        {
            return Err(SpaceStoreAttachError::InvalidColumn {
                table: contract.name,
                column: column.name,
            });
        }
    }
    Ok(())
}

async fn verify_primary_key(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), SpaceStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             CROSS JOIN LATERAL pg_catalog.unnest(c.conkey) \
               WITH ORDINALITY k(attnum, position) \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             WHERE c.contype = 'p' AND NOT c.condeferrable AND c.convalidated \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_index i \
                           WHERE i.indexrelid = c.conindid AND i.indisvalid \
                             AND i.indisready AND i.indislive) \
               AND n.nspname = ? AND t.relname = ? ORDER BY k.position",
            crate::db_params![schema, contract.name],
        )
        .await
        .map_err(|source| SpaceStoreAttachError::Relation {
            table: contract.name,
            source,
        })?;
    let mut actual = Vec::new();
    while let Some(row) = rows.next().await? {
        actual.push(row.get::<String>(0)?);
    }
    if actual.iter().map(String::as_str).collect::<Vec<_>>() != contract.primary_key {
        return Err(SpaceStoreAttachError::InvalidPrimaryKey {
            table: contract.name,
        });
    }
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), SpaceStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.count(*)::bigint FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = ?",
            crate::db_params![schema, contract.name],
        )
        .await
        .map_err(|source| SpaceStoreAttachError::Relation {
            table: contract.name,
            source,
        })?;
    let index_count: i64 = rows
        .next()
        .await?
        .ok_or(SpaceStoreAttachError::InvalidTable {
            table: contract.name,
        })?
        .get(0)?;
    drop(rows);
    if index_count != contract.indexes.len() as i64 + 1 {
        return Err(SpaceStoreAttachError::UnexpectedIndex {
            table: contract.name,
        });
    }
    for expected in contract.indexes {
        let mut rows = tx
            .query(
                "SELECT i.indisunique, i.indisvalid, i.indisready, i.indislive, \
                        i.indisprimary, i.indpred IS NULL, i.indexprs IS NULL, \
                        am.amname = 'btree', i.indnkeyatts::bigint, i.indnatts::bigint, \
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
                         WHERE k.position <= i.indnkeyatts) \
                 FROM pg_catalog.pg_index i \
                 JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = ix.relnamespace \
                 JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
                 JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
                 WHERE n.nspname = ? AND ix.relname = ? \
                   AND t.relname = ? AND t.relnamespace = n.oid",
                crate::db_params![schema, expected.name, contract.name],
            )
            .await
            .map_err(|source| SpaceStoreAttachError::Relation {
                table: contract.name,
                source,
            })?;
        let Some(row) = rows.next().await? else {
            return Err(SpaceStoreAttachError::InvalidIndex {
                index: expected.name,
            });
        };
        let unique: bool = row.get(0)?;
        let valid: bool = row.get(1)?;
        let ready: bool = row.get(2)?;
        let live: bool = row.get(3)?;
        let primary: bool = row.get(4)?;
        let no_predicate: bool = row.get(5)?;
        let no_expression: bool = row.get(6)?;
        let btree: bool = row.get(7)?;
        let key_count: i64 = row.get(8)?;
        let total_count: i64 = row.get(9)?;
        let columns: String = row.get(10)?;
        let options_ascending: bool = row.get(11)?;
        if unique != expected.unique
            || !valid
            || !ready
            || !live
            || primary
            || !no_predicate
            || !no_expression
            || !btree
            || key_count != expected.columns.len() as i64
            || total_count != expected.columns.len() as i64
            || columns != expected.columns.join(",")
            || !options_ascending
        {
            return Err(SpaceStoreAttachError::InvalidIndex {
                index: expected.name,
            });
        }
    }
    Ok(())
}

async fn verify_runtime_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &'static str,
) -> Result<(), SpaceStoreAttachError> {
    // The attested capability identifies the runtime login, but does not carry
    // an allowlist of owner or installer roles. Verify the runtime's direct
    // writer boundary here; owner-side role membership remains provisioned
    // outside this attachment check.
    let mut rows = tx
        .query(
            r#"SELECT session_user::text = current_user::text
                    AND current_user::text = runtime.rolname
                    AND runtime.rolcanlogin AND NOT runtime.rolsuper
                    AND NOT runtime.rolcreaterole AND NOT runtime.rolcreatedb
                    AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls
                    AND runtime.oid <> database.datdba
                    AND NOT pg_catalog.has_database_privilege(
                      runtime.oid, database.oid, 'CREATE,TEMP')
                    AND NOT pg_catalog.has_parameter_privilege(
                      runtime.oid, 'session_replication_role', 'SET')
                    AND pg_catalog.current_setting('session_replication_role') = 'origin'
                    -- The runtime login has no inherited or inheritable roles.
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                                    WHERE m.member = runtime.oid OR m.roleid = runtime.oid)
                    AND pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'USAGE')
                    AND NOT pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'CREATE')
                    AND ns.nspowner = owner.oid AND relation.relowner = owner.oid
                    AND owner.oid <> runtime.oid
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.aclexplode(
                        CASE WHEN pg_catalog.cardinality(ns.nspacl) > 0
                          THEN ns.nspacl
                          ELSE pg_catalog.acldefault('n', ns.nspowner) END
                      ) acl WHERE acl.grantee = 0
                        OR (acl.grantee <> owner.oid
                            AND (acl.privilege_type <> 'USAGE' OR acl.is_grantable))
                    )
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT')
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT')
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'UPDATE')
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'DELETE')
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid, 'TRUNCATE, REFERENCES, TRIGGER')
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid,
                      'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION,
                       UPDATE WITH GRANT OPTION, DELETE WITH GRANT OPTION,
                       TRUNCATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION,
                       TRIGGER WITH GRANT OPTION')
                    AND NOT pg_catalog.has_any_column_privilege(
                      runtime.oid, relation.oid,
                      'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION,
                       UPDATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION')
                    AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000
                      THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN')
                      ELSE true END
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.aclexplode(
                        CASE WHEN pg_catalog.cardinality(relation.relacl) > 0
                          THEN relation.relacl
                          ELSE pg_catalog.acldefault('r', relation.relowner) END
                      ) acl WHERE acl.grantee NOT IN (owner.oid, runtime.oid)
                        OR (acl.grantee = runtime.oid AND (
                          acl.is_grantable OR acl.privilege_type NOT IN
                            ('SELECT', 'INSERT', 'UPDATE', 'DELETE')))
                    )
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.pg_attribute a
                      CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl
                      WHERE a.attrelid = relation.oid AND a.attnum > 0
                        AND NOT a.attisdropped
                    )
             FROM pg_catalog.pg_roles runtime
             JOIN pg_catalog.pg_namespace ns ON ns.nspname = ?
             JOIN pg_catalog.pg_roles owner ON owner.oid = ns.nspowner
             JOIN pg_catalog.pg_database database
               ON database.datname = pg_catalog.current_database()
             JOIN pg_catalog.pg_class relation
               ON relation.relnamespace = ns.oid AND relation.relname = ?
                  AND relation.relkind = 'r'
             WHERE runtime.rolname = current_user"#,
            crate::db_params![schema, table],
        )
        .await
        .map_err(|source| SpaceStoreAttachError::Relation { table, source })?;
    let safe = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !safe {
        return Err(SpaceStoreAttachError::UnsafeRuntimeRole);
    }
    Ok(())
}
