//! Trusted PostgreSQL owner preparation for the Push Service schema.

use crate::db::lineage::PgIdentity;
use crate::db::{CatalogFenceLock, Database, DatabaseDriver, DatabaseError, Transaction};

pub(super) const PUSH_SCHEMA_ADVISORY_LOCK_KEY: i64 = 6_841_445_497_037_937_998;
const PUSH_SCHEMA_MARKER_TABLE: &str = "push_service_schema_marker";
const PUSH_SCHEMA_VERSION: i64 = 1;
const PUSH_OWNER_PHASE_INCOMPLETE: i64 = 0;
const PUSH_OWNER_PHASE_COMPLETE: i64 = 1;
const PUSH_RELATIONS_IN_LOCK_ORDER: [&str; 10] = [
    "push_owner_locks",
    "push_nodes",
    "push_node_locks",
    "push_devices",
    "push_delivery_attempts",
    "push_publish_jobs",
    "push_publication_orders",
    super::runtime_attach::VAPID_TABLE_NAME,
    super::runtime_attach::VAPID_MARKER_TABLE_NAME,
    PUSH_SCHEMA_MARKER_TABLE,
];

#[derive(Debug, thiserror::Error)]
pub enum PushServiceOwnerError {
    #[error("Push Service owner preparation requires PostgreSQL")]
    PostgresRequired,
    #[error("Push Service owner preparation cannot use an attested runtime handle")]
    OwnerHandleRequired,
    #[error("Push Service owner preparation identity is invalid")]
    InvalidIdentity,
    #[error("Push Service owner handle does not match the exact NOLOGIN schema owner identity")]
    UnsafeOwnerBoundary,
    #[error("Push Service schema marker is malformed or belongs to a different identity")]
    InvalidMarker,
    #[error(
        "Push Service schema contains a relation that is not owned by its NOLOGIN schema owner"
    )]
    UnsafeExistingRelation,
    #[error("Push Service raw PostgreSQL startup found a temp relation that shadows its schema")]
    UnsafeTemporaryRelation,
    #[error("Push Service schema has an inbound foreign key")]
    InboundForeignKey,
    #[error(
        "notification outbox owner preparation must finish before Push Service ancestry adoption"
    )]
    NotificationOutboxNotPrepared,
    #[error("Push Service owner schema migration failed: {0}")]
    SchemaMigration(#[source] waddle_xmpp::XmppError),
    #[error("Push Service ancestry adoption failed: {0}")]
    AncestryAdoption(#[source] waddle_xmpp::XmppError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Prepare or upgrade the local Push Service schema in one pinned owner
/// transaction. This phase commits an incomplete marker. The separate
/// ancestry owner phase marks it complete only after cross-store adoption.
pub(super) async fn initialize_owner(
    db: &Database,
    identity: &PgIdentity,
) -> Result<(), PushServiceOwnerError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(PushServiceOwnerError::PostgresRequired);
    }
    if db.uses_attested_postgres_pool() {
        return Err(PushServiceOwnerError::OwnerHandleRequired);
    }

    let mut tx = db
        .begin_catalog_fenced(&[CatalogFenceLock::Exclusive(PUSH_SCHEMA_ADVISORY_LOCK_KEY)])
        .await?;
    configure_owner_transaction(&mut tx).await?;
    pin_owner_schema(&mut tx, identity).await?;
    lock_existing_push_relations_tx(&mut tx, &identity.schema.name.0).await?;

    let marker_exists = marker_exists_tx(&mut tx, &identity.schema.name.0).await?;
    if marker_exists {
        super::runtime_attach::verify_marker_shape_and_data_tx(&mut tx, identity)
            .await
            .map_err(|_| PushServiceOwnerError::InvalidMarker)?;
        let marker = qualified_marker(&identity.schema.name.0);
        tx.execute(
            &format!("UPDATE {marker} SET owner_phase = ? WHERE marker_id = 1"),
            crate::db_params![PUSH_OWNER_PHASE_INCOMPLETE],
        )
        .await?;
    }
    verify_existing_relations_tx(&mut tx, &identity.schema.name.0).await?;

    super::DatabasePushServiceStore::initialize_schema_tx(DatabaseDriver::Postgres, &mut tx)
        .await
        .map_err(PushServiceOwnerError::SchemaMigration)?;

    if !marker_exists {
        create_marker_tx(&mut tx, identity).await?;
    }
    let ready = super::runtime_attach::verify_marker_shape_and_data_tx(&mut tx, identity)
        .await
        .map_err(|_| PushServiceOwnerError::InvalidMarker)?;
    if ready {
        return Err(PushServiceOwnerError::InvalidMarker);
    }
    tx.commit().await?;
    Ok(())
}

