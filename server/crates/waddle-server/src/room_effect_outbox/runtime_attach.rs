//! Read-only verification of the owner-prepared room-effect outbox.

use crate::db::physical_attestation::AttestedPostgresWriterCapability;
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
    #[error("room-effect relation {table} has invalid constraints: {observed}")]
    ConstraintDrift {
        table: &'static str,
        observed: String,
    },
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

const fn timestamptz(name: &'static str, not_null: bool, default: Option<&'static str>) -> Column {
    Column {
        name,
        kind: "timestamp with time zone",
        not_null,
        identity: "",
        default,
    }
}

struct Index {
    name: &'static str,
    columns: &'static str,
    options: &'static str,
    unique: bool,
    predicate: Option<&'static str>,
}

struct ForeignKey {
    columns: &'static str,
    referenced_table: &'static str,
    referenced_columns: &'static str,
    on_delete: &'static str,
    on_update: &'static str,
}

struct Table {
    name: &'static str,
    columns: &'static [Column],
    primary: &'static str,
    checks: &'static [&'static str],
    foreign_key: Option<ForeignKey>,
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
    unique: false,
    predicate: None,
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
    foreign_key: None,
    indexes: EFFECT_INDEXES,
    insert: true,
    update: true,
    delete: true,
};

const MUC_ROOM_COLUMNS: &[Column] = &[
    text("room_jid", true),
    text("waddle_id", true),
    text("channel_id", true),
    text("config_json", true),
    text("subject_json", false),
    text("config_lifecycle_id", false),
    bigint("config_revision", false),
    timestamptz("updated_at", true, Some("now()")),
    text("lifecycle_id", false),
    bigint("revision", false),
];
const MUC_ROOM_CHECKS: &[&str] = &[
    "CHECK (((config_lifecycle_id IS NULL) = (config_revision IS NULL)))",
    "CHECK (((lifecycle_id IS NULL) = (revision IS NULL)))",
    "CHECK (((config_revision IS NULL) OR (config_revision >= 1)))",
    "CHECK (((revision IS NULL) OR (revision >= 1)))",
];
const MUC_ROOMS: Table = Table {
    name: "clustering_muc_rooms",
    columns: MUC_ROOM_COLUMNS,
    primary: "room_jid",
    checks: MUC_ROOM_CHECKS,
    foreign_key: None,
    indexes: &[],
    insert: true,
    update: true,
    delete: true,
};

const MUC_AFFILIATION_COLUMNS: &[Column] = &[
    text("room_jid", true),
    text("member_jid", true),
    text("affiliation", true),
    text("reason", false),
    timestamptz("granted_at", false, None),
];
const MUC_AFFILIATION_INDEXES: &[Index] = &[Index {
    name: "clustering_muc_room_affiliations_room_jid_idx",
    columns: "room_jid",
    options: "0",
    unique: false,
    predicate: None,
}];
const MUC_AFFILIATIONS: Table = Table {
    name: "clustering_muc_room_affiliations",
    columns: MUC_AFFILIATION_COLUMNS,
    primary: "room_jid,member_jid",
    checks: &[],
    foreign_key: None,
    indexes: MUC_AFFILIATION_INDEXES,
    insert: true,
    update: true,
    delete: true,
};

