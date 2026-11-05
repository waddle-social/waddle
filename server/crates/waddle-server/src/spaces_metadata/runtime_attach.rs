//! Read-only validation shared by the durable space-store attachments.

use std::collections::HashMap;

use thiserror::Error;

use super::SPACES_METADATA_CONTRACT;
use crate::channel_space_links::CHANNEL_SPACE_LINK_CONTRACT;
use crate::db::lineage::{
    PgDatabaseIdentity, PgDatabaseName, PgIdentity, PgSchemaIdentity, PgSchemaName,
    PgSystemIdentifier,
};
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

const OWNER_GATE_KEY: i64 = 0x5753_4442_4f57_4e52;
const OWNER_MARKER: &str = "_waddle_space_store_owner";
// Bump the marker version when its row shape changes and each store's
// contract version whenever its prepared PostgreSQL shape changes.
const OWNER_MARKER_VERSION: i64 = 1;
const SPACES_METADATA_CONTRACT_VERSION: i64 = 1;
const CHANNEL_SPACE_LINKS_CONTRACT_VERSION: i64 = 1;

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
    #[error("space-store owner preparation requires a PostgreSQL owner database")]
    OwnerDatabaseRequired,
    #[error("the supplied PostgreSQL identity does not match the owner transaction")]
    OwnerIdentityMismatch,
    #[error("space-store preparation requires the schema's NOLOGIN owner role")]
    UnsafeOwnerRole,
    #[error("the space-store preparation marker is missing")]
    OwnerMarkerMissing,
    #[error("the space-store preparation marker is incomplete or does not match this boundary")]
    OwnerMarkerInvalid,
    #[error("temporary relation {table} shadows a managed space-store relation")]
    TemporaryRelationShadow { table: &'static str },
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

const OWNER_MARKER_COLUMNS: &[ColumnContract] = &[
    ColumnContract::plain("marker_id", ColumnKind::Bigint, true),
    ColumnContract::plain("marker_version", ColumnKind::Bigint, true),
    ColumnContract::plain("spaces_contract_version", ColumnKind::Bigint, true),
    ColumnContract::plain("channel_links_contract_version", ColumnKind::Bigint, true),
    ColumnContract::plain("system_identifier", ColumnKind::Text, true),
    ColumnContract::plain("database_oid", ColumnKind::Bigint, true),
    ColumnContract::plain("database_name", ColumnKind::Text, true),
    ColumnContract::plain("schema_oid", ColumnKind::Bigint, true),
    ColumnContract::plain("schema_name", ColumnKind::Text, true),
];

const OWNER_MARKER_CONTRACT: TableContract = TableContract {
    name: OWNER_MARKER,
    columns: OWNER_MARKER_COLUMNS,
    primary_key: &["marker_id"],
    indexes: &[],
};

const MANAGED_RELATIONS: &[&str] = &[
    OWNER_MARKER,
    "channel_space_links",
    "spaces_metadata",
    "spaces_metadata_new",
    "spaces_metadata_old",
];

#[derive(Clone, Copy)]
pub(crate) enum SpaceStoreKind {
    SpacesMetadata,
    ChannelSpaceLinks,
}

impl SpaceStoreKind {
    fn table(self) -> &'static str {
        match self {
            Self::SpacesMetadata => "spaces_metadata",
            Self::ChannelSpaceLinks => "channel_space_links",
        }
    }
}

/// Prepare both space-store tables on one trusted owner connection and bind
/// their completed schema contract to the supplied physical PostgreSQL
/// identity. This does not enroll or alter `_lineage`.
pub(crate) async fn prepare_space_stores_owner(
    db: Database,
    identity: &PgIdentity,
) -> Result<(), SpaceStoreAttachError> {
    #[cfg(test)]
    {
        prepare_space_stores_owner_inner(db, identity, None).await
    }
    #[cfg(not(test))]
    {
        prepare_space_stores_owner_inner(db, identity).await
    }
}

#[cfg(test)]
pub(crate) async fn prepare_space_stores_owner_paused_for_test(
    db: Database,
    identity: &PgIdentity,
    entered: tokio::sync::oneshot::Sender<()>,
    resume: tokio::sync::oneshot::Receiver<()>,
) -> Result<(), SpaceStoreAttachError> {
    prepare_space_stores_owner_inner(db, identity, Some((entered, resume))).await
}

