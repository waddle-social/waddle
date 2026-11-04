//! Read-only verification of the owner-prepared room-effect outbox.

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum RoomEffectRuntimeAttachError {
    #[error("room-effect attachment requires an attested PostgreSQL database")]
    AttestedPostgresRequired,
    #[error("room-effect attachment requires the main database lane")]
    MainLaneRequired,
    #[error("room-effect active schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not bound PostgreSQL waits for room-effect attachment")]
    ReadBounds,
    #[error("failed to lock room-effect relation {table}: {source}")]
    Relation {
        table: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("room-effect relation {table} has an unexpected shape")]
    InvalidTable { table: &'static str },
    #[error("room-effect relation {table} has an invalid column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("room-effect relation {table} has invalid constraints")]
    InvalidConstraint { table: &'static str },
    #[error("room-effect relation {table} has invalid indexes")]
    InvalidIndex { table: &'static str },
    #[error("room-effect runtime role or privileges are unsafe at {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

#[derive(Clone, Copy)]
struct Column {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    identity: &'static str,
    default: Option<&'static str>,
}

const fn column(name: &'static str, kind: &'static str, not_null: bool) -> Column {
    Column {
        name,
        kind,
        not_null,
        identity: "",
        default: None,
    }
}
const fn text(name: &'static str, not_null: bool) -> Column {
    column(name, "text", not_null)
}
const fn bigint(name: &'static str, not_null: bool) -> Column {
    column(name, "bigint", not_null)
}

struct Index {
    name: &'static str,
    columns: &'static str,
    options: &'static str,
}

struct Table {
    name: &'static str,
    columns: &'static [Column],
    primary: &'static str,
    checks: &'static [&'static str],
    indexes: &'static [Index],
    insert: bool,
    update: bool,
    delete: bool,
}

const EFFECT_COLUMNS: &[Column] = &[
    text("lifecycle_id", true),
    bigint("revision", true),
    bigint("ordinal", true),
    text("room_jid", true),
    text("kind", true),
    column("terminal", "boolean", true),
    text("payload_json", true),
    bigint("available_at_ms", true),
    Column {
        name: "superseded",
        kind: "boolean",
        not_null: true,
        identity: "",
        default: Some("false"),
    },
    text("origin_instance_id", true),
    text("producing_node", true),
    text("lease_token", false),
    bigint("leased_at_ms", false),
    Column {
        name: "attempt_count",
        kind: "bigint",
        not_null: true,
        identity: "",
        default: Some("0"),
    },
    text("last_error", false),
    bigint("created_at_ms", true),
    bigint("unowned_since_ms", false),
];
const EFFECT_INDEXES: &[Index] = &[Index {
    name: "clustering_muc_room_effects_due_idx",
    columns: "available_at_ms,lifecycle_id",
    options: "0,0",
}];
const EFFECT_CHECKS: &[&str] = &[
    "CHECK ((revision >= 1))",
    "CHECK ((ordinal >= 0))",
    "CHECK ((room_jid <> ''::text))",
    "CHECK ((kind <> ''::text))",
];
const EFFECTS: Table = Table {
    name: "clustering_muc_room_effects",
    columns: EFFECT_COLUMNS,
    primary: "lifecycle_id,revision,ordinal",
    checks: EFFECT_CHECKS,
    indexes: EFFECT_INDEXES,
    insert: true,
    update: true,
    delete: true,
};

pub(super) async fn verify(db: &Database) -> Result<(), RoomEffectRuntimeAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(RoomEffectRuntimeAttachError::AttestedPostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(RoomEffectRuntimeAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(RoomEffectRuntimeAttachError::AttestedPostgresRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;
    let identity = capability.identity();
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "room-effect schema")
        .map_err(|_| RoomEffectRuntimeAttachError::ActiveSchemaMismatch)?;
    let quoted = crate::db::physical_attestation::quote_identifier(schema);
    let mut tx = db.begin().await?;
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
        .map_err(|_| RoomEffectRuntimeAttachError::ReadBounds)?;
    tx.execute(
        &format!("LOCK TABLE {quoted}.clustering_muc_room_effects IN ACCESS SHARE MODE"),
        (),
    )
    .await
    .map_err(|source| RoomEffectRuntimeAttachError::Relation {
        table: EFFECTS.name,
        source,
    })?;
    capability.attest_transaction(&mut tx).await?;
    let mut active = tx
        .query(
            "SELECT n.oid::bigint = ? AND n.nspname = pg_catalog.current_schema() \
         AND COALESCE((pg_catalog.current_schemas(false))[1]::text,'') = n.nspname \
         AND pg_catalog.cardinality(pg_catalog.current_schemas(false)) <= 2 \
         AND (pg_catalog.cardinality(pg_catalog.current_schemas(false)) = 1 \
           OR pg_catalog.left((pg_catalog.current_schemas(false))[2]::text,8) = 'pg_temp_') \
         FROM pg_catalog.pg_namespace n WHERE n.nspname = ?",
            crate::db_params![i64::from(identity.schema.oid), schema],
        )
        .await?;
    if !matches!(active.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(RoomEffectRuntimeAttachError::ActiveSchemaMismatch);
    }
    verify_runtime_role(&mut tx, schema).await?;
    verify_table(&mut tx, schema, &EFFECTS).await?;
    verify_columns(&mut tx, schema, &EFFECTS).await?;
    verify_constraints(&mut tx, schema, &EFFECTS).await?;
    verify_indexes(&mut tx, schema, &EFFECTS).await?;
    verify_table_acl(&mut tx, schema, &EFFECTS).await?;
    tx.rollback().await?;
    Ok(())
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition AND c.relowner = n.nspowner \
         AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND c.reloftype = 0 \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid = c.oid OR h.inhparent = c.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class = c.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint fk WHERE fk.contype = 'f' AND fk.confrelid = c.oid) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ? AND c.relname = ?",
        crate::db_params![schema, table.name],
    ).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(RoomEffectRuntimeAttachError::InvalidTable { table: table.name });
    }
    Ok(())
}

pub(super) async fn verify_existing_owner(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<bool, RoomEffectRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ? AND c.relname = ?)",
            crate::db_params![schema, EFFECTS.name],
        )
        .await?;
    let exists = matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?);
    if exists {
        verify_table(tx, schema, &EFFECTS).await?;
        verify_columns(tx, schema, &EFFECTS).await?;
        verify_constraints(tx, schema, &EFFECTS).await?;
        verify_indexes(tx, schema, &EFFECTS).await?;
    }
    Ok(exists)
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull, \
         a.attgenerated::text, a.attidentity::text, pg_catalog.pg_get_expr(d.adbin,d.adrelid), \
         a.attcollation = typ.typcollation \
         FROM pg_catalog.pg_attribute a JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         JOIN pg_catalog.pg_type typ ON typ.oid = a.atttypid \
         LEFT JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
         WHERE n.nspname = ? AND c.relname = ? AND a.attnum > 0 AND NOT a.attisdropped",
        crate::db_params![schema, table.name],
    ).await?;
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
    if actual.len() != table.columns.len() {
        return Err(RoomEffectRuntimeAttachError::InvalidTable { table: table.name });
    }
    for column in table.columns {
        let Some((kind, not_null, generated, identity, default, default_collation)) =
            actual.get(column.name)
        else {
            return Err(RoomEffectRuntimeAttachError::InvalidColumn {
                table: table.name,
                column: column.name,
            });
        };
        if kind != column.kind
            || *not_null != column.not_null
            || !generated.is_empty()
            || identity != column.identity
            || default.as_deref() != column.default
            || !default_collation
        {
            return Err(RoomEffectRuntimeAttachError::InvalidColumn {
                table: table.name,
                column: column.name,
            });
        }
    }
    Ok(())
}