const MUC_LIFECYCLE_COLUMNS: &[Column] = &[
    text("lifecycle_id", true),
    text("room_jid", true),
    bigint("revision", true),
    text("state", true),
    text("mutation_fingerprint", false),
    timestamptz("created_at", true, Some("now()")),
    timestamptz("updated_at", true, Some("now()")),
];
const MUC_LIFECYCLE_CHECKS: &[&str] = &[
    "CHECK ((revision >= 1))",
    "CHECK ((state = ANY (ARRAY['preparing'::text, 'active'::text, 'dormant'::text, 'tombstoned'::text])))",
];
const MUC_LIFECYCLE_INDEXES: &[Index] = &[Index {
    name: "clustering_muc_room_lifecycles_live_room_idx",
    columns: "room_jid",
    options: "0",
    unique: true,
    predicate: Some("(state = ANY (ARRAY['preparing'::text, 'active'::text, 'dormant'::text]))"),
}];
const MUC_LIFECYCLES: Table = Table {
    name: "clustering_muc_room_lifecycles",
    columns: MUC_LIFECYCLE_COLUMNS,
    primary: "lifecycle_id",
    checks: MUC_LIFECYCLE_CHECKS,
    foreign_key: None,
    indexes: MUC_LIFECYCLE_INDEXES,
    insert: true,
    update: true,
    delete: true,
};

const MUC_ADMIN_RECEIPT_COLUMNS: &[Column] = &[
    text("attempt_id", true),
    text("room_jid", true),
    text("lifecycle_id", true),
    bigint("revision", true),
    bigint("created_at_ms", true),
    text("removed_sessions_json", true),
];
const MUC_ADMIN_RECEIPT_INDEXES: &[Index] = &[
    Index {
        name: "clustering_muc_admin_receipts_lifecycle_idx",
        columns: "lifecycle_id",
        options: "0",
        unique: false,
        predicate: None,
    },
    Index {
        name: "clustering_muc_admin_receipts_created_at_idx",
        columns: "created_at_ms",
        options: "0",
        unique: false,
        predicate: None,
    },
];
const MUC_ADMIN_RECEIPTS: Table = Table {
    name: "clustering_muc_admin_receipts",
    columns: MUC_ADMIN_RECEIPT_COLUMNS,
    primary: "attempt_id",
    checks: &["CHECK ((revision >= 1))"],
    foreign_key: Some(ForeignKey {
        columns: "lifecycle_id",
        referenced_table: "clustering_muc_room_lifecycles",
        referenced_columns: "lifecycle_id",
        on_delete: "c",
        on_update: "a",
    }),
    indexes: MUC_ADMIN_RECEIPT_INDEXES,
    insert: true,
    update: false,
    delete: true,
};

const MUC_DESTROY_COLUMNS: &[Column] = &[
    text("attempt_id", true),
    text("payload_json", true),
    text("lifecycle_id", false),
    text("origin_instance_id", false),
    bigint("available_at_ms", true),
    text("lease_token", false),
    bigint("leased_at_ms", false),
];
const MUC_DESTROY_INDEXES: &[Index] = &[Index {
    name: "clustering_muc_destroy_outbox_due_idx",
    columns: "available_at_ms,leased_at_ms",
    options: "0,0",
    unique: false,
    predicate: None,
}];
const MUC_DESTROY_OUTBOX: Table = Table {
    name: "clustering_muc_destroy_outbox",
    columns: MUC_DESTROY_COLUMNS,
    primary: "attempt_id",
    checks: &[],
    foreign_key: None,
    indexes: MUC_DESTROY_INDEXES,
    insert: true,
    update: true,
    delete: true,
};

const MUC_TABLES: &[&Table] = &[
    &MUC_ROOMS,
    &MUC_AFFILIATIONS,
    &MUC_LIFECYCLES,
    &MUC_ADMIN_RECEIPTS,
    &MUC_DESTROY_OUTBOX,
    &EFFECTS,
];

pub(super) async fn verify(db: &Database) -> Result<(), RoomEffectRuntimeAttachError> {
    verify_tables(db, &[&EFFECTS], false, None, None)
        .await
        .map(drop)
}

pub(super) async fn verify_muc_room_runtime(
    db: &Database,
) -> Result<AttestedPostgresWriterCapability, RoomEffectRuntimeAttachError> {
    verify_tables(db, MUC_TABLES, true, None, None).await
}