/// Prepare the first durable VAPID signer after the regular Push Service
/// owner phase is complete. DDL, initial key insertion, and the VAPID-specific
/// completion marker commit atomically under the shared Push schema gate.
pub(super) async fn prepare_vapid_owner(
    db: &Database,
    identity: &PgIdentity,
    root_key: &[u8],
) -> Result<(), super::vapid_storage::VapidOwnerError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(super::vapid_storage::VapidOwnerError::PostgresRequired);
    }
    if db.uses_attested_postgres_pool() {
        return Err(super::vapid_storage::VapidOwnerError::OwnerHandleRequired);
    }

    let mut tx = db
        .begin_catalog_fenced(&[CatalogFenceLock::Exclusive(PUSH_SCHEMA_ADVISORY_LOCK_KEY)])
        .await?;
    configure_owner_transaction(&mut tx).await?;
    pin_owner_schema(&mut tx, identity).await?;
    lock_existing_push_relations_tx(&mut tx, &identity.schema.name.0).await?;
    let push_ready =
        super::runtime_attach::verify_marker_shape_and_data_tx(&mut tx, identity).await?;
    if !push_ready {
        return Err(super::vapid_storage::VapidOwnerError::PushServiceNotReady);
    }
    super::vapid_storage::prepare_vapid_owner_tx(&mut tx, identity, root_key).await?;
    tx.commit().await?;
    Ok(())
}

/// Adopt the exact legacy-to-canonical Push Service ancestry bindings while
/// holding the notification outbox shared gate through the Push Service gate.
/// Outbox owner/raw code never acquires the Push Service gate, so this order
/// is acyclic and prevents outbox invalidation during ancestry certification.
pub(super) async fn adopt_notification_ancestry_owner(
    db: &Database,
    identity: &PgIdentity,
) -> Result<(), PushServiceOwnerError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(PushServiceOwnerError::PostgresRequired);
    }
    if db.uses_attested_postgres_pool() {
        return Err(PushServiceOwnerError::OwnerHandleRequired);
    }

    let mut tx = db
        .begin_catalog_fenced(&[
            CatalogFenceLock::SharedPair(
                crate::notification_outbox::outbox_schema_gate().0,
                crate::notification_outbox::outbox_schema_gate().1,
            ),
            CatalogFenceLock::Exclusive(PUSH_SCHEMA_ADVISORY_LOCK_KEY),
        ])
        .await?;
    configure_owner_transaction(&mut tx).await?;
    crate::notification_outbox::NotificationOutboxStore::verify_owner_preparation_complete_tx(
        &mut tx, identity,
    )
    .await
    .map_err(|_| PushServiceOwnerError::NotificationOutboxNotPrepared)?;
    pin_owner_schema(&mut tx, identity).await?;
    lock_existing_push_relations_tx(&mut tx, &identity.schema.name.0).await?;
    super::runtime_attach::verify_marker_shape_and_data_tx(&mut tx, identity)
        .await
        .map_err(|_| PushServiceOwnerError::InvalidMarker)?;
    if !super::publish_jobs::has_notification_lineage_tx(&mut tx)
        .await
        .map_err(|_| PushServiceOwnerError::NotificationOutboxNotPrepared)?
        || !super::publish_jobs::has_notification_outbox_frozen_schema_tx(&mut tx)
            .await
            .map_err(|_| PushServiceOwnerError::NotificationOutboxNotPrepared)?
    {
        return Err(PushServiceOwnerError::NotificationOutboxNotPrepared);
    }

    super::publish_jobs::adopt_notification_ancestry_tx(&mut tx)
        .await
        .map_err(PushServiceOwnerError::AncestryAdoption)?;
    let marker = qualified_marker(&identity.schema.name.0);
    tx.execute(
        &format!("UPDATE {marker} SET owner_phase = ? WHERE marker_id = 1"),
        crate::db_params![PUSH_OWNER_PHASE_COMPLETE],
    )
    .await?;
    if !super::runtime_attach::verify_marker_shape_and_data_tx(&mut tx, identity)
        .await
        .map_err(|_| PushServiceOwnerError::InvalidMarker)?
    {
        return Err(PushServiceOwnerError::InvalidMarker);
    }
    tx.commit().await?;
    Ok(())
}

async fn configure_owner_transaction(
    tx: &mut Transaction<'_>,
) -> Result<(), PushServiceOwnerError> {
    // begin_catalog_fenced starts a fresh READ COMMITTED, READ WRITE
    // transaction and acquires catalog-X plus its gates before this setup.
    tx.execute("SET LOCAL lock_timeout = '30s'", ()).await?;
    tx.execute("SET LOCAL statement_timeout = '300000ms'", ())
        .await?;
    Ok(())
}

