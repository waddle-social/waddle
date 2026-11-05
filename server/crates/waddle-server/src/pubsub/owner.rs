//! PostgreSQL owner marker and schema-mutation gate for PubSub.

use crate::db::lineage::PgIdentity;
use crate::db::{DatabaseError, Transaction};
use waddle_xmpp::XmppError;

use super::runtime_attach::{self, PubSubRuntimeAttachError};

pub(super) const PUBSUB_SCHEMA_ADVISORY_LOCK_KEY: i64 = 7_534_519_881_230_429_064;
pub(super) const PUBSUB_SCHEMA_MARKER_TABLE: &str = "pubsub_schema_marker";
pub(super) const PUBSUB_SCHEMA_MARKER_VERSION: i64 = 1;
pub(super) const PUBSUB_ATTACH_LOCK_FUNCTION: &str = "pubsub_runtime_attach_lock";
const PUBSUB_OWNER_PHASE_INCOMPLETE: i64 = 0;
const PUBSUB_OWNER_PHASE_COMPLETE: i64 = 1;

pub(super) const MANAGED_RELATIONS: &[&str] = &[
    "dnd_projection",
    "dnd_projection_source_version",
    "notification_settings_projection",
    "notification_settings_projection_source_version",
    "pubsub_affiliations",
    "pubsub_items",
    "pubsub_nodes",
    "pubsub_owner_configured",
    "pubsub_push_publications",
    PUBSUB_SCHEMA_MARKER_TABLE,
    "pubsub_schema_version",
    "pubsub_subscriptions",
];

/// Lock existing PubSub relations in their stable, store-local order. Owner
/// preparation can reset empty schemas, so it must exclude old DML while
/// counting and dropping the relations. Runtime verification uses SUE, which
/// conflicts with CIC while permitting ordinary row writers.
pub(super) async fn lock_existing_relations_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            "SELECT relation.relname::text FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname IN (\
               'dnd_projection', 'dnd_projection_source_version', \
               'notification_settings_projection', 'notification_settings_projection_source_version', \
               'pubsub_affiliations', 'pubsub_items', 'pubsub_nodes', 'pubsub_owner_configured', \
               'pubsub_push_publications', 'pubsub_schema_marker', 'pubsub_schema_version', \
               'pubsub_subscriptions') ORDER BY relation.relname",
            crate::db_params![schema],
        )
        .await
        .map_err(internal)?;
    let mut existing = Vec::new();
    while let Some(row) = rows.next().await.map_err(internal)? {
        existing.push(row.get::<String>(0).map_err(internal)?);
    }
    drop(rows);
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    for name in MANAGED_RELATIONS {
        if !existing.iter().any(|relation| relation == name) {
            continue;
        }
        let quoted_relation = crate::db::physical_attestation::quote_identifier(name);
        tx.execute(
            &format!("LOCK TABLE {quoted_schema}.{quoted_relation} IN SHARE ROW EXCLUSIVE MODE"),
            (),
        )
        .await
        .map_err(internal)?;
    }
    Ok(())
}

/// Refuse a marker left by a different physical boundary. A matching marker
/// is made incomplete before any owner DDL or data repair starts.
pub(super) async fn begin_owner_phase_tx(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), XmppError> {
    if marker_exists_tx(tx, &identity.schema.name.0).await? {
        let _ = runtime_attach::verify_marker_shape_and_data_tx(tx, identity)
            .await
            .map_err(marker_error)?;
        set_owner_phase_tx(tx, &identity.schema.name.0, PUBSUB_OWNER_PHASE_INCOMPLETE).await?;
    }
    Ok(())
}

/// A legacy raw initializer has no trusted identity to complete the marker.
/// It can only revoke completion, in the same transaction and under the same
/// PubSub advisory gate as owner preparation.
pub(super) async fn invalidate_completion_marker_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), XmppError> {
    if marker_exists_tx(tx, schema).await? {
        runtime_attach::verify_marker_shape_tx(tx, schema)
            .await
            .map_err(marker_error)?;
        set_owner_phase_tx(tx, schema, PUBSUB_OWNER_PHASE_INCOMPLETE).await?;
    }
    Ok(())
}

/// Complete the marker only after the schema preparation transaction has
/// finished its reset/create/repair work successfully.
pub(super) async fn complete_owner_phase_tx(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), XmppError> {
    if !marker_exists_tx(tx, &identity.schema.name.0).await? {
        create_marker_tx(tx, identity).await?;
    }
    ensure_attach_lock_function_tx(tx, &identity.schema.name.0).await?;
    runtime_attach::verify_owner_schema_shape_tx(tx, &identity.schema.name.0)
        .await
        .map_err(marker_error)?;
    let _ = runtime_attach::verify_marker_shape_and_data_tx(tx, identity)
        .await
        .map_err(marker_error)?;
    set_owner_phase_tx(tx, &identity.schema.name.0, PUBSUB_OWNER_PHASE_COMPLETE).await?;
    if !runtime_attach::verify_marker_shape_and_data_tx(tx, identity)
        .await
        .map_err(marker_error)?
    {
        return Err(XmppError::internal(
            "PubSub owner completion marker did not verify".to_string(),
        ));
    }
    Ok(())
}

