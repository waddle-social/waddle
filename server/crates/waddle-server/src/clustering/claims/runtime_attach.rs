//! Physical same-main-pool verification for claim and occupancy authority.
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use std::collections::HashMap;

#[derive(Debug, thiserror::Error)]
pub enum ClaimOccupancyAttachError {
    #[error("claim and occupancy attachment requires an attested PostgreSQL database")]
    AttestedPostgresRequired,
    #[error("claim and occupancy attachment requires the main database lane")]
    MainLaneRequired,
    #[error("claim and occupancy active schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not bound PostgreSQL waits for claim and occupancy attachment")]
    ReadBounds,
    #[error("failed to lock authority relation {table}: {source}")]
    Relation {
        table: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("authority relation {table} has an unexpected shape")]
    InvalidTable { table: &'static str },
    #[error("authority relation {table} has an invalid column {column}")]
    InvalidColumn {
        table: &'static str,
        column: &'static str,
    },
    #[error("authority relation {table} has invalid constraints")]
    InvalidConstraint { table: &'static str },
    #[error("authority relation {table} has invalid indexes")]
    InvalidIndex { table: &'static str },
    #[error("authority runtime role or privileges are unsafe at {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error("claim epoch sequence is missing, unsafe, exhausted, or behind retained claims")]
    InvalidClaimSequence,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Evidence of claims and occupancy structure on one physically attested
/// main pool. It does not authorize mutations, validate the control-plane
/// lane, establish historical sequence provenance, or activate an epoch.
pub struct VerifiedMainClaimOccupancy {
    capability: AttestedPostgresWriterCapability,
    _db: Database,
}
impl VerifiedMainClaimOccupancy {
    /// Test the exact attested physical pool and main lane, without exposing
    /// a raw transaction or conferring authority to mutate it.
    pub fn belongs_to_main_pool(&self, candidate: &Database) -> bool {
        candidate.is_main_lane()
            && candidate
                .verify_attested_pg_writer_capability(&self.capability)
                .is_ok()
    }
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

const CLAIM_COLUMNS: &[Column] = &[
    text("entity", true),
    text("entity_type", true),
    text("node_id", true),
    text("node_epoch", true),
    Column {
        name: "claim_epoch",
        kind: "bigint",
        not_null: true,
        identity: "",
        default: Some("nextval('clustering_claim_epoch_seq'::regclass)"),
    },
];
const OCCUPANCY_COLUMNS: &[Column] = &[text("full_jid", true), text("generation", true)];
const CLAIM_INDEXES: &[Index] = &[
    Index {
        name: "clustering_claims_node_id_node_epoch",
        columns: "node_id,node_epoch",
        options: "0,0",
    },
    Index {
        name: "clustering_claims_owner_type_entity",
        columns: "node_id,node_epoch,entity_type,entity",
        options: "0,0,0,0",
    },
    Index {
        name: "clustering_claims_entity_type_entity",
        columns: "entity_type,entity",
        options: "0,0",
    },
];
const TABLES: &[Table] = &[
    Table {
        name: "clustering_claims",
        columns: CLAIM_COLUMNS,
        primary: "entity",
        checks: &[],
        indexes: CLAIM_INDEXES,
        insert: true,
        update: true,
        delete: true,
    },
    Table {
        name: "xmpp_occupancy_authority",
        columns: OCCUPANCY_COLUMNS,
        primary: "full_jid",
        checks: &[],
        indexes: &[],
        insert: true,
        update: true,
        delete: false,
    },
];

pub(super) async fn verify(
    db: Database,
) -> Result<VerifiedMainClaimOccupancy, ClaimOccupancyAttachError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(ClaimOccupancyAttachError::AttestedPostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(ClaimOccupancyAttachError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(ClaimOccupancyAttachError::AttestedPostgresRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;
    let identity = capability.identity();
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "claim authority schema")
        .map_err(|_| ClaimOccupancyAttachError::ActiveSchemaMismatch)?;
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
        .map_err(|_| ClaimOccupancyAttachError::ReadBounds)?;
    for table in TABLES {
        tx.execute(
            &format!(
                "LOCK TABLE {quoted}.\"{}\" IN ACCESS SHARE MODE",
                table.name
            ),
            (),
        )
        .await
        .map_err(|source| ClaimOccupancyAttachError::Relation {
            table: table.name,
            source,
        })?;
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
        return Err(ClaimOccupancyAttachError::ActiveSchemaMismatch);
    }
    verify_runtime_role(&mut tx, schema).await?;
    for table in TABLES {
        verify_table(&mut tx, schema, table).await?;
        verify_columns(&mut tx, schema, table).await?;
        verify_constraints(&mut tx, schema, table).await?;
        verify_indexes(&mut tx, schema, table).await?;
        verify_table_acl(&mut tx, schema, table).await?;
    }
    verify_sequence(&mut tx, schema, &quoted).await?;
    tx.rollback().await?;
    Ok(VerifiedMainClaimOccupancy {
        capability,
        _db: db,
    })
}

async fn verify_table(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), ClaimOccupancyAttachError> {
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
        return Err(ClaimOccupancyAttachError::InvalidTable { table: table.name });
    }
    Ok(())
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), ClaimOccupancyAttachError> {
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
        return Err(ClaimOccupancyAttachError::InvalidTable { table: table.name });
    }
    for column in table.columns {
        let Some((kind, not_null, generated, identity, default, default_collation)) =
            actual.get(column.name)
        else {
            return Err(ClaimOccupancyAttachError::InvalidColumn {
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
            return Err(ClaimOccupancyAttachError::InvalidColumn {
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
) -> Result<(), ClaimOccupancyAttachError> {
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
            return Err(ClaimOccupancyAttachError::InvalidConstraint { table: table.name });
        }
        match kind.as_str() {
            "p" => {
                primary = !primary && row.get::<bool>(4)? && row.get::<String>(5)? == table.primary;
            }
            "c" => checks.push(row.get::<String>(6)?),
            _ => return Err(ClaimOccupancyAttachError::InvalidConstraint { table: table.name }),
        }
    }
    checks.sort_unstable();
    let mut expected = table.checks.to_vec();
    expected.sort_unstable();
    if !primary
        || count != 1 + table.checks.len()
        || checks.iter().map(String::as_str).collect::<Vec<_>>() != expected
    {
        return Err(ClaimOccupancyAttachError::InvalidConstraint { table: table.name });
    }
    Ok(())
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), ClaimOccupancyAttachError> {
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
        .ok_or(ClaimOccupancyAttachError::InvalidIndex { table: table.name })?
        .get(0)?;
    if count != (1 + table.indexes.len()) as i64 {
        return Err(ClaimOccupancyAttachError::InvalidIndex { table: table.name });
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
            return Err(ClaimOccupancyAttachError::InvalidIndex { table: table.name });
        }
    }
    Ok(())
}

async fn verify_runtime_role(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), ClaimOccupancyAttachError> {
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
        return Err(ClaimOccupancyAttachError::UnsafeRuntimePrivileges {
            component: "runtime role",
        });
    }
    Ok(())
}

async fn verify_table_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &Table,
) -> Result<(), ClaimOccupancyAttachError> {
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
        return Err(ClaimOccupancyAttachError::UnsafeRuntimePrivileges {
            component: table.name,
        });
    }
    Ok(())
}
async fn verify_sequence(
    tx: &mut Transaction<'_>,
    schema: &str,
    quoted: &str,
) -> Result<(), ClaimOccupancyAttachError> {
    let mut rows = tx.query(
        "SELECT seq.relkind = 'S' AND seq.relpersistence = 'p' AND seq.relowner = n.nspowner \
         AND s.seqtypid = 'pg_catalog.int8'::pg_catalog.regtype AND s.seqstart = 1 AND s.seqincrement = 1 \
         AND s.seqmin = 0 AND s.seqmax = 9223372036854775807 AND s.seqcache = 1 AND NOT s.seqcycle \
         AND pg_catalog.has_sequence_privilege(r.oid,seq.oid,'SELECT') \
         AND pg_catalog.has_sequence_privilege(r.oid,seq.oid,'USAGE') \
         AND NOT pg_catalog.has_sequence_privilege(r.oid,seq.oid,'UPDATE') \
         AND NOT pg_catalog.has_sequence_privilege(r.oid,seq.oid,'SELECT WITH GRANT OPTION,USAGE WITH GRANT OPTION,UPDATE WITH GRANT OPTION') \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(seq.relacl,pg_catalog.acldefault('S',seq.relowner))) x \
           WHERE x.grantee NOT IN (seq.relowner,r.oid) OR (x.grantee = r.oid AND (x.privilege_type NOT IN ('SELECT','USAGE') OR x.is_grantable))) \
         AND EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a \
           JOIN pg_catalog.pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
           JOIN pg_catalog.pg_depend dep ON dep.classid = 'pg_catalog.pg_attrdef'::pg_catalog.regclass \
             AND dep.objid = d.oid AND dep.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass \
             AND dep.refobjid = seq.oid AND dep.deptype = 'n' \
           WHERE a.attrelid = t.oid AND a.attname = 'claim_epoch' AND NOT a.attisdropped) \
         FROM pg_catalog.pg_class t JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
         JOIN pg_catalog.pg_class seq ON seq.relnamespace = n.oid AND seq.relname = 'clustering_claim_epoch_seq' \
         JOIN pg_catalog.pg_sequence s ON s.seqrelid = seq.oid \
         JOIN pg_catalog.pg_roles r ON r.rolname = current_user \
         WHERE n.nspname = ? AND t.relname = 'clustering_claims'", crate::db_params![schema],
    ).await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(ClaimOccupancyAttachError::InvalidClaimSequence);
    }
    let mut state = tx
        .query(
            &format!(
                "SELECT last_value::bigint, is_called FROM {quoted}.clustering_claim_epoch_seq"
            ),
            (),
        )
        .await?;
    let state = state
        .next()
        .await?
        .ok_or(ClaimOccupancyAttachError::InvalidClaimSequence)?;
    let last: i64 = state.get(0)?;
    let called: bool = state.get(1)?;
    let mut range = tx
        .query(
            &format!("SELECT min(claim_epoch), max(claim_epoch) FROM {quoted}.clustering_claims"),
            (),
        )
        .await?;
    let range = range
        .next()
        .await?
        .ok_or(ClaimOccupancyAttachError::InvalidClaimSequence)?;
    let min: Option<i64> = range.get(0)?;
    let max: Option<i64> = range.get(1)?;
    if last < 1
        || last == i64::MAX
        || min.is_some_and(|v| v < 1)
        || max.is_some_and(|v| !called || v > last)
        || (!called && last != 1)
    {
        return Err(ClaimOccupancyAttachError::InvalidClaimSequence);
    }
    Ok(())
}
