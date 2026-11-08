//! Fixed, partial catalog and ACL observation for the selected-entry diagnostic.
//!
//! V1 checks eight independently pinned principals and the listed guarded
//! ingress and PubSub heap identities and direct ACLs, plus the epoch and
//! manifest direct ACLs. It does not check those store heap
//! columns, indexes, constraints, triggers, functions, data, other relations,
//! or every privilege in the database. In particular, it is not a store or
//! serving authorization. R cannot lock the guarded heaps without grants the
//! intended role graph deliberately denies, so these checks are catalog-only.
//! A provider must enforce the absence of a login trigger and quiesce privileged
//! DDL, role, ACL, and index mutation from before connection through rollback.

use sqlx::postgres::types::Oid;
use sqlx::PgConnection;
use thiserror::Error;

use super::PinnedRuntimeAuthority;

const INGRESS: [&str; 13] = [
    "ingress_messages",
    "ingress_origin_aliases",
    "ingress_sm_refs",
    "ingress_deliveries",
    "ingress_sm_streams",
    "ingress_effect_intents",
    "ingress_effect_receipts",
    "ingress_carbon_receipts",
    "ingress_delivery_receipts",
    "ingress_archive_dispatch",
    "ingress_send_attempts",
    "ingress_effect_descendants",
    "ingress_owner_rejections",
];

// Runtime table privileges, in the same order as the independently supplied
// PubSub OIDs. H has UPDATE on each table solely for the separate lock helper.
const PUBSUB: [(&str, &[&str]); 12] = [
    ("dnd_projection", &["SELECT", "INSERT", "UPDATE", "DELETE"]),
    (
        "dnd_projection_source_version",
        &["SELECT", "INSERT", "UPDATE"],
    ),
    (
        "notification_settings_projection",
        &["SELECT", "INSERT", "UPDATE", "DELETE"],
    ),
    (
        "notification_settings_projection_source_version",
        &["SELECT", "INSERT", "UPDATE"],
    ),
    (
        "pubsub_affiliations",
        &["SELECT", "INSERT", "UPDATE", "DELETE"],
    ),
    ("pubsub_items", &["SELECT", "INSERT", "UPDATE", "DELETE"]),
    ("pubsub_nodes", &["SELECT", "INSERT", "UPDATE", "DELETE"]),
    ("pubsub_owner_configured", &["SELECT", "INSERT"]),
    ("pubsub_push_publications", &["SELECT", "INSERT", "UPDATE"]),
    ("pubsub_schema_marker", &["SELECT"]),
    ("pubsub_schema_version", &["SELECT"]),
    ("pubsub_subscriptions", &["SELECT", "INSERT", "DELETE"]),
];

const INGRESS_API_RIGHTS: &[&str] = &["SELECT", "INSERT", "UPDATE", "DELETE"];

/// Additional identities from an independent operator handoff. S, R, T and I
/// and the core lineage/ledger/epoch/manifest pins are in the pinned pool.
/// The other roles are A (ingress API), H (PubSub helper), M (`pg_monitor`),
/// and X (migrator). Heap OIDs use the fixed orders returned below; they must
/// not be learned from the connection being inspected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PinnedPartialProfilePinsV1 {
    pub api_role_oid: u32,
    pub helper_role_oid: u32,
    pub monitor_role_oid: u32,
    pub migrator_role_oid: u32,
    pub ingress_relation_oids: [u32; 13],
    pub pubsub_relation_oids: [u32; 12],
}