async fn verify_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT con.contype::text, con.condeferrable, con.condeferred, con.convalidated, \
         COALESCE(ix.indisvalid AND ix.indisready AND ix.indislive AND ix.indimmediate, false), \
         pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.conkey) WITH ORDINALITY k(attnum,pos) \
           JOIN pg_catalog.pg_attribute a ON a.attrelid = c.oid AND a.attnum = k.attnum ORDER BY k.pos), ','), \
         pg_catalog.pg_get_constraintdef(con.oid) \
         FROM pg_catalog.pg_constraint con JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         LEFT JOIN pg_catalog.pg_index ix ON ix.indexrelid = con.conindid \
         WHERE n.nspname = ? AND c.relname = ?",
        crate::db_params![schema, table.name],
    ).await?;
    let mut primary = false;
    let mut checks = Vec::new();
    let mut count = 0usize;
    while let Some(row) = rows.next().await? {
        count += 1;
        let kind: String = row.get(0)?;
        let deferred: bool = row.get(1)?;
        let initially_deferred: bool = row.get(2)?;
        let validated: bool = row.get(3)?;
        if deferred || initially_deferred || !validated {
            return Err(RoomEffectRuntimeAttachError::InvalidConstraint { table: table.name });
        }
        match kind.as_str() {
            "p" => {
                primary = !primary && row.get::<bool>(4)? && row.get::<String>(5)? == table.primary;
            }
            "c" => checks.push(row.get::<String>(6)?),
            _ => return Err(RoomEffectRuntimeAttachError::InvalidConstraint { table: table.name }),
        }
    }
    checks.sort_unstable();
    let mut expected = table.checks.to_vec();
    expected.sort_unstable();
    if !primary
        || count != 1 + table.checks.len()
        || checks.iter().map(String::as_str).collect::<Vec<_>>() != expected
    {
        return Err(RoomEffectRuntimeAttachError::InvalidConstraint { table: table.name });
    }
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.count(*)::bigint FROM pg_catalog.pg_index i \
         JOIN pg_catalog.pg_class c ON c.oid = i.indrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, table.name],
        )
        .await?;
    let count: i64 = rows
        .next()
        .await?
        .ok_or(RoomEffectRuntimeAttachError::InvalidIndex { table: table.name })?
        .get(0)?;
    if count != (1 + table.indexes.len()) as i64 {
        return Err(RoomEffectRuntimeAttachError::InvalidIndex { table: table.name });
    }
    for index in table.indexes {
        let mut rows = tx.query(
            "SELECT NOT i.indisunique AND NOT i.indisprimary AND i.indisvalid AND i.indisready AND i.indislive \
             AND i.indpred IS NULL AND i.indexprs IS NULL AND am.amname = 'btree' \
             AND i.indnkeyatts = i.indnatts, \
             pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(i.indkey) WITH ORDINALITY k(attnum,pos) \
               JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum WHERE k.pos <= i.indnkeyatts ORDER BY k.pos), ','), \
             pg_catalog.array_to_string(ARRAY(SELECT k.option::text FROM pg_catalog.unnest(i.indoption) WITH ORDINALITY k(option,pos) \
               WHERE k.pos <= i.indnkeyatts ORDER BY k.pos), ',') \
             FROM pg_catalog.pg_index i JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
             WHERE n.nspname = ? AND t.relname = ? AND ix.relname = ? AND ix.relnamespace = n.oid",
            crate::db_params![schema, table.name, index.name],
        ).await?;
        let valid = match rows.next().await? {
            Some(row) => {
                row.get::<bool>(0)?
                    && row.get::<String>(1)? == index.columns
                    && row.get::<String>(2)? == index.options
            }
            None => false,
        };
        if !valid {
            return Err(RoomEffectRuntimeAttachError::InvalidIndex { table: table.name });
        }
    }
    Ok(())
}