async fn prepare_space_stores_owner_inner(
    db: Database,
    identity: &PgIdentity,
    #[cfg(test)] checkpoint: Option<(
        tokio::sync::oneshot::Sender<()>,
        tokio::sync::oneshot::Receiver<()>,
    )>,
) -> Result<(), SpaceStoreAttachError> {
    if db.driver() != DatabaseDriver::Postgres || db.uses_attested_postgres_pool() {
        return Err(SpaceStoreAttachError::OwnerDatabaseRequired);
    }
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "space-store owner schema")
        .map_err(|_| SpaceStoreAttachError::InvalidSchema)?;

    let mut tx = db.begin_immediate().await?;
    set_owner_transaction(&mut tx).await?;
    acquire_owner_gate(&mut tx, true).await?;
    verify_owner_boundary(&mut tx, identity).await?;
    set_local_search_path(&mut tx, schema).await?;
    verify_no_temporary_shadows(&mut tx).await?;

    lock_existing_relations(&mut tx, schema, MANAGED_RELATIONS, "ACCESS EXCLUSIVE").await?;
    let marker_exists = relation_info(&mut tx, schema, OWNER_MARKER)
        .await?
        .is_some();
    if !marker_exists {
        create_owner_marker(&mut tx, schema).await?;
    } else {
        verify_contract_shape(&mut tx, schema, &OWNER_MARKER_CONTRACT).await?;
        verify_existing_marker_if_present(&mut tx, schema, identity).await?;
    }
    tx.execute(
        &format!("DELETE FROM {}", qualified_relation(schema, OWNER_MARKER)),
        (),
    )
    .await?;

    #[cfg(test)]
    if let Some((entered, resume)) = checkpoint {
        let _ = entered.send(());
        let _ = resume.await;
    }

    super::schema::initialize_postgres_in_transaction(&mut tx, schema).await?;
    crate::channel_space_links::schema::initialize_postgres_in_transaction(&mut tx, schema).await?;
    lock_relations(
        &mut tx,
        schema,
        &[OWNER_MARKER, "channel_space_links", "spaces_metadata"],
        "ACCESS EXCLUSIVE",
    )
    .await?;

    verify_no_stale_rebuild_tables(&mut tx, schema).await?;
    for contract in [
        &SPACES_METADATA_CONTRACT,
        &CHANNEL_SPACE_LINK_CONTRACT,
        &OWNER_MARKER_CONTRACT,
    ] {
        verify_contract_shape(&mut tx, schema, contract).await?;
    }
    verify_owner_relation_ownership(&mut tx, schema).await?;
    write_owner_marker(&mut tx, schema, identity).await?;
    tx.commit().await?;
    Ok(())
}