impl PinnedPartialProfilePinsV1 {
    /// Fixed V1 ingress OID order for independent provisioning.
    pub fn ingress_relation_names() -> &'static [&'static str; 13] {
        &INGRESS
    }

    /// Fixed V1 PubSub OID order for independent provisioning.
    pub fn pubsub_relation_names() -> [&'static str; 12] {
        PUBSUB.map(|(name, _)| name)
    }

    pub(super) fn valid_for(&self, authority: &PinnedRuntimeAuthority) -> bool {
        let roles = [
            authority.attestor.schema_owner_role_oid,
            authority.attestor.runtime_role_oid,
            self.api_role_oid,
            self.helper_role_oid,
            authority.attestor.definer_role_oid,
            authority.attestor.installer_role_oid,
            self.monitor_role_oid,
            self.migrator_role_oid,
        ];
        let mut relations = vec![
            authority.attestor.lineage_relation_oid,
            authority.attestor.lineage_primary_index_oid,
            authority.ledger.ledger_relation_oid,
            authority.ledger.ledger_primary_index_oid,
            authority.epoch_relation_oid,
            authority.epoch_primary_index_oid,
            authority.guard_manifest_relation_oid,
        ];
        relations.extend(self.ingress_relation_oids);
        relations.extend(self.pubsub_relation_oids);
        distinct_nonzero(&roles)
            && !roles[1..].contains(&authority.attestor.database_owner_role_oid)
            && distinct_nonzero(&relations)
    }
}

fn distinct_nonzero(values: &[u32]) -> bool {
    values
        .iter()
        .enumerate()
        .all(|(i, oid)| *oid != 0 && !values[..i].contains(oid))
}

#[derive(Debug, Error)]
pub enum PinnedPartialProfileError {
    #[error("partial profile requires distinct, independent role and relation pins")]
    InvalidPins,
    #[error("partial profile role identities, attributes, or membership drifted")]
    RoleDrift,
    #[error("partial profile catalog or ACL drifted for {relation}")]
    RelationDrift { relation: &'static str },
    #[error("partial profile PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// Catalog-only, bounded-input check. The caller supplies a catalog-first
/// search path and the external privileged-mutation boundary. This is repeated
/// from a new READ COMMITTED snapshot after every relevant relation wait and
/// before the selected transaction reads the corresponding protected data.
pub(crate) async fn verify_pinned_partial_profile(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    pins: &PinnedPartialProfilePinsV1,
) -> Result<(), PinnedPartialProfileError> {
    if !pins.valid_for(authority) {
        return Err(PinnedPartialProfileError::InvalidPins);
    }
    let roles = [
        authority.attestor.schema_owner_role_oid,
        authority.attestor.runtime_role_oid,
        pins.api_role_oid,
        pins.helper_role_oid,
        authority.attestor.definer_role_oid,
        authority.attestor.installer_role_oid,
        pins.monitor_role_oid,
        pins.migrator_role_oid,
    ]
    .map(Oid);
    let exact: bool = sqlx::query_scalar(ROLES)
        .bind(roles.to_vec())
        .bind(Oid(authority.config.postgres().schema.oid))
        .fetch_one(&mut *connection)
        .await?;
    if !exact {
        return Err(PinnedPartialProfileError::RoleDrift);
    }

    for (name, oid, is_epoch) in [
        ("ingress_protocol_epoch", authority.epoch_relation_oid, true),
        (
            "ingress_epoch_guard_manifest",
            authority.guard_manifest_relation_oid,
            false,
        ),
    ] {
        verify_epoch_acl(connection, authority, pins, name, oid, is_epoch).await?;
    }

    for (name, oid) in INGRESS.into_iter().zip(pins.ingress_relation_oids) {
        verify_relation(
            connection,
            authority,
            name,
            oid,
            ExpectedAcl {
                grantee: pins.api_role_oid,
                required: INGRESS_API_RIGHTS,
                helper: None,
                monitor: pins.monitor_role_oid,
            },
        )
        .await?;
    }
    for ((name, rights), oid) in PUBSUB.into_iter().zip(pins.pubsub_relation_oids) {
        verify_relation(
            connection,
            authority,
            name,
            oid,
            ExpectedAcl {
                grantee: authority.attestor.runtime_role_oid,
                required: rights,
                helper: Some(pins.helper_role_oid),
                monitor: pins.monitor_role_oid,
            },
        )
        .await?;
    }
    Ok(())
}

struct ExpectedAcl<'a> {
    grantee: u32,
    required: &'a [&'a str],
    helper: Option<u32>,
    monitor: u32,
}