async fn verify_runtime_role(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT r.rolcanlogin AND NOT r.rolsuper AND NOT r.rolcreatedb AND NOT r.rolcreaterole \
         AND NOT r.rolreplication AND NOT r.rolbypassrls AND session_user::text = r.rolname \
         AND current_user::text = r.rolname AND r.oid <> db.datdba AND r.oid <> n.nspowner \
         AND NOT owner.rolcanlogin AND NOT owner.rolsuper AND NOT owner.rolcreatedb \
         AND NOT owner.rolcreaterole AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
         AND owner.oid <> db.datdba \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = r.oid OR m.roleid = r.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p WHERE p.pronamespace = n.oid AND p.proowner = r.oid) \
         AND pg_catalog.has_schema_privilege(r.oid,n.oid,'USAGE') \
         AND NOT pg_catalog.has_schema_privilege(r.oid,n.oid,'CREATE') \
         AND NOT pg_catalog.has_database_privilege(r.oid,db.oid,'CREATE,TEMP') \
         AND NOT pg_catalog.has_parameter_privilege(r.oid,'session_replication_role','SET') \
         AND pg_catalog.current_setting('session_replication_role') = 'origin' \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(n.nspacl,pg_catalog.acldefault('n',n.nspowner))) a \
           WHERE a.grantee = 0 OR (a.grantee <> n.nspowner AND (a.privilege_type <> 'USAGE' OR a.is_grantable))) \
         FROM pg_catalog.pg_roles r JOIN pg_catalog.pg_namespace n ON n.nspname = ? \
         JOIN pg_catalog.pg_roles owner ON owner.oid = n.nspowner \
         JOIN pg_catalog.pg_database db ON db.datname = pg_catalog.current_database() \
         WHERE r.rolname = current_user",
        crate::db_params![schema],
    ).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "runtime role",
        });
    }
    Ok(())
}

async fn verify_table_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let mut rows = tx.query(
        "SELECT c.relowner = n.nspowner AND c.relowner <> r.oid \
         AND NOT pg_catalog.pg_has_role(r.oid,c.relowner,'SET') \
         AND pg_catalog.has_table_privilege(r.oid,c.oid,'SELECT') \
         AND pg_catalog.has_table_privilege(r.oid,c.oid,'INSERT') = (? = 1) \
         AND pg_catalog.has_table_privilege(r.oid,c.oid,'UPDATE') = (? = 1) \
         AND pg_catalog.has_table_privilege(r.oid,c.oid,'DELETE') = (? = 1) \
         AND NOT pg_catalog.has_table_privilege(r.oid,c.oid,'TRUNCATE,REFERENCES,TRIGGER') \
         AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,'REFERENCES') \
         AND NOT pg_catalog.has_table_privilege(r.oid,c.oid, \
          'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION') \
         AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid, \
          'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION') \
         AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
           THEN NOT pg_catalog.has_table_privilege(r.oid,c.oid,'MAINTAIN') ELSE true END \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) a \
           WHERE a.grantee NOT IN (c.relowner,r.oid)) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute col CROSS JOIN LATERAL pg_catalog.aclexplode(col.attacl) a \
           WHERE col.attrelid = c.oid AND col.attnum > 0 AND NOT col.attisdropped AND a.grantee <> c.relowner) \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
         JOIN pg_catalog.pg_roles r ON r.rolname=current_user WHERE n.nspname = ? AND c.relname = ?",
        crate::db_params![table.insert, table.update, table.delete, schema, table.name],
    ).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: table.name,
        });
    }
    Ok(())
}