/// Legacy open/initialize paths remain available for SQLite and ordinary
/// PostgreSQL startup. PostgreSQL mutations clear the shared preparation
/// marker before schema changes, in the same exclusive-gated transaction.
pub(crate) async fn initialize_raw_space_store(
    db: &Database,
    store: SpaceStoreKind,
) -> Result<(), SpaceStoreAttachError> {
    if db.driver() != DatabaseDriver::Postgres || db.uses_attested_postgres_pool() {
        return Err(SpaceStoreAttachError::OwnerDatabaseRequired);
    }

    let mut tx = db.begin_immediate().await?;
    set_owner_transaction(&mut tx).await?;
    acquire_owner_gate(&mut tx, true).await?;
    let schema = current_schema(&mut tx).await?;
    crate::db::physical_attestation::validate_identifier(&schema, "space-store schema")
        .map_err(|_| SpaceStoreAttachError::InvalidSchema)?;
    set_local_search_path(&mut tx, &schema).await?;
    verify_no_temporary_shadows(&mut tx).await?;

    let store_table = store.table();
    let relations: &[&'static str] = match store {
        SpaceStoreKind::SpacesMetadata => &[
            OWNER_MARKER,
            "spaces_metadata_new",
            "spaces_metadata_old",
            "spaces_metadata",
        ],
        SpaceStoreKind::ChannelSpaceLinks => &[OWNER_MARKER, "channel_space_links"],
    };
    lock_existing_relations(&mut tx, &schema, relations, "ACCESS EXCLUSIVE").await?;
    if relation_info(&mut tx, &schema, OWNER_MARKER)
        .await?
        .is_some()
    {
        verify_contract_shape(&mut tx, &schema, &OWNER_MARKER_CONTRACT).await?;
    } else {
        create_owner_marker(&mut tx, &schema).await?;
    }
    tx.execute(
        &format!("DELETE FROM {}", qualified_relation(&schema, OWNER_MARKER)),
        (),
    )
    .await?;

    match store {
        SpaceStoreKind::SpacesMetadata => {
            super::schema::initialize_postgres_in_transaction(&mut tx, &schema).await?;
        }
        SpaceStoreKind::ChannelSpaceLinks => {
            crate::channel_space_links::schema::initialize_postgres_in_transaction(
                &mut tx, &schema,
            )
            .await?;
        }
    }
    lock_relations(
        &mut tx,
        &schema,
        &[OWNER_MARKER, store_table],
        "ACCESS EXCLUSIVE",
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn set_owner_transaction(tx: &mut Transaction<'_>) -> Result<(), SpaceStoreAttachError> {
    // Keep the lock waiter's next statement on a fresh READ COMMITTED
    // snapshot; the owner gate serializes the cooperating DDL paths.
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await?;
    tx.execute("SET LOCAL lock_timeout = '5s'", ()).await?;
    tx.execute("SET LOCAL statement_timeout = '30s'", ())
        .await?;
    Ok(())
}

async fn acquire_owner_gate(
    tx: &mut Transaction<'_>,
    exclusive: bool,
) -> Result<(), SpaceStoreAttachError> {
    let sql = if exclusive {
        "SELECT pg_catalog.pg_advisory_xact_lock(?)"
    } else {
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)"
    };
    let mut rows = tx.query(sql, crate::db_params![OWNER_GATE_KEY]).await?;
    let _ = rows.next().await?;
    Ok(())
}

async fn current_schema(tx: &mut Transaction<'_>) -> Result<String, SpaceStoreAttachError> {
    let mut rows = tx
        .query("SELECT pg_catalog.current_schema()::text", ())
        .await?;
    let schema: Option<String> = rows
        .next()
        .await?
        .ok_or(SpaceStoreAttachError::ActiveSchemaMismatch)?
        .get(0)?;
    schema.ok_or(SpaceStoreAttachError::ActiveSchemaMismatch)
}

async fn set_local_search_path(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), SpaceStoreAttachError> {
    crate::db::physical_attestation::validate_identifier(schema, "space-store schema")
        .map_err(|_| SpaceStoreAttachError::InvalidSchema)?;
    tx.execute(
        &format!(
            "SET LOCAL search_path TO {}",
            crate::db::physical_attestation::quote_identifier(schema)
        ),
        (),
    )
    .await?;
    Ok(())
}

async fn verify_owner_boundary(
    tx: &mut Transaction<'_>,
    expected: &PgIdentity,
) -> Result<(), SpaceStoreAttachError> {
    let schema = &expected.schema.name.0;
    let mut rows = tx
        .query(
            "SELECT (SELECT system_identifier::text \
                      FROM pg_catalog.pg_control_system()), \
                    db.oid::bigint, db.datname::text, ns.oid::bigint, ns.nspname::text, \
                    ns.nspname = pg_catalog.current_schema(), \
                    owner.rolname = current_user AND NOT owner.rolcanlogin \
                      AND NOT owner.rolsuper AND NOT owner.rolcreatedb \
                      AND NOT owner.rolcreaterole AND NOT owner.rolreplication \
                      AND NOT owner.rolbypassrls AND ns.nspowner = owner.oid \
                      AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                                      WHERE membership.member = owner.oid) \
             FROM pg_catalog.pg_namespace ns \
             JOIN pg_catalog.pg_roles owner ON owner.oid = ns.nspowner \
             JOIN pg_catalog.pg_database db \
               ON db.datname = pg_catalog.current_database() \
             WHERE ns.nspname = ?",
            crate::db_params![schema],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(SpaceStoreAttachError::OwnerIdentityMismatch);
    };
    let system: String = row.get(0)?;
    let database_oid: i64 = row.get(1)?;
    let database_name: String = row.get(2)?;
    let schema_oid: i64 = row.get(3)?;
    let schema_name: String = row.get(4)?;
    let current_schema_matches: bool = row.get(5)?;
    let owner_role_is_safe: bool = row.get(6)?;
    let live = PgIdentity {
        system_identifier: system
            .parse::<PgSystemIdentifier>()
            .map_err(|_| SpaceStoreAttachError::OwnerIdentityMismatch)?,
        database: PgDatabaseIdentity {
            oid: u32::try_from(database_oid)
                .map_err(|_| SpaceStoreAttachError::OwnerIdentityMismatch)?,
            name: PgDatabaseName(database_name),
        },
        schema: PgSchemaIdentity {
            oid: u32::try_from(schema_oid)
                .map_err(|_| SpaceStoreAttachError::OwnerIdentityMismatch)?,
            name: PgSchemaName(schema_name),
        },
    };
    if &live != expected || !current_schema_matches {
        return Err(SpaceStoreAttachError::OwnerIdentityMismatch);
    }
    if !owner_role_is_safe {
        return Err(SpaceStoreAttachError::UnsafeOwnerRole);
    }
    Ok(())
}

async fn verify_no_temporary_shadows(
    tx: &mut Transaction<'_>,
) -> Result<(), SpaceStoreAttachError> {
    for &relation in MANAGED_RELATIONS {
        let mut rows = tx
            .query(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
                 WHERE c.relnamespace = pg_catalog.pg_my_temp_schema() \
                   AND c.relname = ?)",
                crate::db_params![relation],
            )
            .await?;
        if rows
            .next()
            .await?
            .ok_or(SpaceStoreAttachError::OwnerMarkerInvalid)?
            .get::<bool>(0)?
        {
            return Err(SpaceStoreAttachError::TemporaryRelationShadow { table: relation });
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) async fn verify_temporary_shadow_for_test(
    tx: &mut Transaction<'_>,
) -> Result<(), SpaceStoreAttachError> {
    verify_no_temporary_shadows(tx).await
}

async fn relation_info(
    tx: &mut Transaction<'_>,
    schema: &str,
    relation: &str,
) -> Result<Option<(String, String)>, SpaceStoreAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind::text, c.relpersistence::text \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, relation],
        )
        .await?;
    let info = match rows.next().await? {
        Some(row) => Some((row.get(0)?, row.get(1)?)),
        None => None,
    };
    Ok(info)
}