async fn ensure_attach_lock_function_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_proc p \
             JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = ? AND p.proname = ?)",
            crate::db_params![schema, PUBSUB_ATTACH_LOCK_FUNCTION],
        )
        .await
        .map_err(internal)?;
    let exists = rows
        .next()
        .await
        .map_err(internal)?
        .ok_or_else(|| XmppError::internal("PubSub attach lock probe returned no row".to_string()))?
        .get::<bool>(0)
        .map_err(internal)?;
    drop(rows);
    if exists {
        return Ok(());
    }

    let function = format!(
        "{}.{}",
        crate::db::physical_attestation::quote_identifier(schema),
        crate::db::physical_attestation::quote_identifier(PUBSUB_ATTACH_LOCK_FUNCTION)
    );
    let body = attach_lock_function_body(schema);
    tx.execute(
        &format!(
            "CREATE FUNCTION {function}() RETURNS void LANGUAGE sql SECURITY DEFINER \
             SET search_path = pg_catalog, pg_temp AS $waddle_pubsub_attach_lock${body}$waddle_pubsub_attach_lock$"
        ),
        (),
    )
    .await
    .map_err(internal)?;
    tx.execute(
        &format!("REVOKE ALL ON FUNCTION {function}() FROM PUBLIC"),
        (),
    )
    .await
    .map_err(internal)?;
    Ok(())
}

pub(super) fn attach_lock_function_body(schema: &str) -> String {
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    let statements = MANAGED_RELATIONS
        .iter()
        .map(|relation| {
            let quoted_relation = crate::db::physical_attestation::quote_identifier(relation);
            format!(
                "    LOCK TABLE {quoted_schema}.{quoted_relation} IN SHARE UPDATE EXCLUSIVE MODE NOWAIT;"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{statements}\nSELECT NULL::void;")
}

/// Pin an untrusted legacy PostgreSQL initializer to its active persistent
/// schema and reject temp relations that could shadow the schema being reset.
pub(super) async fn pin_legacy_schema_path_tx(
    tx: &mut Transaction<'_>,
) -> Result<String, XmppError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.current_schema()::text, \
             COALESCE(pg_catalog.current_schema()::text = ( \
               SELECT namespace.nspname::text FROM pg_catalog.pg_namespace namespace \
               WHERE namespace.oid = pg_catalog.pg_my_temp_schema()), false) \
             OR EXISTS (SELECT 1 FROM pg_catalog.pg_class relation \
               WHERE relation.relnamespace = pg_catalog.pg_my_temp_schema() \
                 AND relation.relname IN (\
                   'dnd_projection', 'dnd_projection_source_version', \
                   'notification_settings_projection', 'notification_settings_projection_source_version', \
                   'pubsub_affiliations', 'pubsub_items', 'pubsub_nodes', 'pubsub_owner_configured', \
                   'pubsub_push_publications', 'pubsub_schema_marker', 'pubsub_schema_version', \
                   'pubsub_subscriptions'))",
            (),
        )
        .await
        .map_err(internal)?;
    let row =
        rows.next().await.map_err(internal)?.ok_or_else(|| {
            XmppError::internal("PubSub schema lookup returned no row".to_string())
        })?;
    let schema = row
        .get::<Option<String>>(0)
        .map_err(internal)?
        .ok_or_else(|| XmppError::internal("PubSub active schema is missing".to_string()))?;
    let shadowed = row.get::<bool>(1).map_err(internal)?;
    drop(rows);
    if shadowed {
        return Err(XmppError::internal(
            "PubSub raw initializer refused a temporary schema shadow".to_string(),
        ));
    }
    crate::db::physical_attestation::validate_identifier(&schema, "PubSub raw schema")
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let quoted_schema = crate::db::physical_attestation::quote_identifier(&schema);
    tx.query(
        "SELECT pg_catalog.set_config('search_path', ?, true)",
        crate::db_params![format!("{quoted_schema}, pg_temp")],
    )
    .await
    .map_err(internal)?;
    Ok(schema)
}

async fn marker_exists_tx(tx: &mut Transaction<'_>, schema: &str) -> Result<bool, XmppError> {
    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = ?)",
            crate::db_params![schema, PUBSUB_SCHEMA_MARKER_TABLE],
        )
        .await
        .map_err(internal)?;
    rows.next()
        .await
        .map_err(internal)?
        .ok_or_else(|| XmppError::internal("PubSub marker probe returned no row".to_string()))?
        .get(0)
        .map_err(internal)
}

async fn create_marker_tx(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), XmppError> {
    tx.execute(
        "CREATE TABLE pubsub_schema_marker (\
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
    .await
    .map_err(internal)?;
    tx.execute(
        "INSERT INTO pubsub_schema_marker \
           (marker_id, version, owner_phase, system_identifier, database_oid, database_name, schema_oid, schema_name) \
         VALUES (1, ?, ?, ?, ?, ?, ?, ?)",
        crate::db_params![
            PUBSUB_SCHEMA_MARKER_VERSION,
            PUBSUB_OWNER_PHASE_INCOMPLETE,
            identity.system_identifier.to_string(),
            i64::from(identity.database.oid),
            identity.database.name.0.clone(),
            i64::from(identity.schema.oid),
            identity.schema.name.0.clone(),
        ],
    )
    .await
    .map_err(internal)?;
    Ok(())
}

async fn set_owner_phase_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
    phase: i64,
) -> Result<(), XmppError> {
    let marker = format!(
        "{}.{}",
        crate::db::physical_attestation::quote_identifier(schema),
        crate::db::physical_attestation::quote_identifier(PUBSUB_SCHEMA_MARKER_TABLE)
    );
    tx.execute(
        &format!("UPDATE {marker} SET owner_phase = ? WHERE marker_id = 1"),
        crate::db_params![phase],
    )
    .await
    .map_err(internal)?;
    Ok(())
}

fn marker_error(error: PubSubRuntimeAttachError) -> XmppError {
    XmppError::internal(error.to_string())
}

fn internal(error: DatabaseError) -> XmppError {
    XmppError::internal(error.to_string())
}
