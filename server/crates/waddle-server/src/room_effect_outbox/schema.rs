use super::RoomEffectOutboxError;
use crate::db::ConnectionGuard;
use crate::db::lineage::{PgIdentity, PgSystemIdentifier};
use crate::db::{CatalogFenceLock, Database, DatabaseDriver, Transaction};

/// Dedicated transaction-scoped Postgres advisory lock for room-effect outbox
/// bootstrap. It is distinct from the claims (`…991`), migrations (`…992`),
/// lineage (`…993`), room schema (`…994`), and destroy outbox (`…995`) keys.
const ROOM_EFFECT_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_996;

pub(super) async fn initialize(db: &Database) -> Result<(), RoomEffectOutboxError> {
    match db.driver() {
        DatabaseDriver::Postgres => postgres(db).await,
        DatabaseDriver::Sqlite => sqlite(db).await,
    }
}

/// Owner bootstrap is confined to the independently supplied physical
/// identity and a single schema. Existing outboxes must already match the
/// current contract; this operation never rewrites persisted effects.
pub(super) async fn initialize_owner(
    db: &Database,
    identity: &PgIdentity,
) -> Result<(), RoomEffectOutboxError> {
    if db.driver() != DatabaseDriver::Postgres || db.uses_attested_postgres_pool() {
        return Err(RoomEffectOutboxError::OwnerPreparationRequired);
    }
    let mut tx = db
        .begin_catalog_fenced(&[CatalogFenceLock::Exclusive(
            ROOM_EFFECT_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY,
        )])
        .await?;
    pin_owner_schema(&mut tx, identity).await?;
    let schema = &identity.schema.name.0;
    let exists = super::runtime_attach::verify_existing_owner(&mut tx, schema)
        .await
        .map_err(|_| RoomEffectOutboxError::UnsafeOwnerBoundary)?;
    if !exists {
        postgres_schema(&mut tx).await?;
        super::runtime_attach::verify_existing_owner(&mut tx, schema)
            .await
            .map_err(|_| RoomEffectOutboxError::UnsafeOwnerBoundary)?;
    }
    tx.commit().await?;
    Ok(())
}

pub(crate) async fn pin_owner_schema(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), RoomEffectOutboxError> {
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "room-effect owner schema")
        .map_err(|_| RoomEffectOutboxError::UnsafeOwnerBoundary)?;
    let mut rows = tx.query(
        "SELECT (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
         db.oid::bigint, db.datname::text, n.oid::bigint, n.nspname::text, \
         n.nspname = pg_catalog.current_schema() AND owner.rolname = current_user \
         AND NOT owner.rolcanlogin AND NOT owner.rolsuper AND NOT owner.rolcreatedb \
         AND NOT owner.rolcreaterole AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c WHERE c.relnamespace = pg_catalog.pg_my_temp_schema()) \
         FROM pg_catalog.pg_namespace n JOIN pg_catalog.pg_roles owner ON owner.oid = n.nspowner \
         JOIN pg_catalog.pg_database db ON db.datname = pg_catalog.current_database() \
         WHERE n.nspname = ?", crate::db_params![schema],
    ).await?;
    let valid = match rows.next().await? {
        Some(row) => {
            let system: String = row.get(0)?;
            system.parse::<PgSystemIdentifier>().ok() == Some(identity.system_identifier)
                && row.get::<i64>(1)? == i64::from(identity.database.oid)
                && row.get::<String>(2)? == identity.database.name.0
                && row.get::<i64>(3)? == i64::from(identity.schema.oid)
                && row.get::<String>(4)? == *schema
                && row.get::<bool>(5)?
        }
        None => false,
    };
    if !valid {
        return Err(RoomEffectOutboxError::UnsafeOwnerBoundary);
    }
    let quoted = crate::db::physical_attestation::quote_identifier(schema);
    tx.query(
        "SELECT pg_catalog.set_config('search_path', ?, true)",
        crate::db_params![format!("{quoted}, pg_temp")],
    )
    .await?;
    Ok(())
}