async fn lock_existing_relations(
    tx: &mut Transaction<'_>,
    schema: &str,
    relations: &[&'static str],
    mode: &'static str,
) -> Result<(), SpaceStoreAttachError> {
    let mut sorted = relations.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for relation in sorted {
        if let Some((kind, persistence)) = relation_info(tx, schema, relation).await? {
            if kind != "r" || persistence != "p" {
                return Err(SpaceStoreAttachError::InvalidTable { table: relation });
            }
            lock_relation(tx, schema, relation, mode).await?;
        }
    }
    Ok(())
}

async fn lock_relations(
    tx: &mut Transaction<'_>,
    schema: &str,
    relations: &[&'static str],
    mode: &'static str,
) -> Result<(), SpaceStoreAttachError> {
    let mut sorted = relations.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for relation in sorted {
        let Some((kind, persistence)) = relation_info(tx, schema, relation).await? else {
            return Err(if relation == OWNER_MARKER {
                SpaceStoreAttachError::OwnerMarkerMissing
            } else {
                SpaceStoreAttachError::InvalidTable { table: relation }
            });
        };
        if kind != "r" || persistence != "p" {
            return Err(SpaceStoreAttachError::InvalidTable { table: relation });
        }
        lock_relation(tx, schema, relation, mode).await?;
    }
    Ok(())
}

async fn lock_attested_relations(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), SpaceStoreAttachError> {
    let mut relations = vec![OWNER_MARKER, "channel_space_links", "spaces_metadata"];
    relations.sort_unstable();
    for relation in relations {
        let Some((kind, persistence)) = relation_info(tx, schema, relation).await? else {
            return Err(if relation == OWNER_MARKER {
                SpaceStoreAttachError::OwnerMarkerMissing
            } else {
                SpaceStoreAttachError::InvalidTable { table: relation }
            });
        };
        if kind != "r" || persistence != "p" {
            return Err(SpaceStoreAttachError::InvalidTable { table: relation });
        }
        let mode = if relation == OWNER_MARKER {
            // Runtime has SELECT-only access to the marker. ACCESS SHARE
            // protects its shape from ALTER/DROP while owner/raw writers
            // remain serialized by the exclusive advisory gate. A
            // privileged, non-cooperating CREATE INDEX CONCURRENTLY on the
            // marker is outside this restricted startup contract.
            "ACCESS SHARE"
        } else {
            "SHARE UPDATE EXCLUSIVE"
        };
        lock_relation(tx, schema, relation, mode).await?;
    }
    Ok(())
}

async fn lock_relation(
    tx: &mut Transaction<'_>,
    schema: &str,
    relation: &'static str,
    mode: &'static str,
) -> Result<(), SpaceStoreAttachError> {
    let sql = format!(
        "LOCK TABLE {} IN {mode} MODE",
        qualified_relation(schema, relation)
    );
    tx.execute(&sql, ()).await.map_err(|source| {
        if relation == OWNER_MARKER {
            SpaceStoreAttachError::Relation {
                table: OWNER_MARKER,
                source,
            }
        } else {
            SpaceStoreAttachError::Relation {
                table: relation,
                source,
            }
        }
    })?;
    Ok(())
}

fn qualified_relation(schema: &str, relation: &str) -> String {
    format!(
        "{}.{}",
        crate::db::physical_attestation::quote_identifier(schema),
        crate::db::physical_attestation::quote_identifier(relation)
    )
}

async fn create_owner_marker(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), SpaceStoreAttachError> {
    tx.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {} (\
               marker_id BIGINT PRIMARY KEY, \
               marker_version BIGINT NOT NULL, \
               spaces_contract_version BIGINT NOT NULL, \
               channel_links_contract_version BIGINT NOT NULL, \
               system_identifier TEXT NOT NULL, \
               database_oid BIGINT NOT NULL, \
               database_name TEXT NOT NULL, \
               schema_oid BIGINT NOT NULL, \
               schema_name TEXT NOT NULL\
             )",
            qualified_relation(schema, OWNER_MARKER)
        ),
        (),
    )
    .await?;
    Ok(())
}

