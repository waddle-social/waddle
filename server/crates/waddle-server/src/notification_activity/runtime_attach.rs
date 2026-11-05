//! Read-only PostgreSQL attachment checks shared by notification stores.

use std::collections::HashMap;

use super::schema::{
    ACTIVITY_OWNER_STATE_REVISION, ACTIVITY_OWNER_STATE_REVISION_CHECK,
    ACTIVITY_OWNER_STATE_SINGLETON_CHECK, ACTIVITY_OWNER_STATE_TABLE, ACTIVITY_SCHEMA_GATE,
};
use crate::db::lineage::PgIdentity;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum DurableStoreAttachError {
    #[error("notification store attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("notification store attachment requires the main database lane")]
    MainLaneRequired,
    #[error("notification store attachment requires a physically attested pool")]
    AttestedPoolRequired,
    #[error("the attested PostgreSQL schema identifier is invalid")]
    InvalidSchema,
    #[error("the active PostgreSQL schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not verify the PostgreSQL {setting} bound for notification store attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("failed to inspect notification store relation {table}")]
    Relation {
        table: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("notification store relation {table} is not an ordinary table")]
    InvalidTable { table: &'static str },
    #[error("notification store relation {table} has an unexpected inbound foreign key")]
    InboundForeignKey { table: &'static str },
    #[error("notification store relation {table} has an unexpected trigger")]
    UnexpectedTrigger { table: &'static str },
    #[error("notification store relation {table} contains an unexpected column")]
    UnexpectedColumn { table: &'static str },
    #[error("notification store relation {table} has an invalid column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("notification store relation {table} has an invalid primary key")]
    InvalidPrimaryKey { table: &'static str },
    #[error("notification store index {index} is missing or has an invalid definition")]
    InvalidIndex { index: &'static str },
    #[error("notification store relation {table} contains an unexpected index")]
    UnexpectedIndex { table: &'static str },
    #[error("notification store relation {table} has an invalid constraint")]
    InvalidConstraint { table: &'static str },
    #[error("the PostgreSQL runtime role or notification-store grants are unsafe")]
    UnsafeRuntimeRole,
    #[error("notification activity owner preparation is incomplete or missing")]
    OwnerPreparationIncomplete,
    #[error("notification activity owner marker has an invalid shape")]
    InvalidOwnerMarker,
    #[error("notification activity owner marker belongs to a different physical schema")]
    OwnerIdentityMismatch,
    #[error("notification store relation {table} could not be locked")]
    RelationLock {
        table: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

#[derive(Clone, Copy)]
enum ColumnKind {
    Text,
    Integer,
    Bigint,
}

#[derive(Clone, Copy)]
enum ColumnDefault {
    None,
    EmptyText,
    ZeroInteger,
}

#[derive(Clone, Copy)]
struct ColumnContract {
    name: &'static str,
    kind: ColumnKind,
    not_null: bool,
    default: ColumnDefault,
}

impl ColumnContract {
    const fn plain(name: &'static str, kind: ColumnKind, not_null: bool) -> Self {
        Self {
            name,
            kind,
            not_null,
            default: ColumnDefault::None,
        }
    }

    const fn empty_text_default(name: &'static str, not_null: bool) -> Self {
        Self {
            name,
            kind: ColumnKind::Text,
            not_null,
            default: ColumnDefault::EmptyText,
        }
    }

    const fn zero_integer_default(name: &'static str, not_null: bool) -> Self {
        Self {
            name,
            kind: ColumnKind::Integer,
            not_null,
            default: ColumnDefault::ZeroInteger,
        }
    }
}

#[derive(Clone, Copy)]
struct IndexContract {
    name: &'static str,
    unique: bool,
    columns: &'static [&'static str],
}

#[derive(Clone, Copy)]
struct CheckContract {
    name: &'static str,
    column: &'static str,
    nullable: bool,
    values: &'static [&'static str],
}

pub(crate) struct TableContract {
    name: &'static str,
    columns: &'static [ColumnContract],
    primary_key: &'static [&'static str],
    indexes: &'static [IndexContract],
    checks: &'static [CheckContract],
    runtime_delete: bool,
}

const ACTIVITY_COLUMNS: &[ColumnContract] = &[
    ColumnContract::plain("owner_bare_jid", ColumnKind::Text, true),
    ColumnContract::plain("conversation_jid", ColumnKind::Text, true),
    ColumnContract::plain("last_active_at_ms", ColumnKind::Bigint, true),
    ColumnContract::plain("last_chat_state", ColumnKind::Text, false),
    ColumnContract::plain("last_read_at_ms", ColumnKind::Bigint, false),
    ColumnContract::plain("presence_show", ColumnKind::Text, false),
    ColumnContract::plain("created_at_ms", ColumnKind::Bigint, true),
    ColumnContract::plain("updated_at_ms", ColumnKind::Bigint, true),
];

const ACTIVITY_INDEXES: &[IndexContract] = &[IndexContract {
    name: "idx_notification_activity_conversation_active",
    unique: false,
    columns: &["conversation_jid", "last_active_at_ms"],
}];

const ACTIVITY_CHECKS: &[CheckContract] = &[
    CheckContract {
        name: super::schema::NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_CHECK_NAME,
        column: "last_chat_state",
        nullable: true,
        values: &super::schema::NOTIFICATION_ACTIVITY_LAST_CHAT_STATE_VALUES,
    },
    CheckContract {
        name: super::schema::NOTIFICATION_ACTIVITY_PRESENCE_SHOW_CHECK_NAME,
        column: "presence_show",
        nullable: true,
        values: &super::schema::NOTIFICATION_ACTIVITY_PRESENCE_SHOW_VALUES,
    },
];

const ACTIVITY_OWNER_STATE_COLUMNS: &[ColumnContract] = &[
    ColumnContract::plain("singleton", ColumnKind::Text, true),
    ColumnContract::plain("prepared_revision", ColumnKind::Text, true),
    ColumnContract::plain("system_identifier", ColumnKind::Text, true),
    ColumnContract::plain("database_oid", ColumnKind::Bigint, true),
    ColumnContract::plain("database_name", ColumnKind::Text, true),
    ColumnContract::plain("schema_oid", ColumnKind::Bigint, true),
    ColumnContract::plain("schema_name", ColumnKind::Text, true),
];

const ACTIVITY_OWNER_STATE_CHECKS: &[CheckContract] = &[
    CheckContract {
        name: ACTIVITY_OWNER_STATE_SINGLETON_CHECK,
        column: "singleton",
        nullable: false,
        values: &["1"],
    },
    CheckContract {
        name: ACTIVITY_OWNER_STATE_REVISION_CHECK,
        column: "prepared_revision",
        nullable: false,
        values: &["0", "1"],
    },
];

pub(crate) const ACTIVITY_CONTRACT: TableContract = TableContract {
    name: "notification_activity",
    columns: ACTIVITY_COLUMNS,
    primary_key: &["owner_bare_jid", "conversation_jid"],
    indexes: ACTIVITY_INDEXES,
    checks: ACTIVITY_CHECKS,
    runtime_delete: false,
};

const ACTIVITY_OWNER_STATE_CONTRACT: TableContract = TableContract {
    name: ACTIVITY_OWNER_STATE_TABLE,
    columns: ACTIVITY_OWNER_STATE_COLUMNS,
    primary_key: &["singleton"],
    indexes: &[],
    checks: ACTIVITY_OWNER_STATE_CHECKS,
    runtime_delete: false,
};

const PUSH_REGISTRATION_COLUMNS: &[ColumnContract] = &[
    ColumnContract::plain("owner_bare_jid", ColumnKind::Text, true),
    ColumnContract::plain("push_service_jid", ColumnKind::Text, true),
    ColumnContract::empty_text_default("node", true),
    ColumnContract::plain("publish_options_xml", ColumnKind::Text, false),
    ColumnContract::plain("status", ColumnKind::Text, true),
    ColumnContract::zero_integer_default("failure_count", true),
    ColumnContract::plain("last_error", ColumnKind::Text, false),
    ColumnContract::plain("next_retry_at_ms", ColumnKind::Bigint, false),
    ColumnContract::plain("created_at_ms", ColumnKind::Bigint, true),
    ColumnContract::plain("updated_at_ms", ColumnKind::Bigint, true),
];

const PUSH_REGISTRATION_INDEXES: &[IndexContract] = &[IndexContract {
    name: "idx_push_registrations_owner_status",
    unique: false,
    columns: &["owner_bare_jid", "status"],
}];

const PUSH_REGISTRATION_CHECKS: &[CheckContract] = &[CheckContract {
    name: "push_registrations_status_check",
    column: "status",
    nullable: false,
    values: &["enabled", "disabled", "backoff"],
}];

pub(crate) const PUSH_REGISTRATIONS_CONTRACT: TableContract = TableContract {
    name: "push_registrations",
    columns: PUSH_REGISTRATION_COLUMNS,
    primary_key: &["owner_bare_jid", "push_service_jid", "node"],
    indexes: PUSH_REGISTRATION_INDEXES,
    checks: PUSH_REGISTRATION_CHECKS,
    runtime_delete: true,
};

/// Validate one owner-created table on the exact attested PostgreSQL main
/// pool. The transaction is read-only and bounded, and its qualified
/// `SHARE UPDATE EXCLUSIVE` relation lock is acquired before the first catalog
/// snapshot. This blocks schema changes that could add triggers or foreign
/// keys during validation while allowing ordinary row writers to continue.
pub(crate) async fn verify_attested_table(
    db: &Database,
    contract: &TableContract,
) -> Result<(), DurableStoreAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(DurableStoreAttachError::PostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(DurableStoreAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(DurableStoreAttachError::AttestedPoolRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;

    let identity = capability.identity();
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "notification-store schema")
        .map_err(|_| DurableStoreAttachError::InvalidSchema)?;
    let activity_contract = contract.name == ACTIVITY_CONTRACT.name;
    let qualified = format!(
        "{}.\"{}\"",
        crate::db::physical_attestation::quote_identifier(schema),
        contract.name
    );

    let mut tx = db.begin().await?;
    if activity_contract {
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            (),
        )
        .await?;
    } else {
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
    }
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|error| match error {
            crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                DurableStoreAttachError::InvalidTimeoutSetting { setting }
            }
            crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                DurableStoreAttachError::Database(error)
            }
        })?;

    if activity_contract {
        let mut rows = tx
            .query(
                "WITH acquired AS MATERIALIZED (\
                   SELECT pg_catalog.pg_advisory_xact_lock_shared(?::integer, ?::integer)\
                 ) SELECT true FROM acquired",
                crate::db_params![ACTIVITY_SCHEMA_GATE.0, ACTIVITY_SCHEMA_GATE.1],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(DurableStoreAttachError::Database(
                DatabaseError::QueryFailed(
                    "notification activity shared schema gate returned no row".to_owned(),
                ),
            ));
        };
        if !row.get::<bool>(0)? {
            return Err(DurableStoreAttachError::Database(
                DatabaseError::QueryFailed(
                    "notification activity shared schema gate was not acquired".to_owned(),
                ),
            ));
        }
        drop(rows);
    }
    tx.execute(
        &format!("LOCK TABLE {qualified} IN SHARE UPDATE EXCLUSIVE MODE"),
        (),
    )
    .await
    .map_err(|source| {
        if activity_contract {
            DurableStoreAttachError::RelationLock {
                table: contract.name,
                source,
            }
        } else {
            DurableStoreAttachError::Relation {
                table: contract.name,
                source,
            }
        }
    })?;
    if activity_contract {
        let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
        let marker_lock = format!(
            "LOCK TABLE {quoted_schema}.\"{ACTIVITY_OWNER_STATE_TABLE}\" IN ACCESS SHARE MODE"
        );
        tx.execute(&marker_lock, ()).await.map_err(|source| {
            if is_undefined_relation(&source) {
                DurableStoreAttachError::OwnerPreparationIncomplete
            } else {
                DurableStoreAttachError::RelationLock {
                    table: ACTIVITY_OWNER_STATE_TABLE,
                    source,
                }
            }
        })?;
    }

    // The attestation covers the same connection that holds the qualified
    // relation locks and performs every catalog inspection below. READ
    // COMMITTED gives the first post-lock catalog query a fresh snapshot.
    capability.attest_transaction(&mut tx).await?;
    verify_active_schema(&mut tx, schema, i64::from(identity.schema.oid)).await?;
    verify_table(&mut tx, schema, contract).await?;
    verify_columns(&mut tx, schema, contract).await?;
    verify_constraints(&mut tx, schema, contract).await?;
    verify_indexes(&mut tx, schema, contract).await?;
    verify_runtime_acl(&mut tx, schema, contract.name, contract.runtime_delete).await?;
    if activity_contract {
        verify_table(&mut tx, schema, &ACTIVITY_OWNER_STATE_CONTRACT)
            .await
            .map_err(|error| match error {
                DurableStoreAttachError::InvalidTable { .. }
                | DurableStoreAttachError::InboundForeignKey { .. }
                | DurableStoreAttachError::UnexpectedTrigger { .. } => {
                    DurableStoreAttachError::InvalidOwnerMarker
                }
                error => error,
            })?;
        verify_columns(&mut tx, schema, &ACTIVITY_OWNER_STATE_CONTRACT)
            .await
            .map_err(|_| DurableStoreAttachError::InvalidOwnerMarker)?;
        verify_constraints(&mut tx, schema, &ACTIVITY_OWNER_STATE_CONTRACT)
            .await
            .map_err(|_| DurableStoreAttachError::InvalidOwnerMarker)?;
        verify_indexes(&mut tx, schema, &ACTIVITY_OWNER_STATE_CONTRACT)
            .await
            .map_err(|error| match error {
                DurableStoreAttachError::InvalidTable { .. }
                | DurableStoreAttachError::InvalidIndex { .. }
                | DurableStoreAttachError::UnexpectedIndex { .. } => {
                    DurableStoreAttachError::InvalidOwnerMarker
                }
                error => error,
            })?;
        verify_runtime_owner_state_acl(&mut tx, schema).await?;
        verify_owner_state(&mut tx, schema, identity).await?;
    }

    tx.rollback().await?;
    Ok(())
}

async fn verify_active_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: i64,
) -> Result<(), DurableStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.current_schema()::text, n.oid::bigint \
             FROM pg_catalog.pg_namespace n WHERE n.nspname = ?",
            crate::db_params![schema],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(DurableStoreAttachError::ActiveSchemaMismatch);
    };
    let active: Option<String> = row.get(0)?;
    let actual_oid: i64 = row.get(1)?;
    if active.as_deref() != Some(schema) || actual_oid != schema_oid {
        return Err(DurableStoreAttachError::ActiveSchemaMismatch);
    }
    Ok(())
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), DurableStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' \
                    AND NOT c.relispartition AND c.reloftype = 0 \
                    AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                                    WHERE rw.ev_class = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p \
                                    WHERE p.polrelid = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                                    WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid), \
                    NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint fk \
                                WHERE fk.contype = 'f' AND fk.confrelid = c.oid), \
                    NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                                WHERE tr.tgrelid = c.oid) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, contract.name],
        )
        .await
        .map_err(|source| DurableStoreAttachError::Relation {
            table: contract.name,
            source,
        })?;
    let Some(row) = rows.next().await? else {
        return Err(DurableStoreAttachError::InvalidTable {
            table: contract.name,
        });
    };
    if !row.get::<bool>(0)? {
        return Err(DurableStoreAttachError::InvalidTable {
            table: contract.name,
        });
    }
    if !row.get::<bool>(1)? {
        return Err(DurableStoreAttachError::InboundForeignKey {
            table: contract.name,
        });
    }
    if !row.get::<bool>(2)? {
        return Err(DurableStoreAttachError::UnexpectedTrigger {
            table: contract.name,
        });
    }
    Ok(())
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), DurableStoreAttachError> {
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
             WHERE n.nspname = ? AND c.relname = ? AND c.relkind = 'r' \
               AND a.attnum > 0 AND NOT a.attisdropped",
            crate::db_params![schema, contract.name],
        )
        .await
        .map_err(|source| DurableStoreAttachError::Relation {
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
                row.get::<bool>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
                row.get::<Option<String>>(6)?,
            ),
        );
    }
    if actual.len() != contract.columns.len() {
        return Err(DurableStoreAttachError::UnexpectedColumn {
            table: contract.name,
        });
    }
    for column in contract.columns {
        let Some((kind, not_null, collation_matches, generated, identity, default)) =
            actual.get(column.name)
        else {
            return Err(DurableStoreAttachError::InvalidColumn {
                table: contract.name,
                column: column.name,
            });
        };
        let expected_kind = match column.kind {
            ColumnKind::Text => "text",
            ColumnKind::Integer => "integer",
            ColumnKind::Bigint => "bigint",
        };
        let default_matches = match column.default {
            ColumnDefault::None => default.is_none(),
            ColumnDefault::EmptyText => default.as_deref() == Some("''::text"),
            ColumnDefault::ZeroInteger => default.as_deref() == Some("0"),
        };
        if kind != expected_kind
            || *not_null != column.not_null
            || !collation_matches
            || !generated.is_empty()
            || !identity.is_empty()
            || !default_matches
        {
            return Err(DurableStoreAttachError::InvalidColumn {
                table: contract.name,
                column: column.name,
            });
        }
    }
    Ok(())
}