async fn postgres(db: &Database) -> Result<(), RoomEffectOutboxError> {
    let mut tx = db
        .begin_catalog_fenced(&[CatalogFenceLock::Exclusive(
            ROOM_EFFECT_OUTBOX_SCHEMA_ADVISORY_LOCK_KEY,
        )])
        .await?;
    postgres_schema(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn postgres_schema(tx: &mut Transaction<'_>) -> Result<(), RoomEffectOutboxError> {
    tx.execute("CREATE TABLE IF NOT EXISTS clustering_muc_room_effects (lifecycle_id TEXT NOT NULL, revision BIGINT NOT NULL CHECK (revision >= 1), ordinal BIGINT NOT NULL CHECK (ordinal >= 0), room_jid TEXT NOT NULL CHECK (room_jid <> ''), kind TEXT NOT NULL CHECK (kind <> ''), terminal BOOLEAN NOT NULL, payload_json TEXT NOT NULL, available_at_ms BIGINT NOT NULL, superseded BOOLEAN NOT NULL DEFAULT FALSE, origin_instance_id TEXT NOT NULL, producing_node TEXT NOT NULL, lease_token TEXT NULL, leased_at_ms BIGINT NULL, attempt_count BIGINT NOT NULL DEFAULT 0, last_error TEXT NULL, created_at_ms BIGINT NOT NULL, unowned_since_ms BIGINT NULL, PRIMARY KEY (lifecycle_id, revision, ordinal))", ()).await?;
    tx.execute(
        "ALTER TABLE clustering_muc_room_effects ADD COLUMN IF NOT EXISTS unowned_since_ms BIGINT",
        (),
    )
    .await?;
    tx.execute("DO $$ BEGIN IF NOT EXISTS (SELECT 1 FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid WHERE i.indrelid = 'clustering_muc_room_effects'::regclass AND c.relname = 'clustering_muc_room_effects_due_idx') THEN CREATE INDEX clustering_muc_room_effects_due_idx ON clustering_muc_room_effects (available_at_ms, lifecycle_id); END IF; END $$", ()).await?;
    Ok(())
}
async fn sqlite(db: &Database) -> Result<(), RoomEffectOutboxError> {
    let connection = db.guard().await?;
    connection.execute("CREATE TABLE IF NOT EXISTS clustering_muc_room_effects (lifecycle_id TEXT NOT NULL, revision INTEGER NOT NULL CHECK (revision >= 1), ordinal INTEGER NOT NULL CHECK (ordinal >= 0), room_jid TEXT NOT NULL CHECK (room_jid <> ''), kind TEXT NOT NULL CHECK (kind <> ''), terminal BOOLEAN NOT NULL, payload_json TEXT NOT NULL, available_at_ms INTEGER NOT NULL, superseded BOOLEAN NOT NULL DEFAULT FALSE, origin_instance_id TEXT NOT NULL, producing_node TEXT NOT NULL, lease_token TEXT NULL, leased_at_ms INTEGER NULL, attempt_count INTEGER NOT NULL DEFAULT 0, last_error TEXT NULL, created_at_ms INTEGER NOT NULL, unowned_since_ms INTEGER NULL, PRIMARY KEY (lifecycle_id, revision, ordinal))", ()).await?;
    if !sqlite_column_present(&connection, "unowned_since_ms").await? {
        connection
            .execute(
                "ALTER TABLE clustering_muc_room_effects ADD COLUMN unowned_since_ms INTEGER",
                (),
            )
            .await?;
    }
    connection.execute("CREATE INDEX IF NOT EXISTS clustering_muc_room_effects_due_idx ON clustering_muc_room_effects (available_at_ms, lifecycle_id)", ()).await?;
    Ok(())
}

async fn sqlite_column_present(
    connection: &ConnectionGuard,
    column: &str,
) -> Result<bool, RoomEffectOutboxError> {
    let mut rows = connection
        .query("PRAGMA table_info(clustering_muc_room_effects)", ())
        .await?;
    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}