async fn verify_contract_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), SpaceStoreAttachError> {
    verify_table(tx, schema, contract.name).await?;
    verify_columns(tx, schema, contract).await?;
    verify_primary_key(tx, schema, contract).await?;
    verify_indexes(tx, schema, contract).await?;
    Ok(())
}

async fn verify_existing_marker_if_present(
    tx: &mut Transaction<'_>,
    schema: &str,
    identity: &PgIdentity,
) -> Result<(), SpaceStoreAttachError> {
    let mut rows = tx
        .query(
            &format!(
                "SELECT COUNT(*)::bigint FROM {}",
                qualified_relation(schema, OWNER_MARKER)
            ),
            (),
        )
        .await?;
    let count: i64 = rows
        .next()
        .await?
        .ok_or(SpaceStoreAttachError::OwnerMarkerInvalid)?
        .get(0)?;
    drop(rows);
    if count == 0 {
        return Ok(());
    }
    verify_marker(tx, schema, identity).await
}

async fn verify_marker(
    tx: &mut Transaction<'_>,
    schema: &str,
    identity: &PgIdentity,
) -> Result<(), SpaceStoreAttachError> {
    let qualified = qualified_relation(schema, OWNER_MARKER);
    let mut rows = tx
        .query(&format!("SELECT COUNT(*)::bigint FROM {qualified}"), ())
        .await?;
    let count: i64 = rows
        .next()
        .await?
        .ok_or(SpaceStoreAttachError::OwnerMarkerInvalid)?
        .get(0)?;
    drop(rows);
    if count == 0 {
        return Err(SpaceStoreAttachError::OwnerMarkerMissing);
    }
    if count != 1 {
        return Err(SpaceStoreAttachError::OwnerMarkerInvalid);
    }
    let mut rows = tx
        .query(
            &format!(
                "SELECT marker_version, spaces_contract_version, \
                        channel_links_contract_version, system_identifier, \
                        database_oid, database_name, schema_oid, schema_name \
                 FROM {qualified} WHERE marker_id = 1"
            ),
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(SpaceStoreAttachError::OwnerMarkerInvalid);
    };
    let valid = row.get::<i64>(0)? == OWNER_MARKER_VERSION
        && row.get::<i64>(1)? == SPACES_METADATA_CONTRACT_VERSION
        && row.get::<i64>(2)? == CHANNEL_SPACE_LINKS_CONTRACT_VERSION
        && row.get::<String>(3)? == identity.system_identifier.to_string()
        && row.get::<i64>(4)? == i64::from(identity.database.oid)
        && row.get::<String>(5)? == identity.database.name.0
        && row.get::<i64>(6)? == i64::from(identity.schema.oid)
        && row.get::<String>(7)? == identity.schema.name.0;
    if !valid {
        return Err(SpaceStoreAttachError::OwnerMarkerInvalid);
    }
    Ok(())
}

