//! Read-only verification of the trusted-owner prepared call-teardown outbox.

use std::collections::{HashMap, HashSet};

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

const TABLE: &str = "call_teardown_outbox";

#[derive(Debug, thiserror::Error)]
pub enum CallTeardownRuntimeAttachError {
    #[error("call teardown attachment requires an attested PostgreSQL database")]
    AttestedPostgresRequired,
    #[error("call teardown attachment requires the main database lane")]
    MainLaneRequired,
    #[error("call teardown active schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not bound PostgreSQL waits for call teardown attachment")]
    ReadBounds,
    #[error("failed to lock call teardown relation: {0}")]
    Relation(#[source] DatabaseError),
    #[error("call teardown relation has an unexpected shape")]
    InvalidTable,
    #[error("call teardown relation has an invalid column {0}")]
    InvalidColumn(&'static str),
    #[error("call teardown relation has invalid constraints")]
    InvalidConstraints,
    #[error("call teardown relation has invalid indexes")]
    InvalidIndexes,
    #[error("call teardown runtime role or grants are unsafe")]
    UnsafeRuntimeRole,
    #[error("call teardown outbox runtime table grants are unsafe")]
    UnsafeRuntimePrivileges,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

struct Column {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default: Option<&'static str>,
}

const COLUMNS: &[Column] = &[
    Column {
        name: "intent_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "call_id",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "identity",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "room_jid",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "action",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "generation",
        kind: "integer",
        not_null: false,
        default: None,
    },
    Column {
        name: "occupant",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "unbound_occupant",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "room_sid",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "participant_sid",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "session_binding",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "thread_id",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "anchor_origin_id",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "thread_started_at_ms",
        kind: "bigint",
        not_null: false,
        default: None,
    },
    Column {
        name: "thread_ended_at_ms",
        kind: "bigint",
        not_null: false,
        default: None,
    },
    Column {
        name: "producing_node",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "status",
        kind: "text",
        not_null: true,
        default: None,
    },
    Column {
        name: "attempt_count",
        kind: "integer",
        not_null: true,
        default: Some("0"),
    },
    Column {
        name: "last_error",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "next_attempt_at_ms",
        kind: "bigint",
        not_null: false,
        default: None,
    },
    Column {
        name: "claimed_at_ms",
        kind: "bigint",
        not_null: false,
        default: None,
    },
    Column {
        name: "claim_token",
        kind: "text",
        not_null: false,
        default: None,
    },
    Column {
        name: "created_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
    Column {
        name: "updated_at_ms",
        kind: "bigint",
        not_null: true,
        default: None,
    },
];

const INDEXES: &[(&str, &str)] = &[
    (
        "idx_call_teardown_outbox_status_due",
        "status,next_attempt_at_ms,created_at_ms",
    ),
    ("idx_call_teardown_outbox_dedupe", "status,call_id,action"),
];

/// Verify the exact attested main pool, relation shape, constraints, indexes,
/// and restricted runtime role. The caller receives the same `Database` only
/// after this read-only transaction rolls back successfully.
pub(super) async fn verify(db: &Database) -> Result<(), CallTeardownRuntimeAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(CallTeardownRuntimeAttachError::AttestedPostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(CallTeardownRuntimeAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(CallTeardownRuntimeAttachError::AttestedPostgresRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;

    let identity = capability.identity();
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "call teardown schema")
        .map_err(|_| CallTeardownRuntimeAttachError::ActiveSchemaMismatch)?;
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    let mut tx = db.begin().await?;
    // Require a fresh, bounded read-only transaction. The repeatable-read
    // snapshot is first established only after the relation lock is acquired.
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
        (),
    )
    .await?;
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|_| CallTeardownRuntimeAttachError::ReadBounds)?;
    tx.execute(
        &format!("LOCK TABLE {quoted_schema}.{TABLE} IN ACCESS SHARE MODE"),
        (),
    )
    .await
    .map_err(CallTeardownRuntimeAttachError::Relation)?;

    capability.attest_transaction(&mut tx).await?;
    verify_active_schema(&mut tx, schema, i64::from(identity.schema.oid)).await?;
    verify_runtime_role(&mut tx, schema).await?;
    verify_table(&mut tx, schema).await?;
    verify_columns(&mut tx, schema).await?;
    verify_constraints(&mut tx, schema).await?;
    verify_indexes(&mut tx, schema).await?;
    verify_table_acl(&mut tx, schema).await?;

    tx.rollback().await?;
    Ok(())
}

/// Owner preparation checks the same structural contract in its write
/// transaction, without requiring the runtime login's restricted ACL.
pub(super) async fn verify_owner_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: i64,
) -> Result<(), CallTeardownRuntimeAttachError> {
    verify_active_schema(tx, schema, schema_oid).await?;
    verify_table(tx, schema).await?;
    verify_columns(tx, schema).await?;
    verify_constraints(tx, schema).await?;
    verify_indexes(tx, schema).await?;
    Ok(())
}

async fn verify_active_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: i64,
) -> Result<(), CallTeardownRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT n.oid::bigint = ? AND n.nspname = pg_catalog.current_schema() \
             AND COALESCE((pg_catalog.current_schemas(false))[1]::text, '') = n.nspname \
             AND pg_catalog.cardinality(pg_catalog.current_schemas(false)) <= 2 \
             AND (pg_catalog.cardinality(pg_catalog.current_schemas(false)) = 1 \
               OR pg_catalog.left((pg_catalog.current_schemas(false))[2]::text, 8) = 'pg_temp_') \
             FROM pg_catalog.pg_namespace n WHERE n.nspname = ?",
            crate::db_params![schema_oid, schema],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(CallTeardownRuntimeAttachError::ActiveSchemaMismatch);
    }
    Ok(())
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), CallTeardownRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' \
             AND NOT c.relispartition AND c.reloftype = 0 \
             AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
             AND c.relowner = n.nspowner \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h \
                             WHERE h.inhrelid = c.oid OR h.inhparent = c.oid) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class = c.oid) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr WHERE tr.tgrelid = c.oid) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint fk \
                             WHERE fk.contype = 'f' \
                               AND (fk.conrelid = c.oid OR fk.confrelid = c.oid)) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, TABLE],
        )
        .await
        .map_err(CallTeardownRuntimeAttachError::Relation)?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(CallTeardownRuntimeAttachError::InvalidTable);
    }
    Ok(())
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), CallTeardownRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
             a.attnotnull, a.attgenerated::text, a.attidentity::text, \
             pg_catalog.pg_get_expr(d.adbin, d.adrelid), typ.typcollation = a.attcollation \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_type typ ON typ.oid = a.atttypid \
             LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = ? AND c.relname = ? AND a.attnum > 0 AND NOT a.attisdropped",
            crate::db_params![schema, TABLE],
        )
        .await
        .map_err(CallTeardownRuntimeAttachError::Relation)?;
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
                row.get::<bool>(6)?,
            ),
        );
    }
    if actual.len() != COLUMNS.len() {
        return Err(CallTeardownRuntimeAttachError::InvalidTable);
    }
    for column in COLUMNS {
        let Some((kind, not_null, generated, identity, default, default_collation)) =
            actual.get(column.name)
        else {
            return Err(CallTeardownRuntimeAttachError::InvalidColumn(column.name));
        };
        if kind != column.kind
            || *not_null != column.not_null
            || !generated.is_empty()
            || !identity.is_empty()
            || default.as_deref() != column.default
            || !default_collation
        {
            return Err(CallTeardownRuntimeAttachError::InvalidColumn(column.name));
        }
    }
    Ok(())
}

