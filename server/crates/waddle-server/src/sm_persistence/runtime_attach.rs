//! Read-only structural validation of an owner-prepared portable SM schema.

use std::collections::HashMap;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

#[derive(Debug, thiserror::Error)]
pub enum SmRuntimeAttachError {
    #[error("portable SM runtime attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("portable SM runtime attachment requires the main database admission lane")]
    MainLaneRequired,
    #[error("portable SM runtime attachment requires a physically attested PostgreSQL pool")]
    AttestedPostgresPoolRequired,
    #[error("the active PostgreSQL schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not verify the PostgreSQL {setting} bound for SM attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("failed to inspect SM relation {relation}: {source}")]
    Relation {
        relation: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("SM relation {table} is not an ordinary table in the attested schema")]
    InvalidTable { table: &'static str },
    #[error("SM relation {table} has an invalid column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("SM relation {table} has an invalid primary key")]
    InvalidPrimaryKey { table: &'static str },
    #[error("SM ingress disposition constraint is missing or invalid")]
    InvalidDispositionConstraint,
    #[error("SM relation {table} is missing a CHECK constraint or has an invalid one")]
    InvalidCheckConstraint { table: &'static str },
    #[error("SM index {index} is missing or has an invalid definition")]
    InvalidIndex { index: &'static str },
    #[error("SM runtime role or privileges are unsafe at {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error("SM relation {component} contains unexpected triggers or rules")]
    UnexpectedExecutableSchema { component: &'static str },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

struct Column {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default: Option<&'static str>,
}

const fn column(
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default: Option<&'static str>,
) -> Column {
    Column {
        name,
        kind,
        not_null,
        default,
    }
}

// The profile is derived from the portable owner initializer and the V1012,
// V1019 and V1027 PostgreSQL migrations. Owner bootstrap widens the older i64
// columns and removes shadow_ordinal before this snapshot may be attached.
const SESSIONS: &[Column] = &[
    column("stream_id", "text", true, None),
    column("user_id", "text", true, None),
    column("full_jid", "text", true, None),
    column("occupancy_session", "text", false, None),
    column("inbound_count", "bigint", true, None),
    column("outbound_count", "bigint", true, None),
    column("last_acked", "bigint", true, None),
    column("max_resume_secs", "bigint", false, None),
    column("detached_at_ms", "bigint", true, None),
    column("max_resume_duration_ms", "bigint", true, None),
    column("carbons_enabled", "integer", true, None),
    column("roster_interested", "integer", true, None),
    column("blocklist_interested", "integer", true, Some("0")),
    column("presence_available", "integer", true, None),
    column("presence_show", "text", false, None),
    column("presence_status", "text", false, None),
    column("presence_priority", "integer", true, None),
    column("replay_gap_through", "bigint", false, None),
    column("promotion_attempts", "integer", true, Some("0")),
    column("presence_payloads", "text", false, None),
    column("bare_jid", "text", false, None),
    column("auth_context_id", "text", false, None),
    column("auth_context_version", "bigint", false, None),
    column("principal_auth_epoch", "bigint", false, None),
];
const UNACKED: &[Column] = &[
    column("stream_id", "text", true, None),
    column("sequence", "bigint", true, None),
    column("stanza_xml", "text", true, None),
    column("original_receipt_at_ms", "bigint", true, None),
    column("ingress_receipts", "bytea", false, None),
    column("provenance", "bytea", false, None),
    column("origin_stream_id", "text", false, None),
    column("inbound_seq", "bigint", false, None),
    column("pair_sequence", "bigint", false, None),
];
const WINDOWS: &[Column] = &[
    column("stream_id", "text", true, None),
    column("from_exclusive", "bigint", true, None),
    column("to_inclusive", "bigint", true, None),
];
const APPENDS: &[Column] = &[
    column("message_key", "text", true, None),
    column("receipt_kind", "integer", true, None),
    column("semantic_identity_hash", "bytea", true, None),
    column("resource", "text", true, None),
    column("accepting_stream_id", "text", true, None),
    column("sequence", "bigint", true, None),
    column("appended_at_ms", "bigint", true, None),
    column("custody_payload", "text", true, None),
    column("original_receipt_at_ms", "bigint", true, None),
    column("disposition", "integer", true, None),
];

struct Table {
    name: &'static str,
    columns: &'static [Column],
    primary_key: &'static [&'static str],
}

const TABLES: &[Table] = &[
    Table {
        name: "sm_sessions",
        columns: SESSIONS,
        primary_key: &["stream_id"],
    },
    Table {
        name: "sm_unacked",
        columns: UNACKED,
        primary_key: &["stream_id", "sequence"],
    },
    Table {
        name: "sm_acknowledged_windows",
        columns: WINDOWS,
        primary_key: &["stream_id", "from_exclusive", "to_inclusive"],
    },
    Table {
        name: "sm_ingress_appends",
        columns: APPENDS,
        primary_key: &[
            "message_key",
            "receipt_kind",
            "semantic_identity_hash",
            "resource",
        ],
    },
];

struct Index {
    table: &'static str,
    name: &'static str,
    unique: bool,
    definition: &'static str,
}

const INDEXES: &[Index] = &[
    Index { table: "sm_sessions", name: "idx_sm_sessions_detached", unique: false, definition: "USING btree (detached_at_ms)" },
    Index { table: "sm_unacked", name: "idx_sm_unacked_dedup", unique: true, definition: "USING btree (stream_id, origin_stream_id, inbound_seq) WHERE ((origin_stream_id IS NOT NULL) AND (inbound_seq IS NOT NULL))" },
    Index { table: "sm_ingress_appends", name: "idx_sm_ingress_appends_stream", unique: false, definition: "USING btree (accepting_stream_id)" },
    Index { table: "sm_ingress_appends", name: "idx_sm_ingress_appends_pending", unique: false, definition: "USING btree (disposition, appended_at_ms)" },
];

/// A validated CHECK constraint, as `pg_get_constraintdef` prints it.
struct Check {
    table: &'static str,
    definition: &'static str,
}

/// Which runtime ACL contract a profile's relations are checked against.
#[derive(Clone, Copy)]
enum AclFamily {
    Portable,
    Durable,
}

/// One store's relations, indexes, constraints and ACL contract.
struct Profile {
    tables: &'static [Table],
    indexes: &'static [Index],
    checks: &'static [Check],
    acl: AclFamily,
}

const PORTABLE: Profile = Profile {
    tables: TABLES,
    indexes: INDEXES,
    checks: &[Check {
        table: "sm_ingress_appends",
        definition: "CHECK ((disposition = ANY (ARRAY[0, 1, 2, 3])))",
    }],
    acl: AclFamily::Portable,
};

// The profile is derived from `crate::sm_authority`'s owner initializer.
const DURABLE_SESSIONS: &[Column] = &[
    column("session_id", "text", true, None),
    column("resume_token_sha256", "text", true, None),
    column("full_jid", "text", true, None),
    column("state", "integer", true, None),
    column("terminal_reason", "integer", false, None),
    column("generation", "bigint", true, None),
    column("binding", "text", true, None),
    column("bare_jid", "text", true, None),
    column("auth_context_id", "text", true, None),
    column("auth_context_version", "bigint", true, None),
    column("principal_auth_epoch", "bigint", true, None),
    column("owner_node_id", "text", false, None),
    column("owner_node_epoch", "text", false, None),
    column("claim_epoch", "bigint", false, None),
    column("inbound_handled", "bigint", true, None),
    column("outbound_acked", "bigint", true, None),
    column("outbound_exposure_ceiling", "bigint", true, None),
    column("outbound_appended", "bigint", true, None),
    column("outbound_unacked_bytes", "bigint", true, None),
    column("max_resume_secs", "bigint", true, None),
    column("detached_at_ms", "bigint", false, None),
    column("enable_operation", "text", true, None),
    column("last_operation", "text", true, None),
    column("updated_at_ms", "bigint", true, None),
];
const DURABLE_OPERATIONS: &[Column] = &[
    column("operation_id", "text", true, None),
    column("session_id", "text", true, None),
    column("kind", "integer", true, None),
    column("outcome", "integer", true, None),
    column("generation", "bigint", true, None),
    column("ordinal", "bigint", false, None),
    column("recorded_at_ms", "bigint", true, None),
];

/// The durable session authority's own relations. It also writes
/// `sm_sessions` and `sm_unacked`, which the portable profile covers.
const DURABLE: Profile = Profile {
    tables: &[
        Table {
            name: "sm_durable_sessions",
            columns: DURABLE_SESSIONS,
            primary_key: &["session_id"],
        },
        Table {
            name: "sm_durable_operations",
            columns: DURABLE_OPERATIONS,
            primary_key: &["operation_id"],
        },
    ],
    indexes: &[
        Index {
            table: "sm_durable_sessions",
            name: "idx_sm_durable_sessions_token",
            unique: true,
            definition: "USING btree (resume_token_sha256)",
        },
        Index {
            table: "sm_durable_sessions",
            name: "idx_sm_durable_sessions_detached",
            unique: false,
            definition: "USING btree (state, detached_at_ms)",
        },
        Index {
            table: "sm_durable_sessions",
            name: "idx_sm_durable_sessions_resource",
            unique: false,
            definition: "USING btree (full_jid)",
        },
        Index {
            table: "sm_durable_operations",
            name: "idx_sm_durable_operations_session",
            unique: false,
            definition: "USING btree (session_id, kind, ordinal)",
        },
    ],
    checks: &[
        Check {
            table: "sm_durable_sessions",
            definition: "CHECK ((state = ANY (ARRAY[1, 2, 3, 4])))",
        },
        Check {
            table: "sm_durable_operations",
            definition: "CHECK ((kind = ANY (ARRAY[1, 2, 3])))",
        },
        Check {
            table: "sm_durable_operations",
            definition: "CHECK ((outcome = ANY (ARRAY[1, 2])))",
        },
    ],
    acl: AclFamily::Durable,
};

pub(super) async fn verify(
    db: &Database,
    require_runtime_writes: bool,
) -> Result<(), SmRuntimeAttachError> {
    verify_profile(db, &PORTABLE, require_runtime_writes).await
}

/// Verify the durable session authority's relations and the runtime role's
/// exact privileges on them, together with the portable relations it writes.
pub(super) async fn verify_durable(db: &Database) -> Result<(), SmRuntimeAttachError> {
    verify_profile(db, &PORTABLE, true).await?;
    verify_profile(db, &DURABLE, true).await
}

async fn verify_profile(
    db: &Database,
    profile: &Profile,
    require_runtime_writes: bool,
) -> Result<(), SmRuntimeAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(SmRuntimeAttachError::PostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(SmRuntimeAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(SmRuntimeAttachError::AttestedPostgresPoolRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;

    let mut tx = db.begin().await?;
    // Detect an inherited snapshot before establishing the bounded read-only
    // snapshot used for physical attestation and all subsequent inspections.
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
                SmRuntimeAttachError::InvalidTimeoutSetting { setting }
            }
            crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                SmRuntimeAttachError::Database(error)
            }
        })?;
    let expected = &capability.identity().schema;
    let schema = &expected.name.0;
    let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
    // The checkout hook has already attested this physical session. Lock all
    // qualified relations before the first snapshot-forming SELECT: an owner
    // can otherwise commit DDL after physical attestation pins the RR snapshot
    // but before a table is locked, leaving pg_catalog reads stale. LOCK and
    // the SHOW/SET LOCAL timeout setup do not establish an RR snapshot.
    for table in profile.tables {
        tx.execute(
            &format!(
                "LOCK TABLE {quoted}.\"{}\" IN ACCESS SHARE MODE",
                table.name
            ),
            (),
        )
        .await
        .map_err(|source| SmRuntimeAttachError::Relation {
            relation: table.name,
            source,
        })?;
    }
    capability.attest_transaction(&mut tx).await?;
    let mut rows = tx.query(
        "SELECT n.nspname::text, n.oid::bigint FROM pg_catalog.pg_namespace n WHERE n.nspname = pg_catalog.current_schema()",
        (),
    ).await?;
    let active = rows.next().await?;
    let valid = if let Some(row) = active {
        row.get::<String>(0)? == expected.name.0 && row.get::<i64>(1)? == i64::from(expected.oid)
    } else {
        false
    };
    drop(rows);
    if !valid {
        return Err(SmRuntimeAttachError::ActiveSchemaMismatch);
    }
    // Check SELECT access on each locked relation before inspecting its
    // columns and constraints.
    for table in profile.tables {
        tx.query(
            &format!("SELECT 1 FROM {quoted}.\"{}\" WHERE false", table.name),
            (),
        )
        .await
        .map_err(|source| SmRuntimeAttachError::Relation {
            relation: table.name,
            source,
        })?;
    }
    for table in profile.tables {
        verify_table(&mut tx, schema, &quoted, table).await?;
    }
    verify_checks(&mut tx, schema, profile.checks).await?;
    verify_indexes(&mut tx, schema, profile.indexes).await?;
    let unexpected_schema = match (require_runtime_writes, profile.acl) {
        (false, _) => None,
        (true, AclFamily::Portable) => {
            crate::db::runtime_store_acl::unexpected_sm_executable_schema(&mut tx, schema).await?
        }
        (true, AclFamily::Durable) => {
            crate::db::runtime_store_acl::unexpected_durable_sm_executable_schema(&mut tx, schema)
                .await?
        }
    };
    if let Some(component) = unexpected_schema {
        return Err(SmRuntimeAttachError::UnexpectedExecutableSchema { component });
    }
    let privilege_failure = match (require_runtime_writes, profile.acl) {
        (false, _) => None,
        (true, AclFamily::Portable) => {
            crate::db::runtime_store_acl::verify_sm(&mut tx, schema).await?
        }
        (true, AclFamily::Durable) => {
            crate::db::runtime_store_acl::verify_durable_sm(&mut tx, schema).await?
        }
    };
    if let Some(component) = privilege_failure {
        return Err(SmRuntimeAttachError::UnsafeRuntimePrivileges { component });
    }
    tx.commit().await?;
    Ok(())
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    quoted_schema: &str,
    table: &Table,
) -> Result<(), SmRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT 1 FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace WHERE n.nspname = ? AND t.relname = ? AND t.relkind = 'r'",
        crate::db_params![schema, table.name],
    ).await?;
    if rows.next().await?.is_none() {
        return Err(SmRuntimeAttachError::InvalidTable { table: table.name });
    }
    drop(rows);
    let mut rows = tx.query(
        "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, pg_catalog.pg_get_expr(d.adbin, d.adrelid), a.attgenerated::text, a.attidentity::text \
         FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class t ON t.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
         WHERE n.nspname = ? AND t.relname = ? AND a.attnum > 0 AND NOT a.attisdropped",
        crate::db_params![schema, table.name],
    ).await?;
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
    if table.name == "sm_sessions" && columns.contains_key("shadow_ordinal") {
        return Err(SmRuntimeAttachError::InvalidColumn {
            table: table.name,
            column: "shadow_ordinal",
        });
    }
    for column in table.columns {
        let Some((kind, not_null, default, generated, identity)) = columns.get(column.name) else {
            return Err(SmRuntimeAttachError::InvalidColumn {
                table: table.name,
                column: column.name,
            });
        };
        if kind != column.kind
            || *not_null != column.not_null
            || default.as_deref() != column.default
            || !generated.is_empty()
            || !identity.is_empty()
        {
            return Err(SmRuntimeAttachError::InvalidColumn {
                table: table.name,
                column: column.name,
            });
        }
    }
    let mut rows = tx.query(
        "SELECT a.attname::text FROM pg_catalog.pg_constraint c \
         JOIN pg_catalog.pg_class t ON t.oid = c.conrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         CROSS JOIN LATERAL pg_catalog.unnest(c.conkey) WITH ORDINALITY k(attnum, position) \
         JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
         WHERE c.contype = 'p' AND NOT c.condeferrable AND c.convalidated \
           AND EXISTS (SELECT 1 FROM pg_catalog.pg_index pi WHERE pi.indexrelid = c.conindid AND pi.indisvalid AND pi.indisready AND pi.indislive) \
           AND n.nspname = ? AND t.relname = ? ORDER BY k.position",
        crate::db_params![schema, table.name],
    ).await?;
    let mut key = Vec::new();
    while let Some(row) = rows.next().await? {
        key.push(row.get::<String>(0)?);
    }
    drop(rows);
    if key.iter().map(String::as_str).collect::<Vec<_>>() != table.primary_key {
        return Err(SmRuntimeAttachError::InvalidPrimaryKey { table: table.name });
    }
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
    .map_err(|source| SmRuntimeAttachError::Relation {
        relation: table.name,
        source,
    })?;
    Ok(())
}

async fn verify_checks(
    tx: &mut Transaction<'_>,
    schema: &str,
    checks: &[Check],
) -> Result<(), SmRuntimeAttachError> {
    for check in checks {
        let mut rows = tx.query(
            "SELECT 1 FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = ? AND c.contype = 'c' AND c.convalidated \
               AND pg_catalog.pg_get_constraintdef(c.oid) = ?",
            crate::db_params![schema, check.table, check.definition],
        ).await?;
        if rows.next().await?.is_none() {
            return Err(if check.table == "sm_ingress_appends" {
                SmRuntimeAttachError::InvalidDispositionConstraint
            } else {
                SmRuntimeAttachError::InvalidCheckConstraint { table: check.table }
            });
        }
    }
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    indexes: &[Index],
) -> Result<(), SmRuntimeAttachError> {
    for index in indexes {
        let mut rows = tx.query(
            "SELECT x.indisunique, pg_catalog.pg_get_indexdef(x.indexrelid) \
             FROM pg_catalog.pg_index x JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid \
             JOIN pg_catalog.pg_class t ON t.oid = x.indrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = ? AND i.relname = ? AND i.relnamespace = n.oid \
               AND x.indisvalid AND x.indisready AND x.indislive",
            crate::db_params![schema, index.table, index.name],
        ).await?;
        let valid = if let Some(row) = rows.next().await? {
            let unique: bool = row.get(0)?;
            let definition: String = row.get(1)?;
            unique == index.unique
                && definition
                    .split_once(" USING ")
                    .is_some_and(|(_, suffix)| format!("USING {suffix}") == index.definition)
        } else {
            false
        };
        if !valid {
            return Err(SmRuntimeAttachError::InvalidIndex { index: index.name });
        }
    }
    Ok(())
}