async fn write_owner_marker(
    tx: &mut Transaction<'_>,
    schema: &str,
    identity: &PgIdentity,
) -> Result<(), SpaceStoreAttachError> {
    tx.execute(
        &format!(
            "INSERT INTO {} (marker_id, marker_version, spaces_contract_version, \
             channel_links_contract_version, system_identifier, database_oid, \
             database_name, schema_oid, schema_name) VALUES (1, ?, ?, ?, ?, ?, ?, ?, ?)",
            qualified_relation(schema, OWNER_MARKER)
        ),
        crate::db_params![
            OWNER_MARKER_VERSION,
            SPACES_METADATA_CONTRACT_VERSION,
            CHANNEL_SPACE_LINKS_CONTRACT_VERSION,
            identity.system_identifier.to_string(),
            i64::from(identity.database.oid),
            identity.database.name.0.as_str(),
            i64::from(identity.schema.oid),
            identity.schema.name.0.as_str(),
        ],
    )
    .await?;
    Ok(())
}

async fn verify_no_stale_rebuild_tables(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), SpaceStoreAttachError> {
    for relation in ["spaces_metadata_new", "spaces_metadata_old"] {
        if relation_info(tx, schema, relation).await?.is_some() {
            return Err(SpaceStoreAttachError::OwnerMarkerInvalid);
        }
    }
    Ok(())
}

async fn verify_owner_relation_ownership(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), SpaceStoreAttachError> {
    for relation in [OWNER_MARKER, "spaces_metadata", "channel_space_links"] {
        let mut rows = tx
            .query(
                "SELECT ns.nspowner = c.relowner AND NOT owner.rolcanlogin \
                         AND NOT owner.rolsuper AND NOT owner.rolcreatedb \
                         AND NOT owner.rolcreaterole AND NOT owner.rolreplication \
                         AND NOT owner.rolbypassrls \
                         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                                         WHERE membership.member = owner.oid) \
                 FROM pg_catalog.pg_class c \
                 JOIN pg_catalog.pg_namespace ns ON ns.oid = c.relnamespace \
                 JOIN pg_catalog.pg_roles owner ON owner.oid = ns.nspowner \
                 WHERE ns.nspname = ? AND c.relname = ? AND c.relkind = 'r'",
                crate::db_params![schema, relation],
            )
            .await?;
        let owner_is_safe = match rows.next().await? {
            Some(row) => row.get::<bool>(0)?,
            None => false,
        };
        if !owner_is_safe {
            return Err(SpaceStoreAttachError::UnsafeOwnerRole);
        }
    }
    Ok(())
}