pub(super) async fn verify_muc_room_runtime_v1(
    db: &Database,
    profile: &crate::pubsub::RestrictedServingProfileV1,
) -> Result<AttestedPostgresWriterCapability, RoomEffectRuntimeAttachError> {
    verify_tables(db, MUC_TABLES, true, Some(profile), None).await
}

pub(super) async fn verify_effect_outbox_v2(
    db: &Database,
    profile: &crate::pubsub::RestrictedServingProfileV2,
) -> Result<AttestedPostgresWriterCapability, RoomEffectRuntimeAttachError> {
    verify_tables(db, &[&EFFECTS], true, Some(&profile.v1), Some(profile)).await
}

async fn verify_tables(
    db: &Database,
    tables: &[&Table],
    exact_schema_acl: bool,
    profile: Option<&crate::pubsub::RestrictedServingProfileV1>,
    v2: Option<&crate::pubsub::RestrictedServingProfileV2>,
) -> Result<AttestedPostgresWriterCapability, RoomEffectRuntimeAttachError> {
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
    if profile.is_none() {
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
            (),
        )
        .await?;
    }
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|_| RoomEffectRuntimeAttachError::ReadBounds)?;
    if v2.is_none() {
        lock_tables(&mut tx, &quoted, tables, false).await?;
    }
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
    drop(active);
    if let Some(profile) = profile {
        tx.execute("SET LOCAL search_path = pg_catalog, pg_temp", ())
            .await?;
        crate::pubsub::runtime_attach::verify_restricted_v1_graph_tx(
            &mut tx,
            identity,
            capability.definer_role().as_str(),
            profile,
        )
        .await
        .map_err(|_| RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "v1 role graph",
        })?;
        if let Some(v2) = v2 {
            crate::pubsub::runtime_attach::verify_restricted_v2_graph_tx(
                &mut tx,
                identity,
                capability.definer_role().as_str(),
                v2,
            )
            .await
            .map_err(|_| RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
                component: "v2 role graph",
            })?;
            // Check the relation kind and inheritance before a name-based
            // lock, then recheck the catalog after the bounded wait.
            for table in tables {
                crate::pubsub::runtime_attach::verify_v2_relation_tx(
                    &mut tx,
                    identity,
                    table.name,
                    v2.effect_relation_oid,
                )
                .await
                .map_err(|_| RoomEffectRuntimeAttachError::InvalidTable { table: table.name })?;
                verify_table(&mut tx, schema, table, true).await?;
            }
            lock_tables(&mut tx, &quoted, tables, true).await?;
            for table in tables {
                crate::pubsub::runtime_attach::verify_v2_relation_tx(
                    &mut tx,
                    identity,
                    table.name,
                    v2.effect_relation_oid,
                )
                .await
                .map_err(|_| RoomEffectRuntimeAttachError::InvalidTable { table: table.name })?;
            }
            crate::pubsub::runtime_attach::verify_restricted_v2_graph_tx(
                &mut tx,
                identity,
                capability.definer_role().as_str(),
                v2,
            )
            .await
            .map_err(|_| RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
                component: "v2 role graph",
            })?;
        }
        let names: Vec<_> = tables.iter().map(|table| table.name).collect();
        crate::pubsub::runtime_attach::verify_no_definer_writers_tx(&mut tx, schema, &names)
            .await
            .map_err(|_| RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
                component: "v1 definer relay",
            })?;
    }
    verify_runtime_role(
        &mut tx,
        schema,
        exact_schema_acl,
        capability.definer_role().as_str(),
        profile,
    )
    .await?;
    for table in tables {
        verify_table(&mut tx, schema, table, exact_schema_acl).await?;
        verify_columns(&mut tx, schema, table).await?;
        verify_constraints(&mut tx, schema, table).await?;
        verify_indexes(&mut tx, schema, table).await?;
        verify_table_acl(&mut tx, schema, table).await?;
    }
    verify_incoming_foreign_keys(&mut tx, schema, tables).await?;
    tx.rollback().await?;
    Ok(capability)
}