async fn verify_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), DurableStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.conname::text, c.contype::text, c.condeferrable, c.condeferred, \
                    c.convalidated, pg_catalog.pg_get_constraintdef(c.oid) \
             FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = ?",
            crate::db_params![schema, contract.name],
        )
        .await
        .map_err(|source| DurableStoreAttachError::Relation {
            table: contract.name,
            source,
        })?;
    let mut constraints = HashMap::new();
    while let Some(row) = rows.next().await? {
        constraints.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<bool>(3)?,
                row.get::<bool>(4)?,
                row.get::<String>(5)?,
            ),
        );
    }
    if constraints.len() != 1 + contract.checks.len() {
        return Err(DurableStoreAttachError::InvalidConstraint {
            table: contract.name,
        });
    }
    let primary = constraints.values().find(|(kind, _, _, _, _)| kind == "p");
    if !matches!(primary, Some((_, false, false, true, _))) {
        return Err(DurableStoreAttachError::InvalidPrimaryKey {
            table: contract.name,
        });
    }
    for check in contract.checks {
        let Some((kind, deferrable, deferred, validated, definition)) = constraints.get(check.name)
        else {
            return Err(DurableStoreAttachError::InvalidConstraint {
                table: contract.name,
            });
        };
        if kind != "c"
            || *deferrable
            || *deferred
            || !*validated
            || normalize_check_definition(definition) != expected_check_definition(check)
        {
            return Err(DurableStoreAttachError::InvalidConstraint {
                table: contract.name,
            });
        }
    }
    verify_primary_key_columns(tx, schema, contract).await
}