async fn verify_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), CallTeardownRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT con.contype::text, con.condeferrable, con.condeferred, con.convalidated, \
             COALESCE(ix.indisvalid AND ix.indisready AND ix.indislive AND ix.indimmediate, false), \
             pg_catalog.array_to_string(ARRAY(SELECT a.attname::text \
               FROM pg_catalog.unnest(con.conkey) WITH ORDINALITY k(attnum,pos) \
               JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum \
               ORDER BY k.pos), ','), pg_catalog.pg_get_constraintdef(con.oid) \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             LEFT JOIN pg_catalog.pg_index ix ON ix.indexrelid = con.conindid \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, TABLE],
        )
        .await
        .map_err(CallTeardownRuntimeAttachError::Relation)?;
    let mut primary_key = false;
    let mut check_definitions = HashSet::new();
    let mut count = 0;
    while let Some(row) = rows.next().await? {
        count += 1;
        if row.get::<bool>(1)? || row.get::<bool>(2)? || !row.get::<bool>(3)? {
            return Err(CallTeardownRuntimeAttachError::InvalidConstraints);
        }
        match row.get::<String>(0)?.as_str() {
            "p" => {
                primary_key =
                    !primary_key && row.get::<bool>(4)? && row.get::<String>(5)? == "intent_id";
            }
            "c" => {
                check_definitions.insert(normalize_sql(&row.get::<String>(6)?));
            }
            _ => return Err(CallTeardownRuntimeAttachError::InvalidConstraints),
        }
    }
    let expected = expected_check_definitions();
    if count != 14
        || !primary_key
        || check_definitions.len() != expected.len()
        || !expected.is_subset(&check_definitions)
    {
        return Err(CallTeardownRuntimeAttachError::InvalidConstraints);
    }
    Ok(())
}