async fn lock_tables(
    tx: &mut Transaction<'_>,
    quoted_schema: &str,
    tables: &[&Table],
    only: bool,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let only = if only { "ONLY " } else { "" };
    for table in tables {
        let relation = crate::db::physical_attestation::quote_identifier(table.name);
        tx.execute(
            &format!("LOCK TABLE {only}{quoted_schema}.{relation} IN ACCESS SHARE MODE"),
            (),
        )
        .await
        .map_err(|source| RoomEffectRuntimeAttachError::Relation {
            table: table.name,
            source,
        })?;
    }
    Ok(())
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
    exact_storage: bool,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let access_method = if exact_storage {
        "AND c.relam = (SELECT oid FROM pg_catalog.pg_am WHERE amname = 'heap')"
    } else {
        ""
    };
    let incoming_foreign_keys = if exact_storage {
        ""
    } else {
        "AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint fk WHERE fk.contype = 'f' AND fk.confrelid = c.oid)"
    };
    let trigger_absence = if exact_storage {
        "AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid AND NOT t.tgisinternal)"
    } else {
        "AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t WHERE t.tgrelid = c.oid)"
    };
    let mut rows = tx.query(
        &format!("SELECT c.relkind = 'r' AND c.relpersistence = 'p' AND NOT c.relispartition AND c.relowner = n.nspowner \
         AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND c.reloftype = 0 {access_method} \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid = c.oid OR h.inhparent = c.oid) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw WHERE rw.ev_class = c.oid) \
         {trigger_absence} \
         {incoming_foreign_keys} \
         FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = ? AND c.relname = ?"),
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
        verify_table(tx, schema, &EFFECTS, false).await?;
        verify_columns(tx, schema, &EFFECTS).await?;
        verify_constraints(tx, schema, &EFFECTS).await?;
        verify_indexes(tx, schema, &EFFECTS).await?;
        verify_incoming_foreign_keys(tx, schema, &[&EFFECTS]).await?;
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
         pg_catalog.pg_get_constraintdef(con.oid), ref_ns.nspname::text, ref.relname::text, \
         pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.confkey) WITH ORDINALITY k(attnum,pos) \
           JOIN pg_catalog.pg_attribute a ON a.attrelid = ref.oid AND a.attnum = k.attnum ORDER BY k.pos), ','), \
         con.confdeltype::text, con.confupdtype::text, con.confmatchtype::text, con.connoinherit, con.conparentid = 0 \
         FROM pg_catalog.pg_constraint con JOIN pg_catalog.pg_class c ON c.oid = con.conrelid \
         JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
         LEFT JOIN pg_catalog.pg_class ref ON ref.oid = con.confrelid \
         LEFT JOIN pg_catalog.pg_namespace ref_ns ON ref_ns.oid = ref.relnamespace \
         LEFT JOIN pg_catalog.pg_index ix ON ix.indexrelid = con.conindid \
         WHERE n.nspname = ? AND c.relname = ?",
        crate::db_params![schema, table.name],
    ).await?;
    let mut primary = false;
    let mut checks = Vec::new();
    let mut foreign_key = false;
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
            "f" => {
                let Some(expected) = &table.foreign_key else {
                    return Err(RoomEffectRuntimeAttachError::InvalidConstraint {
                        table: table.name,
                    });
                };
                foreign_key = !foreign_key
                    && row.get::<String>(7)? == schema
                    && row.get::<String>(8)? == expected.referenced_table
                    && row.get::<String>(5)? == expected.columns
                    && row.get::<String>(9)? == expected.referenced_columns
                    && row.get::<String>(10)? == expected.on_delete
                    && row.get::<String>(11)? == expected.on_update
                    && row.get::<String>(12)? == "s"
                    && row.get::<bool>(13)?
                    && row.get::<bool>(14)?;
            }
            _ => return Err(RoomEffectRuntimeAttachError::InvalidConstraint { table: table.name }),
        }
    }
    checks.sort_unstable();
    let mut expected = table.checks.to_vec();
    expected.sort_unstable();
    if !primary
        || count != 1 + table.checks.len() + if table.foreign_key.is_some() { 1 } else { 0 }
        || foreign_key != table.foreign_key.is_some()
        || checks.iter().map(String::as_str).collect::<Vec<_>>() != expected
    {
        return Err(RoomEffectRuntimeAttachError::ConstraintDrift {
            table: table.name,
            observed: format!(
                "primary={primary}, count={count}, foreign_key={foreign_key}, checks={checks:?}, expected_checks={expected:?}"
            ),
        });
    }
    Ok(())
}

