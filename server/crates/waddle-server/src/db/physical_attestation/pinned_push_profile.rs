//! Partial Push-family ACL observation on a selected pinned transaction.
//! It does not read Foundation data or authorize serving.
use super::{PinnedPartialProfileError, PinnedPartialProfilePinsV1, PinnedRuntimeAuthority};
use crate::pubsub::{PubSubRuntimeAttachError, RestrictedServingProfileV1};
use sqlx::postgres::types::Oid;
use sqlx::PgConnection;
use thiserror::Error;

const READ: &[&str] = &["SELECT"];
const RIU: &[&str] = &["SELECT", "INSERT", "UPDATE"];
const RID: &[&str] = &["SELECT", "INSERT", "DELETE"];
const RW: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE"];
const PUSH_RELATIONS: [(&str, &[&str]); 17] = [
    ("push_registrations", RW),
    ("notification_activity", RIU),
    ("notification_activity_owner_state", READ),
    ("notification_candidates", RW),
    ("notification_outbox", RW),
    ("notification_outbox_lineage", RW),
    ("notification_outbox_owner_state", READ),
    ("push_service_schema_marker", READ),
    ("push_owner_locks", RIU),
    ("push_nodes", RW),
    ("push_node_locks", RIU),
    ("push_devices", RW),
    ("push_delivery_attempts", RID),
    ("push_publish_jobs", RW),
    ("push_publication_orders", RIU),
    ("push_service_vapid_keys", READ),
    ("push_service_vapid_schema_marker", READ),
];

/// Independently supplied pins; never discover these using the inspected pool.
#[derive(Clone, Debug)]
pub struct PinnedPushProfilePinsV1 {
    pub partial: PinnedPartialProfilePinsV1,
    pub pubsub: RestrictedServingProfileV1,
    pub push_relation_oids: [u32; 17],
}
impl PinnedPushProfilePinsV1 {
    pub fn push_relation_names() -> [&'static str; 17] {
        PUSH_RELATIONS.map(|(name, _)| name)
    }
    pub(super) fn valid_for(&self, authority: &PinnedRuntimeAuthority) -> bool {
        if !self.partial.valid_for(authority)
            || self.pubsub.api_oid != self.partial.api_role_oid
            || self.pubsub.helper_oid != self.partial.helper_role_oid
            || self.pubsub.helper_relation_oids != self.partial.pubsub_relation_oids
            || self.pubsub.helper_function_oid == 0
            || self.pubsub.helper_function_oid == authority.attestor.attestor_function_oid
            || authority.attestor.schema_owner_role_oid
                == authority.attestor.database_owner_role_oid
        {
            return false;
        }
        let mut relations = vec![
            authority.attestor.lineage_relation_oid,
            authority.attestor.lineage_primary_index_oid,
            authority.ledger.ledger_relation_oid,
            authority.ledger.ledger_primary_index_oid,
            authority.epoch_relation_oid,
            authority.epoch_primary_index_oid,
            authority.guard_manifest_relation_oid,
        ];
        relations.extend(self.partial.ingress_relation_oids);
        relations.extend(self.partial.pubsub_relation_oids);
        relations.extend(self.push_relation_oids);
        relations
            .iter()
            .enumerate()
            .all(|(i, oid)| *oid != 0 && !relations[..i].contains(oid))
    }
}