fn expected_check_definitions() -> HashSet<String> {
    [
        "CHECK (((identity IS NULL) OR (identity <> ''::text)))",
        "CHECK (((room_jid IS NULL) OR (room_jid <> ''::text)))",
        "CHECK (((generation IS NULL) OR (generation > 0)))",
        "CHECK (((occupant IS NULL) OR (occupant <> ''::text)))",
        "CHECK (((unbound_occupant IS NULL) OR (unbound_occupant <> ''::text)))",
        "CHECK (((session_binding IS NULL) OR (session_binding <> ''::text)))",
        "CHECK (((thread_id IS NULL) OR (thread_id <> ''::text)))",
        "CHECK (((anchor_origin_id IS NULL) OR (anchor_origin_id <> ''::text)))",
        "CHECK (((producing_node IS NULL) OR (producing_node <> ''::text)))",
        "CHECK ((action = ANY (ARRAY['remove_participant'::text, 'retire_displaced_participant'::text, 'delete_room'::text, 'muji_presence_clear'::text, 'muji_presence_clear_only'::text, 'muji_room_sweep'::text, 'call_thread_end_retry'::text])))",
        "CHECK ((status = ANY (ARRAY['queued'::text, 'in-progress'::text, 'done'::text, 'failed'::text])))",
        "CHECK ((((action = 'remove_participant'::text) AND (identity IS NOT NULL) AND (room_jid IS NULL)) OR ((action = 'retire_displaced_participant'::text) AND (identity IS NOT NULL) AND (room_jid IS NULL) AND ((participant_sid IS NULL) OR (participant_sid <> ''::text)) AND ((participant_sid IS NOT NULL) OR (occupant IS NOT NULL))) OR ((action = 'delete_room'::text) AND (identity IS NULL) AND (room_jid IS NULL) AND (participant_sid IS NULL)) OR ((action = 'muji_presence_clear'::text) AND (identity IS NOT NULL) AND (room_jid IS NOT NULL)) OR ((action = 'muji_presence_clear_only'::text) AND (identity IS NOT NULL) AND (room_jid IS NOT NULL) AND (participant_sid IS NULL)) OR ((action = 'muji_room_sweep'::text) AND (identity IS NULL) AND (room_jid IS NOT NULL) AND (room_sid IS NOT NULL) AND (participant_sid IS NULL)) OR ((action = 'call_thread_end_retry'::text) AND (identity IS NULL) AND (room_jid IS NOT NULL) AND (participant_sid IS NULL) AND (thread_id IS NOT NULL) AND (anchor_origin_id IS NOT NULL) AND (thread_started_at_ms IS NOT NULL) AND (thread_ended_at_ms IS NOT NULL))))",
        "CHECK ((((thread_id IS NULL) AND (anchor_origin_id IS NULL) AND (thread_started_at_ms IS NULL) AND (thread_ended_at_ms IS NULL)) OR (action = 'call_thread_end_retry'::text)))",
    ]
    .into_iter()
    .map(normalize_sql)
    .collect()
}

fn normalize_sql(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), CallTeardownRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.count(*)::bigint FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             WHERE n.nspname = ? AND t.relname = ?",
            crate::db_params![schema, TABLE],
        )
        .await
        .map_err(CallTeardownRuntimeAttachError::Relation)?;
    let count: i64 = rows
        .next()
        .await?
        .ok_or(CallTeardownRuntimeAttachError::InvalidIndexes)?
        .get(0)?;
    drop(rows);
    if count != (INDEXES.len() + 1) as i64 {
        return Err(CallTeardownRuntimeAttachError::InvalidIndexes);
    }
    for &(index, expected_columns) in INDEXES {
        let mut rows = tx
            .query(
                "SELECT NOT i.indisunique AND NOT i.indisprimary \
                 AND i.indisvalid AND i.indisready AND i.indislive \
                 AND i.indpred IS NULL AND i.indexprs IS NULL \
                 AND am.amname = 'btree' AND i.indnkeyatts = i.indnatts \
                 AND pg_catalog.array_to_string(ARRAY( \
                   SELECT a.attname::text FROM pg_catalog.unnest(i.indkey) \
                     WITH ORDINALITY k(attnum,pos) \
                   JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
                   WHERE k.pos <= i.indnkeyatts ORDER BY k.pos), ',') = ? \
                 AND NOT EXISTS (SELECT 1 FROM pg_catalog.unnest(i.indoption) opt \
                                 WHERE opt <> 0) \
                 FROM pg_catalog.pg_index i \
                 JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
                 JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
                 JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
                 JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
                 WHERE n.nspname = ? AND t.relname = ? AND ix.relname = ? \
                   AND ix.relnamespace = n.oid",
                crate::db_params![expected_columns, schema, TABLE, index],
            )
            .await
            .map_err(CallTeardownRuntimeAttachError::Relation)?;
        if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
            return Err(CallTeardownRuntimeAttachError::InvalidIndexes);
        }
    }
    Ok(())
}