async fn verify_incoming_foreign_keys(
    tx: &mut Transaction<'_>,
    schema: &str,
    tables: &[&Table],
) -> Result<(), RoomEffectRuntimeAttachError> {
    let names = tables
        .iter()
        .map(|table| format!("'{}'", table.name.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(
        "SELECT source.relname::text, target.relname::text, \
         pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.conkey) WITH ORDINALITY k(attnum,pos) \
           JOIN pg_catalog.pg_attribute a ON a.attrelid = source.oid AND a.attnum = k.attnum ORDER BY k.pos), ','), \
         pg_catalog.array_to_string(ARRAY(SELECT a.attname::text FROM pg_catalog.unnest(con.confkey) WITH ORDINALITY k(attnum,pos) \
           JOIN pg_catalog.pg_attribute a ON a.attrelid = target.oid AND a.attnum = k.attnum ORDER BY k.pos), ','), \
         con.confdeltype::text, con.confupdtype::text, con.confmatchtype::text, con.convalidated, \
         con.condeferrable, con.condeferred, con.connoinherit, con.conparentid = 0, source_ns.nspname::text \
         FROM pg_catalog.pg_constraint con \
         JOIN pg_catalog.pg_class source ON source.oid = con.conrelid \
         JOIN pg_catalog.pg_namespace source_ns ON source_ns.oid = source.relnamespace \
         JOIN pg_catalog.pg_class target ON target.oid = con.confrelid \
         JOIN pg_catalog.pg_namespace target_ns ON target_ns.oid = target.relnamespace \
         WHERE con.contype = 'f' AND target_ns.nspname = ? AND target.relname IN ({names})"
    );
    let mut rows = tx.query(&sql, crate::db_params![schema]).await?;
    let mut actual = Vec::new();
    while let Some(row) = rows.next().await? {
        let valid = row.get::<String>(4)? == "c"
            && row.get::<String>(5)? == "a"
            && row.get::<String>(6)? == "s"
            && row.get::<bool>(7)?
            && !row.get::<bool>(8)?
            && !row.get::<bool>(9)?
            && row.get::<bool>(10)?
            && row.get::<bool>(11)?
            && row.get::<String>(12)? == schema;
        if !valid {
            return Err(RoomEffectRuntimeAttachError::InvalidConstraint {
                table: "MUC relation set",
            });
        }
        actual.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<String>(2)?,
            row.get::<String>(3)?,
            row.get::<String>(4)?,
            row.get::<String>(5)?,
        ));
    }
    let expected = tables
        .iter()
        .filter_map(|source| {
            source.foreign_key.as_ref().and_then(|foreign_key| {
                tables
                    .iter()
                    .any(|target| target.name == foreign_key.referenced_table)
                    .then(|| {
                        (
                            source.name.to_owned(),
                            foreign_key.referenced_table.to_owned(),
                            foreign_key.columns.to_owned(),
                            foreign_key.referenced_columns.to_owned(),
                            foreign_key.on_delete.to_owned(),
                            foreign_key.on_update.to_owned(),
                        )
                    })
            })
        })
        .collect::<Vec<_>>();
    actual.sort_unstable();
    let mut expected = expected;
    expected.sort_unstable();
    if actual != expected {
        return Err(RoomEffectRuntimeAttachError::InvalidConstraint {
            table: "MUC relation set",
        });
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
            "SELECT i.indisunique, NOT i.indisprimary AND i.indisvalid AND i.indisready AND i.indislive \
             AND i.indexprs IS NULL AND am.amname = 'btree' AND i.indnkeyatts = i.indnatts, \
             pg_catalog.pg_get_expr(i.indpred, i.indrelid), \
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
                row.get::<bool>(0)? == index.unique
                    && row.get::<bool>(1)?
                    && row.get::<Option<String>>(2)?.as_deref() == index.predicate
                    && row.get::<String>(3)? == index.columns
                    && row.get::<String>(4)? == index.options
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
    exact_schema_acl: bool,
    expected_definer: &str,
    profile: Option<&crate::pubsub::RestrictedServingProfileV1>,
) -> Result<(), RoomEffectRuntimeAttachError> {
    let schema_acl = if let Some(profile) = profile {
        format!("AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(n.nspacl,pg_catalog.acldefault('n',n.nspowner))) a \
           WHERE a.grantee NOT IN (n.nspowner,r.oid,definer.oid,pg_catalog.to_regrole('pg_monitor'),{}::pg_catalog.oid,{}::pg_catalog.oid) \
             OR (a.grantee <> n.nspowner AND (a.grantor <> n.nspowner OR a.privilege_type <> 'USAGE' OR a.is_grantable)))",
            profile.api_oid, profile.helper_oid)
    } else if exact_schema_acl {
        "AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(n.nspacl,pg_catalog.acldefault('n',n.nspowner))) a \
           WHERE a.grantee = 0 OR a.grantee NOT IN (n.nspowner,r.oid,definer.oid) \
             OR (a.grantee = r.oid AND (a.privilege_type <> 'USAGE' OR a.is_grantable)) \
             OR (a.grantee = definer.oid AND (a.privilege_type <> 'USAGE' OR a.is_grantable)))".to_owned()
    } else {
        "AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(n.nspacl,pg_catalog.acldefault('n',n.nspowner))) a \
           WHERE a.grantee = 0 OR (a.grantee <> n.nspowner AND (a.privilege_type <> 'USAGE' OR a.is_grantable)))".to_owned()
    };
    let definer_join = if exact_schema_acl {
        "JOIN pg_catalog.pg_roles definer ON definer.rolname = ?"
    } else {
        ""
    };
    let definer_safety = if exact_schema_acl {
        "AND definer.oid <> r.oid AND definer.oid <> owner.oid AND definer.oid <> db.datdba \
         AND NOT definer.rolcanlogin AND NOT definer.rolsuper AND NOT definer.rolcreatedb \
         AND NOT definer.rolcreaterole AND NOT definer.rolreplication AND NOT definer.rolbypassrls \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member = definer.oid)"
    } else {
        ""
    };
    let query = format!(
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
         {schema_acl} \
         {definer_safety} \
         FROM pg_catalog.pg_roles r JOIN pg_catalog.pg_namespace n ON n.nspname = ? \
         JOIN pg_catalog.pg_roles owner ON owner.oid = n.nspowner \
         {definer_join} \
         JOIN pg_catalog.pg_database db ON db.datname = pg_catalog.current_database() \
         WHERE r.rolname = current_user"
    );
    let mut rows = if exact_schema_acl {
        tx.query(&query, crate::db_params![schema, expected_definer])
            .await?
    } else {
        tx.query(&query, crate::db_params![schema]).await?
    };
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
         AND (? = 1 OR NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,'UPDATE')) \
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
        crate::db_params![
            table.insert,
            table.update,
            table.delete,
            table.update,
            schema,
            table.name
        ],
    ).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(RoomEffectRuntimeAttachError::UnsafeRuntimePrivileges {
            component: table.name,
        });
    }
    Ok(())
}