async fn pin_owner_schema(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), PushServiceOwnerError> {
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "Push Service owner schema")
        .map_err(|_| PushServiceOwnerError::InvalidIdentity)?;
    let mut rows = tx
        .query(
            "SELECT (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
             database.oid::bigint, database.datname::text, namespace.oid::bigint, \
             namespace.nspname::text, \
             namespace.nspname = pg_catalog.current_schema() \
               AND COALESCE((pg_catalog.current_schemas(false))[1]::text, '') = namespace.nspname \
               AND pg_catalog.cardinality(pg_catalog.current_schemas(false)) <= 2 \
               AND (pg_catalog.cardinality(pg_catalog.current_schemas(false)) = 1 \
                 OR pg_catalog.left((pg_catalog.current_schemas(false))[2]::text, 8) = 'pg_temp_') \
               AND NOT pg_catalog.pg_is_in_recovery() \
               AND owner.rolname = current_user \
               AND NOT owner.rolcanlogin AND NOT owner.rolsuper \
               AND NOT owner.rolcreatedb AND NOT owner.rolcreaterole \
               AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
               AND owner.oid <> database.datdba \
               AND NOT pg_catalog.has_database_privilege(owner.oid, database.oid, 'TEMP') \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                               WHERE membership.member = owner.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                 COALESCE(namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                 WHERE acl.grantee = 0 OR (acl.grantee <> namespace.nspowner \
                   AND (acl.privilege_type <> 'USAGE' OR acl.is_grantable))) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class temp_relation \
                               WHERE temp_relation.relnamespace = pg_catalog.pg_my_temp_schema()) \
             FROM pg_catalog.pg_namespace namespace \
             JOIN pg_catalog.pg_roles owner ON owner.oid = namespace.nspowner \
             JOIN pg_catalog.pg_database database \
               ON database.datname = pg_catalog.current_database() \
             WHERE namespace.nspname = ?",
            crate::db_params![schema],
        )
        .await?;
    let valid = match rows.next().await? {
        Some(row) => {
            let system_identifier: String = row.get(0)?;
            system_identifier == identity.system_identifier.to_string()
                && row.get::<i64>(1)? == i64::from(identity.database.oid)
                && row.get::<String>(2)? == identity.database.name.0
                && row.get::<i64>(3)? == i64::from(identity.schema.oid)
                && row.get::<String>(4)? == *schema
                && row.get::<bool>(5)?
        }
        None => false,
    };
    if !valid {
        return Err(PushServiceOwnerError::UnsafeOwnerBoundary);
    }
    let quoted = crate::db::physical_attestation::quote_identifier(schema);
    tx.query(
        "SELECT pg_catalog.set_config('search_path', ?, true)",
        crate::db_params![format!("{quoted}, pg_temp")],
    )
    .await?;
    Ok(())
}

async fn marker_exists_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<bool, PushServiceOwnerError> {
    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = 'push_service_schema_marker')",
            crate::db_params![schema],
        )
        .await?;
    Ok(matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?))
}

async fn create_marker_tx(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), PushServiceOwnerError> {
    tx.execute(
        "CREATE TABLE push_service_schema_marker (\
           marker_id INTEGER PRIMARY KEY CHECK (marker_id = 1), \
           version BIGINT NOT NULL CHECK (version = 1), \
           owner_phase BIGINT NOT NULL CHECK (owner_phase IN (0, 1)), \
           system_identifier TEXT NOT NULL, \
           database_oid BIGINT NOT NULL, \
           database_name TEXT NOT NULL, \
           schema_oid BIGINT NOT NULL, \
           schema_name TEXT NOT NULL)",
        (),
    )
    .await?;
    tx.execute(
        "INSERT INTO push_service_schema_marker \
           (marker_id, version, owner_phase, system_identifier, database_oid, database_name, schema_oid, schema_name) \
         VALUES (1, ?, ?, ?, ?, ?, ?, ?)",
        crate::db_params![
            PUSH_SCHEMA_VERSION,
            PUSH_OWNER_PHASE_INCOMPLETE,
            identity.system_identifier.to_string(),
            i64::from(identity.database.oid),
            identity.database.name.0.clone(),
            i64::from(identity.schema.oid),
            identity.schema.name.0.clone(),
        ],
    )
    .await?;
    Ok(())
}