fn normalize_check_definition(definition: &str) -> String {
    definition
        .to_ascii_lowercase()
        .replace("::text[]", "")
        .replace("::text", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '(' && *character != ')')
        .collect::<String>()
        .trim_start_matches("check")
        .to_owned()
}

fn expected_check_definition(check: &CheckContract) -> String {
    // PostgreSQL rewrites an `IN` list with one member to a simple equality
    // expression in `pg_get_constraintdef`; keep the catalog contract exact
    // while matching that canonical rendering.
    if check.values.len() == 1 {
        let value = check.values[0];
        if check.nullable {
            return format!("{}isnullor{}='{}'", check.column, check.column, value);
        }
        return format!("{}='{}'", check.column, value);
    }

    let values = check
        .values
        .iter()
        .map(|value| format!("'{value}'"))
        .collect::<Vec<_>>()
        .join(",");
    if check.nullable {
        format!(
            "{}isnullor{}=anyarray[{}]",
            check.column, check.column, values
        )
    } else {
        format!("{}=anyarray[{}]", check.column, values)
    }
}

async fn verify_primary_key_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), DurableStoreAttachError> {
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
                             AND i.indisready AND i.indislive AND i.indimmediate) \
               AND n.nspname = ? AND t.relname = ? ORDER BY k.position",
            crate::db_params![schema, contract.name],
        )
        .await
        .map_err(|source| DurableStoreAttachError::Relation {
            table: contract.name,
            source,
        })?;
    let mut actual = Vec::new();
    while let Some(row) = rows.next().await? {
        actual.push(row.get::<String>(0)?);
    }
    if actual.iter().map(String::as_str).collect::<Vec<_>>() != contract.primary_key {
        return Err(DurableStoreAttachError::InvalidPrimaryKey {
            table: contract.name,
        });
    }
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), DurableStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.count(*)::bigint FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = ?",
            crate::db_params![schema, contract.name],
        )
        .await
        .map_err(|source| DurableStoreAttachError::Relation {
            table: contract.name,
            source,
        })?;
    let index_count: i64 = rows
        .next()
        .await?
        .ok_or(DurableStoreAttachError::InvalidTable {
            table: contract.name,
        })?
        .get(0)?;
    drop(rows);
    if index_count != contract.indexes.len() as i64 + 1 {
        return Err(DurableStoreAttachError::UnexpectedIndex {
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
            .map_err(|source| DurableStoreAttachError::Relation {
                table: contract.name,
                source,
            })?;
        let Some(row) = rows.next().await? else {
            return Err(DurableStoreAttachError::InvalidIndex {
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
        let ascending: bool = row.get(11)?;
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
            || !ascending
        {
            return Err(DurableStoreAttachError::InvalidIndex {
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
    allow_delete: bool,
) -> Result<(), DurableStoreAttachError> {
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
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                                    WHERE m.member = runtime.oid OR m.roleid = runtime.oid)
                    AND pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'USAGE')
                    AND NOT pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'CREATE')
                    AND ns.nspowner = owner.oid AND relation.relowner = owner.oid
                    AND owner.oid <> runtime.oid AND owner.oid <> database.datdba
                    AND NOT owner.rolcanlogin AND NOT owner.rolsuper
                    AND NOT owner.rolcreaterole AND NOT owner.rolcreatedb
                    AND NOT owner.rolreplication AND NOT owner.rolbypassrls
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
                    AND ((? <> 0 AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'DELETE'))
                      OR (? = 0 AND NOT pg_catalog.has_table_privilege(
                        runtime.oid, relation.oid, 'DELETE')))
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
                            ('SELECT', 'INSERT', 'UPDATE', 'DELETE')
                          OR (? = 0 AND acl.privilege_type = 'DELETE')))
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
            crate::db_params![
                i64::from(allow_delete),
                i64::from(allow_delete),
                i64::from(allow_delete),
                schema,
                table,
            ],
        )
        .await
        .map_err(|source| DurableStoreAttachError::Relation { table, source })?;
    let safe = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !safe {
        return Err(DurableStoreAttachError::UnsafeRuntimeRole);
    }
    Ok(())
}

async fn verify_runtime_owner_state_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), DurableStoreAttachError> {
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
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                                    WHERE m.member = runtime.oid OR m.roleid = runtime.oid)
                    AND pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'USAGE')
                    AND NOT pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'CREATE')
                    AND ns.nspowner = owner.oid AND relation.relowner = owner.oid
                    AND owner.oid <> runtime.oid AND owner.oid <> database.datdba
                    AND NOT owner.rolcanlogin AND NOT owner.rolsuper
                    AND NOT owner.rolcreaterole AND NOT owner.rolcreatedb
                    AND NOT owner.rolreplication AND NOT owner.rolbypassrls
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
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid,
                      'INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid,
                      'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION,
                       UPDATE WITH GRANT OPTION, DELETE WITH GRANT OPTION,
                       TRUNCATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION,
                       TRIGGER WITH GRANT OPTION')
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
                          acl.is_grantable OR acl.privilege_type <> 'SELECT'))
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
            crate::db_params![schema, ACTIVITY_OWNER_STATE_TABLE],
        )
        .await
        .map_err(|source| DurableStoreAttachError::Relation {
            table: ACTIVITY_OWNER_STATE_TABLE,
            source,
        })?;
    let safe = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !safe {
        return Err(DurableStoreAttachError::UnsafeRuntimeRole);
    }
    Ok(())
}

