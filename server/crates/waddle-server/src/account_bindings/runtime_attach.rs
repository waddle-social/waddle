//! Read-only structural validation of the owner-prepared account resource
//! relations, for a runtime that attaches on the attested PostgreSQL pool.

use std::collections::HashMap;

use crate::db::read_only_timeout::{bound_postgres_waits, ReadOnlyTimeoutError};
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

#[derive(Debug, thiserror::Error)]
pub enum AccountBindingAttachError {
    #[error("account binding runtime attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("account binding runtime attachment requires the main database admission lane")]
    MainLaneRequired,
    #[error("account binding runtime attachment requires a physically attested PostgreSQL pool")]
    AttestedPostgresPoolRequired,
    #[error("the active PostgreSQL schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not verify the PostgreSQL {setting} bound for account binding attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("failed to inspect account binding relation {relation}: {source}")]
    Relation {
        relation: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("account binding relation {table} is not an ordinary table in the attested schema")]
    InvalidTable { table: &'static str },
    #[error("account binding relation {table} has an invalid column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("account binding relation {table} has an invalid primary key")]
    InvalidPrimaryKey { table: &'static str },
    #[error(
        "account binding relation {table} is missing a CHECK constraint or has an invalid one"
    )]
    InvalidCheckConstraint { table: &'static str },
    #[error("the deferred stream-management session reference is missing or invalid")]
    InvalidSessionReference,
    #[error("account binding index {index} is missing or has an invalid definition")]
    InvalidIndex { index: &'static str },
    #[error("account binding runtime role or privileges are unsafe at {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error("account binding relation {component} contains unexpected triggers or rules")]
    UnexpectedExecutableSchema { component: &'static str },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

struct Column {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
}

const fn column(name: &'static str, kind: &'static str, not_null: bool) -> Column {
    Column {
        name,
        kind,
        not_null,
    }
}

// The profile is derived from the owner initializer in `super::schema`.
const BINDINGS: &[Column] = &[
    column("full_jid", "text", true),
    column("bare_jid", "text", true),
    column("claim_identity", "text", true),
    column("revision", "bigint", true),
    column("install_generation", "bigint", true),
    column("operation_seq", "bigint", true),
    column("state", "integer", true),
    column("binding_ref", "text", false),
    column("binding_claim_identity", "text", false),
    column("logical_generation", "text", false),
    column("occupancy_generation", "text", false),
    column("endpoint_published", "integer", true),
    column("actor_registered", "integer", true),
    column("presence_available", "integer", true),
    column("presence_show", "integer", false),
    column("presence_status", "text", false),
    column("presence_priority", "integer", true),
    column("carbons_enabled", "integer", true),
    column("roster_interested", "integer", true),
    column("blocklist_interested", "integer", true),
    column("sm_session_id", "text", false),
    column("sm_generation", "bigint", false),
    column("snapshot_revision", "bigint", true),
    column("detach_custody", "integer", false),
    column("retiring_from", "integer", false),
    column("retirement_source_revision", "bigint", false),
    column("retirement_custody", "integer", false),
    column("offline_flushed", "integer", true),
    column("subscriptions_flushed", "integer", true),
    column("updated_at_ms", "bigint", true),
];
const OPERATIONS: &[Column] = &[
    column("operation_id", "text", true),
    column("full_jid", "text", true),
    column("kind", "integer", true),
    column("binding_ref", "text", true),
    column("expected_revision", "bigint", true),
    column("committed_revision", "bigint", true),
    column("operation_seq", "bigint", true),
    column("recorded_at_ms", "bigint", true),
];

struct Table {
    name: &'static str,
    columns: &'static [Column],
    primary_key: &'static [&'static str],
}

const TABLES: &[Table] = &[
    Table {
        name: "account_resource_bindings",
        columns: BINDINGS,
        primary_key: &["full_jid"],
    },
    Table {
        name: "account_resource_operations",
        columns: OPERATIONS,
        primary_key: &["operation_id"],
    },
];

struct Index {
    table: &'static str,
    name: &'static str,
    definition: &'static str,
}

const INDEXES: &[Index] = &[
    Index {
        table: "account_resource_bindings",
        name: "idx_account_resource_bindings_account",
        definition: "USING btree (bare_jid)",
    },
    Index {
        table: "account_resource_bindings",
        name: "idx_account_resource_bindings_sm_session",
        definition: "USING btree (sm_session_id)",
    },
    Index {
        table: "account_resource_operations",
        name: "idx_account_resource_operations_resource",
        definition: "USING btree (full_jid, operation_seq)",
    },
];

/// The lifecycle CHECK, as `pg_get_constraintdef` prints it.
const STATE_CHECK: &str = "CHECK ((state = ANY (ARRAY[0, 1, 2, 3, 4, 5])))";

/// Verify the account resource relations and the runtime role's exact
/// privileges on them in one bounded read-only snapshot.
pub(super) async fn verify(db: &Database) -> Result<(), AccountBindingAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(AccountBindingAttachError::PostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(AccountBindingAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(AccountBindingAttachError::AttestedPostgresPoolRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;

    let mut tx = db.begin().await?;
    // Detect an inherited snapshot before establishing the bounded
    // read-only snapshot every inspection below reads.
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
    bound_postgres_waits(&mut tx)
        .await
        .map_err(|error| match error {
            ReadOnlyTimeoutError::InvalidSetting { setting } => {
                AccountBindingAttachError::InvalidTimeoutSetting { setting }
            }
            ReadOnlyTimeoutError::Database(error) => AccountBindingAttachError::Database(error),
        })?;
    let expected = &capability.identity().schema;
    let schema = &expected.name.0;
    let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
    // Lock every relation before the first snapshot-forming SELECT, so an
    // owner cannot commit DDL between the snapshot and the lock.
    for table in TABLES {
        tx.execute(
            &format!(
                "LOCK TABLE {quoted}.\"{}\" IN ACCESS SHARE MODE",
                table.name
            ),
            (),
        )
        .await
        .map_err(|source| AccountBindingAttachError::Relation {
            relation: table.name,
            source,
        })?;
    }
    capability.attest_transaction(&mut tx).await?;
    let mut rows = tx
        .query(
            "SELECT n.nspname::text, n.oid::bigint FROM pg_catalog.pg_namespace n \
             WHERE n.nspname = pg_catalog.current_schema()",
            (),
        )
        .await?;
    let valid = match rows.next().await? {
        Some(row) => {
            row.get::<String>(0)? == expected.name.0
                && row.get::<i64>(1)? == i64::from(expected.oid)
        }
        None => false,
    };
    drop(rows);
    if !valid {
        return Err(AccountBindingAttachError::ActiveSchemaMismatch);
    }
    for table in TABLES {
        verify_table(&mut tx, schema, &quoted, table).await?;
    }
    verify_constraints(&mut tx, schema).await?;
    verify_indexes(&mut tx, schema).await?;
    if let Some(component) =
        crate::db::runtime_store_acl::unexpected_account_binding_executable_schema(&mut tx, schema)
            .await?
    {
        return Err(AccountBindingAttachError::UnexpectedExecutableSchema { component });
    }
    if let Some(component) =
        crate::db::runtime_store_acl::verify_account_bindings(&mut tx, schema).await?
    {
        return Err(AccountBindingAttachError::UnsafeRuntimePrivileges { component });
    }
    tx.rollback().await?;
    Ok(())
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    quoted_schema: &str,
    table: &Table,
) -> Result<(), AccountBindingAttachError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM pg_catalog.pg_class t \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = ? AND t.relkind = 'r'",
            crate::db_params![schema, table.name],
        )
        .await?;
    if rows.next().await?.is_none() {
        return Err(AccountBindingAttachError::InvalidTable { table: table.name });
    }
    drop(rows);
    let mut rows = tx
        .query(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
             a.attnotnull, pg_catalog.pg_get_expr(d.adbin, d.adrelid), \
             a.attgenerated::text, a.attidentity::text \
             FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class t ON t.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = ? AND t.relname = ? AND a.attnum > 0 AND NOT a.attisdropped",
            crate::db_params![schema, table.name],
        )
        .await?;
    let mut columns = HashMap::new();
    while let Some(row) = rows.next().await? {
        columns.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<Option<String>>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
            ),
        );
    }
    drop(rows);
    for column in table.columns {
        let valid = columns.get(column.name).is_some_and(
            |(kind, not_null, default, generated, identity)| {
                kind == column.kind
                    && *not_null == column.not_null
                    && default.is_none()
                    && generated.is_empty()
                    && identity.is_empty()
            },
        );
        if !valid {
            return Err(AccountBindingAttachError::InvalidColumn {
                table: table.name,
                column: column.name,
            });
        }
    }
    let mut rows = tx
        .query(
            "SELECT a.attname::text FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             CROSS JOIN LATERAL pg_catalog.unnest(c.conkey) WITH ORDINALITY k(attnum, position) \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             WHERE c.contype = 'p' AND NOT c.condeferrable AND c.convalidated \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_index pi WHERE pi.indexrelid = c.conindid \
                 AND pi.indisvalid AND pi.indisready AND pi.indislive) \
               AND n.nspname = ? AND t.relname = ? ORDER BY k.position",
            crate::db_params![schema, table.name],
        )
        .await?;
    let mut key = Vec::new();
    while let Some(row) = rows.next().await? {
        key.push(row.get::<String>(0)?);
    }
    drop(rows);
    if key.iter().map(String::as_str).collect::<Vec<_>>() != table.primary_key {
        return Err(AccountBindingAttachError::InvalidPrimaryKey { table: table.name });
    }
    // Prove SELECT access to every column the store reads.
    let names = table
        .columns
        .iter()
        .map(|column| column.name)
        .collect::<Vec<_>>()
        .join(", ");
    tx.query(
        &format!(
            "SELECT {names} FROM {quoted_schema}.\"{}\" WHERE false",
            table.name
        ),
        (),
    )
    .await
    .map_err(|source| AccountBindingAttachError::Relation {
        relation: table.name,
        source,
    })?;
    Ok(())
}

async fn verify_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), AccountBindingAttachError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = 'account_resource_bindings' \
               AND c.contype = 'c' AND c.convalidated \
               AND pg_catalog.pg_get_constraintdef(c.oid) = ?",
            crate::db_params![schema, STATE_CHECK],
        )
        .await?;
    if rows.next().await?.is_none() {
        return Err(AccountBindingAttachError::InvalidCheckConstraint {
            table: "account_resource_bindings",
        });
    }
    drop(rows);
    // The session reference is one validated foreign key from
    // `sm_session_id` to the durable session relation's key, checked at
    // commit.
    let mut rows = tx
        .query(
            "SELECT 1 FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_class f ON f.oid = c.confrelid AND f.relnamespace = n.oid \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = c.conkey[1] \
             JOIN pg_catalog.pg_attribute fa ON fa.attrelid = f.oid AND fa.attnum = c.confkey[1] \
             WHERE n.nspname = ? AND t.relname = 'account_resource_bindings' \
               AND c.conname = 'account_resource_bindings_sm_session_fk' \
               AND c.contype = 'f' AND c.convalidated AND c.condeferrable AND c.condeferred \
               AND pg_catalog.cardinality(c.conkey) = 1 \
               AND f.relname = 'sm_durable_sessions' \
               AND a.attname = 'sm_session_id' AND fa.attname = 'session_id' \
               AND c.confdeltype = 'a' AND c.confupdtype = 'a'",
            crate::db_params![schema],
        )
        .await?;
    if rows.next().await?.is_none() {
        return Err(AccountBindingAttachError::InvalidSessionReference);
    }
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), AccountBindingAttachError> {
    for index in INDEXES {
        let mut rows = tx
            .query(
                "SELECT x.indisunique, pg_catalog.pg_get_indexdef(x.indexrelid) \
                 FROM pg_catalog.pg_index x JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid \
                 JOIN pg_catalog.pg_class t ON t.oid = x.indrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                 WHERE n.nspname = ? AND t.relname = ? AND i.relname = ? \
                   AND i.relnamespace = n.oid \
                   AND x.indisvalid AND x.indisready AND x.indislive",
                crate::db_params![schema, index.table, index.name],
            )
            .await?;
        let valid = match rows.next().await? {
            Some(row) => {
                !row.get::<bool>(0)?
                    && row
                        .get::<String>(1)?
                        .split_once(" USING ")
                        .is_some_and(|(_, suffix)| format!("USING {suffix}") == index.definition)
            }
            None => false,
        };
        if !valid {
            return Err(AccountBindingAttachError::InvalidIndex { index: index.name });
        }
    }
    Ok(())
}