async fn verify_runtime_role(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), CallTeardownRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreatedb \
             AND NOT r.rolcreaterole AND NOT r.rolreplication AND NOT r.rolbypassrls \
             AND session_user::text = r.rolname AND current_user::text = r.rolname \
             AND r.oid <> db.datdba AND r.oid <> n.nspowner \
             AND NOT owner.rolcanlogin AND NOT owner.rolsuper AND NOT owner.rolcreatedb \
             AND NOT owner.rolcreaterole AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
             AND owner.oid <> db.datdba \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m \
                             WHERE m.member = r.oid OR m.roleid = r.oid) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p \
                             WHERE p.pronamespace = n.oid AND p.proowner = r.oid) \
             AND pg_catalog.has_schema_privilege(r.oid, n.oid, 'USAGE') \
             AND NOT pg_catalog.has_schema_privilege(r.oid, n.oid, 'CREATE') \
             AND NOT pg_catalog.has_database_privilege(r.oid, db.oid, 'CREATE,TEMP') \
             AND NOT pg_catalog.has_parameter_privilege(r.oid, 'session_replication_role', 'SET') \
             AND pg_catalog.current_setting('session_replication_role') = 'origin' \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                 COALESCE(n.nspacl, pg_catalog.acldefault('n', n.nspowner))) a \
                 WHERE a.grantee = 0 OR (a.grantee <> n.nspowner \
                   AND (a.privilege_type <> 'USAGE' OR a.is_grantable))) \
             FROM pg_catalog.pg_roles r \
             JOIN pg_catalog.pg_namespace n ON n.nspname = ? \
             JOIN pg_catalog.pg_roles owner ON owner.oid = n.nspowner \
             JOIN pg_catalog.pg_database db ON db.datname = pg_catalog.current_database() \
             WHERE r.rolname = current_user",
            crate::db_params![schema],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(CallTeardownRuntimeAttachError::UnsafeRuntimeRole);
    }
    Ok(())
}

async fn verify_table_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), CallTeardownRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT relation.relowner = ns.nspowner AND relation.relowner <> runtime.oid \
             AND NOT pg_catalog.pg_has_role(runtime.oid, relation.relowner, 'SET') \
             AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT') \
             AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT') \
             AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'UPDATE') \
             AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'DELETE') \
             AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'TRUNCATE,REFERENCES,TRIGGER') \
             AND NOT pg_catalog.has_any_column_privilege(runtime.oid, relation.oid, 'REFERENCES') \
             AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, \
               'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION') \
             AND NOT pg_catalog.has_any_column_privilege(runtime.oid, relation.oid, \
               'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION') \
             AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
               THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN') ELSE true END \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
               COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) a \
               WHERE a.grantee NOT IN (relation.relowner, runtime.oid) \
                  OR (a.grantee = runtime.oid AND (a.is_grantable OR a.privilege_type NOT IN \
                    ('SELECT','INSERT','UPDATE','DELETE'))) \
                  OR (a.grantee = 0)) \
             AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute col \
               CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a \
               WHERE col.attrelid = relation.oid AND col.attnum > 0 AND NOT col.attisdropped) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace ns ON ns.oid = relation.relnamespace \
             JOIN pg_catalog.pg_roles runtime ON runtime.rolname = current_user \
             WHERE ns.nspname = ? AND relation.relname = ?",
            crate::db_params![schema, TABLE],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(CallTeardownRuntimeAttachError::UnsafeRuntimePrivileges);
    }
    Ok(())
}