async fn verify_owner_state(
    tx: &mut Transaction<'_>,
    schema: &str,
    identity: &PgIdentity,
) -> Result<(), DurableStoreAttachError> {
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    let marker = format!("{quoted_schema}.\"{ACTIVITY_OWNER_STATE_TABLE}\"");
    let mut rows = tx
        .query(
            &format!(
                "SELECT prepared_revision, system_identifier, database_oid, database_name, \
                 schema_oid, schema_name FROM {marker} WHERE singleton = '1'"
            ),
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(DurableStoreAttachError::OwnerPreparationIncomplete);
    };
    let revision: String = row.get(0)?;
    let marker_identity_matches = row.get::<String>(1)? == identity.system_identifier.to_string()
        && row.get::<i64>(2)? == i64::from(identity.database.oid)
        && row.get::<String>(3)? == identity.database.name.0
        && row.get::<i64>(4)? == i64::from(identity.schema.oid)
        && row.get::<String>(5)? == identity.schema.name.0;
    if !marker_identity_matches {
        return Err(DurableStoreAttachError::OwnerIdentityMismatch);
    }
    if revision != ACTIVITY_OWNER_STATE_REVISION {
        return Err(DurableStoreAttachError::OwnerPreparationIncomplete);
    }
    Ok(())
}

fn is_undefined_relation(error: &DatabaseError) -> bool {
    matches!(
        error,
        DatabaseError::Internal(sqlx::Error::Database(database_error))
            if database_error.code().as_deref() == Some("42P01")
    )
}