async fn verify_epoch_acl(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    pins: &PinnedPartialProfilePinsV1,
    name: &'static str,
    oid: u32,
    is_epoch: bool,
) -> Result<(), PinnedPartialProfileError> {
    verify_relation_bounds(connection, authority, name, oid).await?;
    let exact: bool = sqlx::query_scalar(EPOCH_ACL)
        .bind(Oid(oid))
        .bind(Oid(authority.config.postgres().schema.oid))
        .bind(Oid(authority.attestor.schema_owner_role_oid))
        .bind(Oid(authority.attestor.runtime_role_oid))
        .bind(Oid(pins.api_role_oid))
        .bind(Oid(pins.monitor_role_oid))
        .bind(is_epoch)
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !exact {
        return Err(PinnedPartialProfileError::RelationDrift { relation: name });
    }
    Ok(())
}

async fn verify_relation(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    name: &'static str,
    oid: u32,
    acl: ExpectedAcl<'_>,
) -> Result<(), PinnedPartialProfileError> {
    let drift = || PinnedPartialProfileError::RelationDrift { relation: name };
    verify_relation_bounds(connection, authority, name, oid).await?;
    let exact: bool = sqlx::query_scalar(RELATION_ACL)
        .bind(Oid(oid))
        .bind(Oid(authority.config.postgres().schema.oid))
        .bind(Oid(authority.attestor.schema_owner_role_oid))
        .bind(Oid(acl.grantee))
        .bind(Oid(acl.helper.unwrap_or(0)))
        .bind(acl.required)
        .bind(Oid(acl.monitor))
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !exact {
        return Err(drift());
    }
    Ok(())
}

async fn verify_relation_bounds(
    connection: &mut PgConnection,
    authority: &PinnedRuntimeAuthority,
    name: &'static str,
    oid: u32,
) -> Result<(), PinnedPartialProfileError> {
    let drift = || PinnedPartialProfileError::RelationDrift { relation: name };
    // This first statement limits the attribute inventory without expanding
    // ACLs or consuming variable-sized catalog data.
    let bounded_inventory: bool = sqlx::query_scalar(RELATION_INVENTORY)
        .bind(Oid(oid))
        .bind(Oid(authority.config.postgres().schema.oid))
        .bind(name)
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !bounded_inventory {
        return Err(drift());
    }
    // Check storage bounds separately before any ACL expansion; SQL predicate
    // evaluation order inside a statement is not a security boundary.
    let bounded_acl: bool = sqlx::query_scalar(RELATION_ACL_BOUNDS)
        .bind(Oid(oid))
        .fetch_one(&mut *connection)
        .await?;
    if !bounded_acl {
        return Err(drift());
    }
    Ok(())
}

// The array order is S/R/A/H/T/I/M/X. Only X -> S and I -> T may involve
// those principals, with SET but neither INHERIT nor ADMIN. M's unrelated
// built-in membership edges are outside this partial profile.
const ROLES: &str = r#"
SELECT (SELECT pg_catalog.count(*) FROM pg_catalog.pg_roles r WHERE r.oid = ANY($1)) = 8
  AND pg_catalog.current_setting('transaction_isolation') = 'read committed'
  AND pg_catalog.current_setting('transaction_read_only') = 'on'
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_namespace n WHERE n.oid = $2 AND n.nspowner = $1[1])
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.oid = $1[2]
              AND r.rolname = session_user AND r.rolname = current_user)
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.oid = $1[7] AND r.rolname = 'pg_monitor')
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_roles r WHERE r.oid = ANY($1)
    AND r.oid <> $1[7] AND (r.rolsuper OR r.rolcreatedb OR r.rolcreaterole
      OR r.rolreplication OR r.rolbypassrls OR r.rolinherit
      OR r.rolcanlogin <> (r.oid = ANY(ARRAY[$1[2],$1[6],$1[8]]))))
  AND (SELECT pg_catalog.count(*) FROM (
       SELECT 1 FROM pg_catalog.pg_auth_members m
       WHERE m.member = ANY(ARRAY[$1[1],$1[2],$1[3],$1[4],$1[5],$1[6],$1[8]])
          OR m.roleid = ANY(ARRAY[$1[1],$1[2],$1[3],$1[4],$1[5],$1[6],$1[8]])
       LIMIT 3) bounded) = 2
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.roleid = $1[1]
      AND m.member = $1[8] AND m.set_option AND NOT m.inherit_option AND NOT m.admin_option)
  AND EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.roleid = $1[5]
      AND m.member = $1[6] AND m.set_option AND NOT m.inherit_option AND NOT m.admin_option)