/// Validate the complete owner-prepared space-store group on the exact
/// attested PostgreSQL main pool. Both store tables and their shared marker
/// are checked in one read-only transaction under the shared owner gate.
pub(crate) async fn verify_attested_store(
    db: &Database,
    _contract: &TableContract,
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
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
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

    acquire_owner_gate(&mut tx, false).await?;
    lock_attested_relations(&mut tx, schema).await?;
    verify_no_temporary_shadows(&mut tx).await?;

    // This proves the same physical database, schema OID, lineage, and login
    // attached the transaction used for the marker and both table contracts.
    capability.attest_transaction(&mut tx).await?;
    verify_active_schema(&mut tx, schema, i64::from(identity.schema.oid)).await?;
    verify_no_stale_rebuild_tables(&mut tx, schema).await?;
    for contract in [
        &SPACES_METADATA_CONTRACT,
        &CHANNEL_SPACE_LINK_CONTRACT,
        &OWNER_MARKER_CONTRACT,
    ] {
        verify_contract_shape(&mut tx, schema, contract).await?;
    }
    verify_owner_relation_ownership(&mut tx, schema).await?;
    verify_marker(&mut tx, schema, identity).await?;
    verify_marker_acl(&mut tx, schema).await?;
    verify_runtime_acl(&mut tx, schema, SPACES_METADATA_CONTRACT.name).await?;
    verify_runtime_acl(&mut tx, schema, CHANNEL_SPACE_LINK_CONTRACT.name).await?;

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
                    AND NOT c.relispartition AND c.reloftype = 0 \
                    AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con \
                                    WHERE con.conrelid = c.oid AND con.contype <> 'p') \
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
                row.get::<bool>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
                row.get::<Option<String>>(6)?,
            ),
        );
    }
    if actual.len() != contract.columns.len() {
        return Err(SpaceStoreAttachError::UnexpectedColumn {
            table: contract.name,
        });
    }
    for column in contract.columns {
        let Some((kind, not_null, default_collation, generated, identity, default)) =
            actual.get(column.name)
        else {
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
            || !*default_collation
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
            "SELECT a.attname::text, opc.opcdefault AND opc.opcintype = a.atttypid, \
                    collation_key.collation_oid = a.attcollation, \
                    i.indisvalid AND i.indisready AND i.indislive AND i.indimmediate \
                      AND i.indpred IS NULL AND i.indexprs IS NULL \
                      AND am.amname = 'btree', \
                    i.indnkeyatts::bigint, i.indnatts::bigint \
             FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_index i ON i.indexrelid = c.conindid \
             JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
             CROSS JOIN LATERAL pg_catalog.unnest(c.conkey) \
               WITH ORDINALITY k(attnum, position) \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             JOIN LATERAL pg_catalog.unnest(i.indclass) \
               WITH ORDINALITY opc_key(opclass_oid, position) \
               ON opc_key.position = k.position \
             JOIN LATERAL pg_catalog.unnest(i.indcollation) \
               WITH ORDINALITY collation_key(collation_oid, position) \
               ON collation_key.position = k.position \
             JOIN pg_catalog.pg_opclass opc ON opc.oid = opc_key.opclass_oid \
             WHERE c.contype = 'p' AND NOT c.condeferrable AND c.convalidated \
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
        actual.push((
            row.get::<String>(0)?,
            row.get::<bool>(1)?,
            row.get::<bool>(2)?,
            row.get::<bool>(3)?,
            row.get::<i64>(4)?,
            row.get::<i64>(5)?,
        ));
    }
    if actual.len() != contract.primary_key.len()
        || actual
            .iter()
            .map(|(column, _, _, _, _, _)| column.as_str())
            .collect::<Vec<_>>()
            != contract.primary_key
        || actual.iter().any(
            |(_, default_opclass, default_collation, exact_index, key_count, total_count)| {
                !default_opclass
                    || !default_collation
                    || !exact_index
                    || *key_count != contract.primary_key.len() as i64
                    || *total_count != contract.primary_key.len() as i64
            },
        )
    {
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
        let default_opclasses: bool = row.get(12)?;
        let default_collations: bool = row.get(13)?;
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
            || !default_opclasses
            || !default_collations
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
                    AND NOT owner.rolcanlogin AND NOT owner.rolsuper
                    AND NOT owner.rolcreatedb AND NOT owner.rolcreaterole
                    AND NOT owner.rolreplication AND NOT owner.rolbypassrls
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

async fn verify_marker_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), SpaceStoreAttachError> {
    let mut rows = tx
        .query(
            r#"SELECT ns.nspowner = owner.oid AND relation.relowner = owner.oid
                    AND NOT owner.rolcanlogin AND NOT owner.rolsuper
                    AND NOT owner.rolcreatedb AND NOT owner.rolcreaterole
                    AND NOT owner.rolreplication AND NOT owner.rolbypassrls
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid, 'INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT')
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
                      CASE WHEN pg_catalog.cardinality(relation.relacl) > 0
                        THEN relation.relacl
                        ELSE pg_catalog.acldefault('r', relation.relowner) END
                    ) acl WHERE acl.grantee NOT IN (owner.oid, runtime.oid)
                      OR (acl.grantee = runtime.oid AND (
                        acl.is_grantable OR acl.privilege_type <> 'SELECT')))
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
                      CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl
                      WHERE a.attrelid = relation.oid AND a.attnum > 0
                        AND NOT a.attisdropped)
             FROM pg_catalog.pg_roles owner
             JOIN pg_catalog.pg_roles runtime ON runtime.rolname = current_user
             JOIN pg_catalog.pg_namespace ns ON ns.nspname = ?
             JOIN pg_catalog.pg_class relation
               ON relation.relnamespace = ns.oid AND relation.relname = ?
                  AND relation.relkind = 'r'
             WHERE owner.oid = ns.nspowner"#,
            crate::db_params![schema, OWNER_MARKER],
        )
        .await
        .map_err(|source| SpaceStoreAttachError::Relation {
            table: OWNER_MARKER,
            source,
        })?;
    let safe = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !safe {
        return Err(SpaceStoreAttachError::UnsafeRuntimeRole);
    }
    Ok(())
}