/// Make a completed owner marker unusable before a legacy raw initializer can
/// run any DDL or data migration. This is deliberately one-way: only the
/// explicit owner ancestry phase can restore completion.
pub(super) async fn invalidate_completion_marker_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PushServiceOwnerError> {
    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = 'push_service_schema_marker')",
            crate::db_params![schema],
        )
        .await?;
    let exists = matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?);
    drop(rows);
    if exists {
        let marker = qualified_marker(schema);
        tx.execute(
            &format!("UPDATE {marker} SET owner_phase = ? WHERE marker_id = 1"),
            crate::db_params![PUSH_OWNER_PHASE_INCOMPLETE],
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn verify_no_temp_push_relations_tx(
    tx: &mut Transaction<'_>,
) -> Result<(), PushServiceOwnerError> {
    let mut rows = tx
        .query(
            "SELECT COALESCE(pg_catalog.current_schema()::text = ( \
               SELECT namespace.nspname::text FROM pg_catalog.pg_namespace namespace \
               WHERE namespace.oid = pg_catalog.pg_my_temp_schema()), false) \
               OR EXISTS (SELECT 1 FROM pg_catalog.pg_class relation \
                 WHERE relation.relnamespace = pg_catalog.pg_my_temp_schema() \
                   AND relation.relname IN ( \
                     'push_service_schema_marker', 'push_owner_locks', 'push_nodes', \
                     'push_node_locks', 'push_devices', 'push_delivery_attempts', \
                   'push_publish_jobs', 'push_publication_orders', \
                   'push_service_vapid_keys', 'push_service_vapid_schema_marker', \
                     'notification_outbox', 'notification_outbox_lineage'))",
            (),
        )
        .await?;
    if matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PushServiceOwnerError::UnsafeTemporaryRelation);
    }
    Ok(())
}

fn qualified_marker(schema: &str) -> String {
    let schema = schema.replace('"', "\"\"");
    format!("\"{schema}\".\"{PUSH_SCHEMA_MARKER_TABLE}\"")
}

pub(super) async fn lock_existing_push_relations_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PushServiceOwnerError> {
    let mut rows = tx
        .query(
            "SELECT relation.relname::text FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname IN (\
               'push_service_schema_marker', 'push_owner_locks', 'push_nodes', \
               'push_node_locks', 'push_devices', 'push_delivery_attempts', \
               'push_publish_jobs', 'push_publication_orders', \
               'push_service_vapid_keys', 'push_service_vapid_schema_marker') \
             ORDER BY relation.relname",
            crate::db_params![schema],
        )
        .await?;
    let mut names = Vec::new();
    while let Some(row) = rows.next().await? {
        names.push(row.get::<String>(0)?);
    }
    drop(rows);
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    for name in PUSH_RELATIONS_IN_LOCK_ORDER {
        if !names.iter().any(|existing| existing == name) {
            continue;
        }
        let quoted_name = crate::db::physical_attestation::quote_identifier(name);
        tx.execute(
            &format!("LOCK TABLE {quoted_schema}.{quoted_name} IN SHARE ROW EXCLUSIVE MODE"),
            (),
        )
        .await?;
    }
    Ok(())
}

async fn verify_existing_relations_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PushServiceOwnerError> {
    let mut rows = tx
        .query(
            "SELECT relation.relname::text, relation.relkind = 'r' \
               AND relation.relpersistence = 'p' \
               AND relation.relowner = namespace.nspowner \
               AND NOT relation.relrowsecurity AND NOT relation.relforcerowsecurity \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger trigger \
                               WHERE trigger.tgrelid = relation.oid AND NOT trigger.tgisinternal) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rewrite \
                               WHERE rewrite.ev_class = relation.oid) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname IN (\
               'push_owner_locks', 'push_nodes', 'push_node_locks', 'push_devices', \
               'push_delivery_attempts', 'push_publish_jobs', 'push_publication_orders', \
               'push_service_vapid_keys', 'push_service_vapid_schema_marker')",
            crate::db_params![schema],
        )
        .await?;
    while let Some(row) = rows.next().await? {
        let _name: String = row.get(0)?;
        if !row.get::<bool>(1)? {
            return Err(PushServiceOwnerError::UnsafeExistingRelation);
        }
    }
    drop(rows);

    let mut rows = tx
        .query(
            "SELECT EXISTS ( \
               SELECT 1 FROM pg_catalog.pg_constraint inbound \
               JOIN pg_catalog.pg_class target ON target.oid = inbound.confrelid \
               JOIN pg_catalog.pg_namespace namespace ON namespace.oid = target.relnamespace \
               JOIN pg_catalog.pg_class source ON source.oid = inbound.conrelid \
               WHERE inbound.contype = 'f' AND namespace.nspname = ? \
                 AND target.relname IN ('push_owner_locks', 'push_nodes', \
                   'push_node_locks', 'push_devices', 'push_delivery_attempts', \
                   'push_publish_jobs', 'push_publication_orders', \
                   'push_service_vapid_keys', 'push_service_vapid_schema_marker') \
                 AND NOT (source.relnamespace = namespace.oid \
                   AND source.relname IN ('push_owner_locks', 'push_nodes', \
                     'push_node_locks', 'push_devices', 'push_delivery_attempts', \
                     'push_publish_jobs', 'push_publication_orders', \
                     'push_service_vapid_keys', 'push_service_vapid_schema_marker')))",
            crate::db_params![schema],
        )
        .await?;
    if matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(PushServiceOwnerError::InboundForeignKey);
    }
    Ok(())
}