"#;

const RELATION_INVENTORY: &str = r#"
SELECT c.relnamespace = $2 AND c.relname = $3
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attribute a
       WHERE a.attrelid = c.oid LIMIT 65) bounded) <= 64
FROM pg_catalog.pg_class c WHERE c.oid = $1
"#;

const RELATION_ACL_BOUNDS: &str = r#"
SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c WHERE c.oid = $1
    AND c.relacl IS NOT NULL AND (pg_catalog.pg_column_compression(c.relacl) IS NOT NULL
      OR pg_catalog.pg_column_size(c.relacl) > 4096))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a WHERE a.attrelid = $1
    AND a.attacl IS NOT NULL AND (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL
      OR pg_catalog.pg_column_size(a.attacl) > 4096))
"#;

// The existing pinned epoch/manifest verifier checks their complete bounded
// shapes. This additional statement checks the joint direct-ACL matrix.
const EPOCH_ACL: &str = r#"
SELECT c.relnamespace = $2 AND c.relowner = $3
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
      COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl
    WHERE acl.grantee <> $3 AND (acl.grantor <> $3 OR acl.is_grantable
      OR acl.privilege_type <> 'SELECT'
      OR NOT (acl.grantee IN ($4, $6) OR ($7 AND acl.grantee = $5))))
  AND EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
    WHERE acl.grantee = $4 AND acl.grantor = $3 AND acl.privilege_type = 'SELECT' AND NOT acl.is_grantable)
  AND (NOT $7 OR EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
    WHERE acl.grantee = $5 AND acl.grantor = $3 AND acl.privilege_type = 'SELECT' AND NOT acl.is_grantable))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl
    WHERE a.attrelid = c.oid AND NOT ($7 AND a.attname = 'id' AND acl.grantee = $5
      AND acl.grantor = $3 AND acl.privilege_type = 'UPDATE' AND NOT acl.is_grantable))
  AND (NOT $7 OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl
    WHERE a.attrelid = c.oid AND a.attname = 'id' AND acl.grantee = $5
      AND acl.grantor = $3 AND acl.privilege_type = 'UPDATE' AND NOT acl.is_grantable))
FROM pg_catalog.pg_class c WHERE c.oid = $1
"#;

const RELATION_ACL: &str = r#"
SELECT c.relnamespace = $2 AND c.relowner = $3 AND c.relkind = 'r'
  AND c.relpersistence = 'p' AND c.relam = 2::pg_catalog.oid
  AND NOT c.relispartition AND c.reloftype = 0::pg_catalog.oid
  AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relhasrules
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid = c.oid OR i.inhparent = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite w WHERE w.ev_class = c.oid)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a WHERE a.attrelid = c.oid AND a.attacl IS NOT NULL)
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
      COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl
    WHERE acl.grantee <> c.relowner AND (acl.grantor <> c.relowner OR acl.is_grantable
      OR NOT ((acl.grantee = $4 AND acl.privilege_type = ANY($6))
        OR ($5 <> 0::pg_catalog.oid AND acl.grantee = $5 AND acl.privilege_type = 'UPDATE')
        OR (acl.grantee = $7 AND acl.privilege_type = 'SELECT'))))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.unnest($6::pg_catalog.text[]) required(privilege)
    WHERE NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
      WHERE acl.grantee = $4 AND acl.grantor = c.relowner
        AND acl.privilege_type = required.privilege AND NOT acl.is_grantable))
  AND ($5 = 0::pg_catalog.oid OR EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
      WHERE acl.grantee = $5 AND acl.grantor = c.relowner
        AND acl.privilege_type = 'UPDATE' AND NOT acl.is_grantable))
FROM pg_catalog.pg_class c WHERE c.oid = $1
"#;