#[derive(Debug, Error)]
pub enum PinnedPushProfileError {
    #[error("Push-family profile requires distinct and consistent independent pins")]
    InvalidPins,
    #[error("Push-family PubSub gate is contended")]
    GateContended,
    #[error("Push-family PubSub helper verification or locking failed")]
    Helper(#[from] PubSubRuntimeAttachError),
    #[error("Push-family catalog or ACL drifted for {relation}")]
    RelationDrift { relation: &'static str },
    #[error("Push-family helper catalog exceeds its bounds")]
    HelperBounds,
    #[error("Push-family shared helper graph catalog exceeds its bounds")]
    GlobalCatalogBounds,
    #[error(transparent)]
    PartialProfile(#[from] PinnedPartialProfileError),
    #[error("Push-family PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// All probes and the existing fixed H-owned helper use the same backend.
pub(super) async fn inspect(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    pins: &PinnedPushProfilePinsV1,
) -> Result<(), PinnedPushProfileError> {
    if !pins.valid_for(authority) {
        return Err(PinnedPushProfileError::InvalidPins);
    }
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;
    if !crate::pubsub::runtime_attach::try_restricted_pubsub_gate(connection).await? {
        return Err(PinnedPushProfileError::GateContended);
    }
    global_graph_bounds(connection, authority, pins).await?;
    compatible_pubsub_acl(connection, authority, pins).await?;
    helper_bounds(connection, authority, pins).await?;
    crate::pubsub::runtime_attach::verify_and_call_restricted_pubsub_lock(
        connection,
        authority.config.postgres(),
        authority.config.definer_role.as_str(),
        &pins.pubsub,
    )
    .await?;
    // Recheck independently pinned identities and grants after name-based locks.
    super::verify_pinned_partial_profile(connection, authority, &pins.partial).await?;
    global_graph_bounds(connection, authority, pins).await?;
    compatible_pubsub_acl(connection, authority, pins).await?;
    helper_bounds(connection, authority, pins).await?;
    crate::pubsub::runtime_attach::verify_restricted_v1_graph_connection(
        connection,
        authority.config.postgres(),
        authority.config.definer_role.as_str(),
        &pins.pubsub,
    )
    .await?;
    for ((name, rights), oid) in PUSH_RELATIONS.into_iter().zip(pins.push_relation_oids) {
        verify_heap(connection, authority, name, oid, rights).await?;
    }
    Ok(())
}

// The shared H verifier checks privileges outside the twelve pinned PubSub
// heaps. Bound its input catalogs before it calls PostgreSQL privilege helpers.
// The provider must exclude all mutations of these scanned catalogs, including
// temporary-schema creation in other sessions, from before checkout to rollback.
async fn global_graph_bounds(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    pins: &PinnedPushProfilePinsV1,
) -> Result<(), PinnedPushProfileError> {
    let namespaces: i64 = sqlx::query_scalar(
        "SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_namespace LIMIT 1025) bounded",
    )
    .fetch_one(&mut *connection)
    .await?;
    if namespaces > 1024 {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    let namespace_acls: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.nspacl IS NOT NULL \
         AND (pg_catalog.pg_column_compression(n.nspacl) IS NOT NULL \
           OR pg_catalog.pg_column_size(n.nspacl)>4096))",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !namespace_acls {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    let database_acl: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT datacl IS NULL OR \
          (pg_catalog.pg_column_compression(datacl) IS NULL \
           AND pg_catalog.pg_column_size(datacl)<=4096) \
          FROM pg_catalog.pg_database WHERE oid=$1), false)",
    )
    .bind(Oid(authority.config.postgres().database.oid))
    .fetch_one(&mut *connection)
    .await?;
    if !database_acl {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    let parameters: i64 = sqlx::query_scalar(
        "SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_parameter_acl \
         WHERE parname='session_replication_role' LIMIT 2) bounded",
    )
    .fetch_one(&mut *connection)
    .await?;
    if parameters > 1 {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    let parameter_acl: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_parameter_acl \
         WHERE parname='session_replication_role' AND paracl IS NOT NULL \
          AND (pg_catalog.pg_column_compression(paracl) IS NOT NULL \
            OR pg_catalog.pg_column_size(paracl)>4096))",
    )
    .fetch_one(&mut *connection)
    .await?;
    if !parameter_acl {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }

    // This is the same namespace and relkind scope as the shared H verifier's
    // unrelated-relation privilege scan. LIMIT+1 detects overflow, never
    // treating a truncated scan as success.
    let relations: Vec<Oid> = sqlx::query_scalar(
        r#"SELECT c.oid FROM pg_catalog.pg_class c
         JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
         WHERE (n.oid=$1 OR (n.nspname NOT LIKE 'pg\_%' ESCAPE '\'
                            AND n.nspname<>'information_schema'))
           AND c.relkind IN ('r','p','v','m','f','S') LIMIT 1025"#,
    )
    .bind(Oid(authority.config.postgres().schema.oid))
    .fetch_all(&mut *connection)
    .await?;
    if relations.len() > 1024 {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    let attributes: i64 = sqlx::query_scalar(
        "SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attribute \
         WHERE attrelid=ANY($1::pg_catalog.oid[]) LIMIT 16385) bounded",
    )
    .bind(relations.clone())
    .fetch_one(&mut *connection)
    .await?;
    if attributes > 16384 {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    let relation_acls: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
           WHERE c.oid=ANY($1::pg_catalog.oid[]) AND c.relacl IS NOT NULL \
             AND (pg_catalog.pg_column_compression(c.relacl) IS NOT NULL \
               OR pg_catalog.pg_column_size(c.relacl)>4096)) \
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a \
           WHERE a.attrelid=ANY($1::pg_catalog.oid[]) AND a.attacl IS NOT NULL \
             AND (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL \
               OR pg_catalog.pg_column_size(a.attacl)>4096))",
    )
    .bind(relations)
    .fetch_one(&mut *connection)
    .await?;
    if !relation_acls {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    let pubsub: Vec<Oid> = pins
        .partial
        .pubsub_relation_oids
        .into_iter()
        .map(Oid)
        .collect();
    let triggers: i64 = sqlx::query_scalar(
        "SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_trigger \
         WHERE tgrelid=ANY($1::pg_catalog.oid[]) LIMIT 1025) bounded",
    )
    .bind(pubsub.clone())
    .fetch_one(&mut *connection)
    .await?;
    if triggers > 1024 {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    let constraints: i64 = sqlx::query_scalar(
        "SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_constraint \
         WHERE conrelid=ANY($1::pg_catalog.oid[]) OR confrelid=ANY($1::pg_catalog.oid[]) \
         LIMIT 1025) bounded",
    )
    .bind(pubsub)
    .fetch_one(&mut *connection)
    .await?;
    if constraints > 1024 {
        return Err(PinnedPushProfileError::GlobalCatalogBounds);
    }
    Ok(())
}

async fn compatible_pubsub_acl(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    pins: &PinnedPushProfilePinsV1,
) -> Result<(), PinnedPushProfileError> {
    for (name, oid) in PinnedPartialProfilePinsV1::pubsub_relation_names()
        .into_iter()
        .zip(pins.partial.pubsub_relation_oids)
    {
        let drift = || PinnedPushProfileError::RelationDrift { relation: name };
        // The preceding global preflight bounds both table and column ACL
        // storage; check the exact pinned identity again before privilege reads.
        let compatible: bool = sqlx::query_scalar(
            "SELECT COALESCE((SELECT c.relnamespace=$2 AND c.relname=$3 AND c.relowner=$4 \
              AND NOT pg_catalog.has_table_privilege($5,c.oid,'SELECT') \
              AND NOT pg_catalog.has_any_column_privilege($5,c.oid,'SELECT') \
              AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) acl \
                WHERE acl.grantee<>c.relowner AND (acl.grantor<>c.relowner \
                  OR acl.is_grantable OR acl.grantee NOT IN ($6,$7))) \
              FROM pg_catalog.pg_class c WHERE c.oid=$1), false)",
        )
        .bind(Oid(oid))
        .bind(Oid(authority.config.postgres().schema.oid))
        .bind(name)
        .bind(Oid(authority.attestor.schema_owner_role_oid))
        .bind(Oid(pins.partial.monitor_role_oid))
        .bind(Oid(authority.attestor.runtime_role_oid))
        .bind(Oid(pins.partial.helper_role_oid))
        .fetch_one(&mut *connection)
        .await?;
        if !compatible {
            return Err(drift());
        }
    }
    Ok(())
}

async fn helper_bounds(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    pins: &PinnedPushProfilePinsV1,
) -> Result<(), PinnedPushProfileError> {
    // Bound catalog values before expanding ACLs or reading function source.
    let bounded: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT
           (p.proacl IS NULL OR (pg_catalog.pg_column_compression(p.proacl) IS NULL
                                  AND pg_catalog.pg_column_size(p.proacl) <= 4096))
           AND (p.proconfig IS NULL OR (pg_catalog.pg_column_compression(p.proconfig) IS NULL
                                        AND pg_catalog.pg_column_size(p.proconfig) <= 4096))
           AND (n.nspacl IS NULL OR (pg_catalog.pg_column_compression(n.nspacl) IS NULL
                                     AND pg_catalog.pg_column_size(n.nspacl) <= 4096))
           AND pg_catalog.pg_column_compression(p.prosrc) IS NULL
           AND pg_catalog.pg_column_size(p.prosrc) <= 8192
         FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
         WHERE p.oid=$1 AND n.oid=$2), false)",
    )
    .bind(Oid(pins.pubsub.helper_function_oid))
    .bind(Oid(authority.config.postgres().schema.oid))
    .fetch_one(connection)
    .await?;
    if !bounded {
        return Err(PinnedPushProfileError::HelperBounds);
    }
    Ok(())
}

async fn verify_heap(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    name: &'static str,
    oid: u32,
    rights: &'static [&'static str],
) -> Result<(), PinnedPushProfileError> {
    let drift = || PinnedPushProfileError::RelationDrift { relation: name };
    let bounded: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT c.relnamespace=$2 AND c.relname=$3
           AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attribute a
                WHERE a.attrelid=c.oid LIMIT 65) bounded) <= 64
           FROM pg_catalog.pg_class c WHERE c.oid=$1), false)",
    )
    .bind(Oid(oid))
    .bind(Oid(authority.config.postgres().schema.oid))
    .bind(name)
    .fetch_one(&mut *connection)
    .await?;
    if !bounded {
        return Err(drift());
    }
    let bounded_acl: bool = sqlx::query_scalar(
        "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c WHERE c.oid=$1
           AND c.relacl IS NOT NULL AND (pg_catalog.pg_column_compression(c.relacl) IS NOT NULL
             OR pg_catalog.pg_column_size(c.relacl)>4096))
         AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a WHERE a.attrelid=$1
           AND a.attacl IS NOT NULL AND (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL
             OR pg_catalog.pg_column_size(a.attacl)>4096))",
    )
    .bind(Oid(oid))
    .fetch_one(&mut *connection)
    .await?;
    if !bounded_acl {
        return Err(drift());
    }
    let exact: bool = sqlx::query_scalar(
        "SELECT COALESCE((SELECT c.relnamespace=$2 AND c.relowner=$3
           AND c.relkind='r' AND c.relpersistence='p' AND c.relam=2::pg_catalog.oid
           AND NOT c.relispartition AND c.reloftype=0::pg_catalog.oid
           AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relhasrules
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i
                           WHERE i.inhrelid=c.oid OR i.inhparent=c.oid)
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid)
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite w WHERE w.ev_class=c.oid)
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
                           WHERE a.attrelid=c.oid AND a.attacl IS NOT NULL)
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
              COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) acl
              WHERE acl.grantee<>c.relowner AND (acl.grantor<>c.relowner OR acl.is_grantable
                OR acl.grantee<>$4 OR NOT (acl.privilege_type=ANY($5))))
           AND NOT EXISTS (SELECT 1 FROM pg_catalog.unnest($5::pg_catalog.text[]) required(privilege)
              WHERE NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
                WHERE acl.grantee=$4 AND acl.grantor=c.relowner
                  AND acl.privilege_type=required.privilege AND NOT acl.is_grantable))
           FROM pg_catalog.pg_class c WHERE c.oid=$1), false)",
    ).bind(Oid(oid)).bind(Oid(authority.config.postgres().schema.oid))
     .bind(Oid(authority.attestor.schema_owner_role_oid))
     .bind(Oid(authority.attestor.runtime_role_oid)).bind(rights)
     .fetch_one(connection).await?;
    if !exact {
        return Err(drift());
    }
    Ok(())
}
