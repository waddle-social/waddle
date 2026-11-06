//! Catalog-only checks for the pinned epoch and guard-manifest relations.
//!
//! These checks do not inspect rows, prove trigger/guard behavior, or admit a
//! serving transaction. The caller must already hold the pinned physical and
//! complete migration-ledger barriers in a bounded PG17 READ COMMITTED READ
//! ONLY transaction. Privileged catalog, role, ACL and uncooperative data writes
//! must be independently quiescent throughout this observation and its use.

use sqlx::postgres::types::Oid;
use thiserror::Error;

use super::{quote_identifier, PhysicalAttestationError, PinnedRuntimeAuthority};
use crate::db::Transaction;

#[derive(Debug, Error)]
pub enum PinnedCatalogError {
    #[error("pinned epoch and manifest catalog preflight failed")]
    Preflight,
    #[error("pinned epoch and manifest catalog drifted")]
    CatalogDrift,
    #[error(transparent)]
    Attestation(#[from] PhysicalAttestationError),
    #[error("pinned epoch and manifest PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// Leave the epoch and manifest ACCESS SHARE barriers held by `tx`, in that
/// order. Every catalog check is repeated from a fresh READ COMMITTED snapshot
/// after each name-based lock, which can wait through relation replacement.
pub(super) async fn verify_pinned_epoch_manifest_catalog(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
) -> Result<(), PinnedCatalogError> {
    let schema = &authority.config.postgres().schema.name.0;
    let qualified_schema = quote_identifier(schema);
    let connection = tx
        .postgres_connection()
        .ok_or(PhysicalAttestationError::PostgresRequired)?;
    sqlx::query("SET LOCAL search_path = pg_catalog, pg_temp")
        .execute(&mut *connection)
        .await?;

    verify_catalog(tx, authority).await?;
    lock_relation(tx, &qualified_schema, "ingress_protocol_epoch").await?;
    verify_catalog(tx, authority).await?;
    lock_relation(tx, &qualified_schema, "ingress_epoch_guard_manifest").await?;
    verify_catalog(tx, authority).await?;

    let connection = tx
        .postgres_connection()
        .ok_or(PhysicalAttestationError::PostgresRequired)?;
    sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
        .bind(format!("{qualified_schema}, pg_temp"))
        .execute(connection)
        .await?;
    Ok(())
}

async fn lock_relation(
    tx: &mut Transaction<'_>,
    schema: &str,
    relation: &'static str,
) -> Result<(), PinnedCatalogError> {
    let connection = tx
        .postgres_connection()
        .ok_or(PhysicalAttestationError::PostgresRequired)?;
    sqlx::query(&format!(
        "LOCK TABLE ONLY {schema}.{relation} IN ACCESS SHARE MODE"
    ))
    .execute(connection)
    .await?;
    Ok(())
}

async fn verify_catalog(
    tx: &mut Transaction<'_>,
    authority: &PinnedRuntimeAuthority,
) -> Result<(), PinnedCatalogError> {
    let connection = tx
        .postgres_connection()
        .ok_or(PhysicalAttestationError::PostgresRequired)?;
    let epoch = Oid(authority.epoch_relation_oid);
    let manifest = Oid(authority.guard_manifest_relation_oid);
    let schema_oid = Oid(authority.config.postgres().schema.oid);
    let owner = Oid(authority.attestor.schema_owner_role_oid);
    let runtime = Oid(authority.attestor.runtime_role_oid);

    // Check identities and count catalog entries before per-entry sizing. A
    // separate statement prevents predicate reordering from bypassing this
    // inventory gate when an attacker has supplied an excessive inventory.
    let inventory: bool = sqlx::query_scalar(INVENTORY_PREFLIGHT)
        .bind(epoch)
        .bind(manifest)
        .bind(schema_oid)
        .bind(&authority.config.postgres().schema.name.0)
        .bind(owner)
        .fetch_optional(&mut *connection)
        .await?
        .unwrap_or(false);
    if !inventory {
        return Err(PinnedCatalogError::Preflight);
    }

    // Only inspect bounded storage metadata here, without expanding an ACL or
    // array or deparsing a node. Dependency rows are bounded in this statement
    // after the preceding gate has bounded the associated object inventory.
    let bounded: bool = sqlx::query_scalar(RAW_PREFLIGHT)
        .bind(epoch)
        .bind(manifest)
        .fetch_one(&mut *connection)
        .await?;
    if !bounded {
        return Err(PinnedCatalogError::Preflight);
    }

    // The prior statements bound inputs to ACL/array helpers. This separate
    // statement proves built-in dependencies before the expression deparser.
    let exact: bool = sqlx::query_scalar(CATALOG_SHAPE)
        .bind(epoch)
        .bind(manifest)
        .bind(schema_oid)
        .bind(owner)
        .bind(runtime)
        .bind(Oid(authority.epoch_primary_index_oid))
        .fetch_one(&mut *connection)
        .await?;
    if !exact {
        return Err(PinnedCatalogError::CatalogDrift);
    }

    let expressions: bool = sqlx::query_scalar(EXPRESSIONS)
        .bind(epoch)
        .fetch_one(&mut *connection)
        .await?;
    if !expressions {
        return Err(PinnedCatalogError::CatalogDrift);
    }
    Ok(())
}

// These OID lookups precede every use of a schema-qualified target name.
// Each count stops at its limit + 1; no variable catalog value is inspected.
const INVENTORY_PREFLIGHT: &str = r#"
SELECT e.relnamespace = $3 AND m.relnamespace = $3
  AND n.oid = $3 AND n.nspname = $4 AND n.nspowner = $5
  AND e.relname = 'ingress_protocol_epoch' AND m.relname = 'ingress_epoch_guard_manifest'
  AND e.relowner = $5 AND m.relowner = $5
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attribute a
       WHERE a.attrelid IN ($1, $2) LIMIT 33) bounded) <= 32
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_constraint x
       WHERE x.conrelid IN ($1, $2) LIMIT 9) bounded) <= 8
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_attrdef d
       WHERE d.adrelid IN ($1, $2) LIMIT 5) bounded) <= 4
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_index i
       WHERE i.indrelid IN ($1, $2) LIMIT 5) bounded) <= 4
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_trigger t
       WHERE t.tgrelid IN ($1, $2) LIMIT 9) bounded) <= 8
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_amop o
       WHERE o.amopfamily IN (1976::pg_catalog.oid, 1994::pg_catalog.oid) LIMIT 129) bounded) <= 128
  AND (SELECT pg_catalog.count(*) FROM (SELECT 1 FROM pg_catalog.pg_amproc p
       WHERE p.amprocfamily IN (1976::pg_catalog.oid, 1994::pg_catalog.oid) LIMIT 129) bounded) <= 128
FROM pg_catalog.pg_class e
JOIN pg_catalog.pg_class m ON m.oid = $2
JOIN pg_catalog.pg_namespace n ON n.oid = e.relnamespace
WHERE e.oid = $1
"#;

// pg_column_size plus rejection of compression bounds raw ACLs, array inputs,
// expression nodes, and trigger sources before expansion or deparsing. This
// caps individual values, not the total size of PostgreSQL's system catalogs.
const RAW_PREFLIGHT: &str = r#"
SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_class c
    WHERE c.oid IN ($1, $2) AND c.relacl IS NOT NULL
      AND (pg_catalog.pg_column_compression(c.relacl) IS NOT NULL
        OR pg_catalog.pg_column_size(c.relacl) > 4096))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
    WHERE a.attrelid IN ($1, $2) AND a.attacl IS NOT NULL
      AND (pg_catalog.pg_column_compression(a.attacl) IS NOT NULL
        OR pg_catalog.pg_column_size(a.attacl) > 4096))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x
    WHERE x.conrelid IN ($1, $2) AND (
      (x.conkey IS NOT NULL AND (pg_catalog.pg_column_compression(x.conkey) IS NOT NULL
        OR pg_catalog.pg_column_size(x.conkey) > 4096))
      OR (x.conbin IS NOT NULL AND (pg_catalog.pg_column_compression(x.conbin) IS NOT NULL
        OR pg_catalog.pg_column_size(x.conbin) > 4096))))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d
    WHERE d.adrelid IN ($1, $2)
      AND (pg_catalog.pg_column_compression(d.adbin) IS NOT NULL
        OR pg_catalog.pg_column_size(d.adbin) > 4096))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index i
    WHERE i.indrelid IN ($1, $2) AND (
      pg_catalog.pg_column_compression(i.indkey) IS NOT NULL OR pg_catalog.pg_column_size(i.indkey) > 4096
      OR pg_catalog.pg_column_compression(i.indclass) IS NOT NULL OR pg_catalog.pg_column_size(i.indclass) > 4096
      OR pg_catalog.pg_column_compression(i.indcollation) IS NOT NULL OR pg_catalog.pg_column_size(i.indcollation) > 4096
      OR pg_catalog.pg_column_compression(i.indoption) IS NOT NULL OR pg_catalog.pg_column_size(i.indoption) > 4096
      OR (i.indexprs IS NOT NULL AND (pg_catalog.pg_column_compression(i.indexprs) IS NOT NULL
        OR pg_catalog.pg_column_size(i.indexprs) > 4096))
      OR (i.indpred IS NOT NULL AND (pg_catalog.pg_column_compression(i.indpred) IS NOT NULL
        OR pg_catalog.pg_column_size(i.indpred) > 4096))))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t
    LEFT JOIN pg_catalog.pg_proc p ON p.oid = t.tgfoid
    WHERE t.tgrelid IN ($1, $2) AND (
      p.oid IS NULL OR pg_catalog.pg_column_compression(p.prosrc) IS NOT NULL
      OR pg_catalog.pg_column_size(p.prosrc) > 8192
      OR pg_catalog.pg_column_compression(t.tgattr) IS NOT NULL OR pg_catalog.pg_column_size(t.tgattr) > 4096
      OR pg_catalog.pg_column_compression(t.tgargs) IS NOT NULL
      OR pg_catalog.pg_column_size(t.tgargs) > 4096
      OR (t.tgqual IS NOT NULL AND (pg_catalog.pg_column_compression(t.tgqual) IS NOT NULL
        OR pg_catalog.pg_column_size(t.tgqual) > 4096))))
  AND (SELECT pg_catalog.count(*) FROM (
       SELECT 1 FROM (
         SELECT 'pg_catalog.pg_constraint'::pg_catalog.regclass AS classid, x.oid AS objid
         FROM pg_catalog.pg_constraint x WHERE x.conrelid IN ($1, $2)
         UNION ALL
         SELECT 'pg_catalog.pg_attrdef'::pg_catalog.regclass, d.oid
         FROM pg_catalog.pg_attrdef d WHERE d.adrelid IN ($1, $2)
         UNION ALL
         SELECT 'pg_catalog.pg_class'::pg_catalog.regclass, i.indexrelid
         FROM pg_catalog.pg_index i WHERE i.indrelid IN ($1, $2)
       ) objects
       CROSS JOIN LATERAL (
         SELECT 1 FROM pg_catalog.pg_depend dep
         WHERE dep.classid = objects.classid AND dep.objid = objects.objid LIMIT 129
       ) dependency LIMIT 129) bounded) <= 128
"#;

const CATALOG_SHAPE: &str = r#"
WITH targets AS (
  SELECT c.oid, c.relnamespace, c.relowner, c.relkind, c.relpersistence, c.relam,
         c.relispartition, c.reloftype, c.relrowsecurity, c.relforcerowsecurity,
         c.relhasrules, c.relacl, (c.oid = $1) AS is_epoch
  FROM pg_catalog.pg_class c WHERE c.oid IN ($1, $2)
)
SELECT (SELECT pg_catalog.count(*) FROM targets) = 2
  AND NOT EXISTS (SELECT 1 FROM targets c WHERE
    c.relnamespace <> $3 OR c.relowner <> $4 OR c.relkind <> 'r'
    OR c.relpersistence <> 'p' OR c.relam <> 2::pg_catalog.oid
    OR c.relispartition OR c.reloftype <> 0::pg_catalog.oid
    OR c.relrowsecurity OR c.relforcerowsecurity OR c.relhasrules
    OR EXISTS (SELECT 1 FROM pg_catalog.pg_inherits h WHERE h.inhrelid = c.oid OR h.inhparent = c.oid)
    OR EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid = c.oid)
    OR EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite w WHERE w.ev_class = c.oid)
    OR NOT pg_catalog.has_table_privilege($5, c.oid, 'SELECT')
    OR pg_catalog.has_table_privilege($5, c.oid, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
    OR pg_catalog.has_table_privilege($5, c.oid, 'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION,MAINTAIN WITH GRANT OPTION')
    OR pg_catalog.has_any_column_privilege($5, c.oid, 'INSERT,UPDATE,REFERENCES')
    OR pg_catalog.has_any_column_privilege($5, c.oid, 'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION')
    OR NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(c.relacl) acl
       WHERE acl.grantee = $5 AND acl.grantor = c.relowner
         AND acl.privilege_type = 'SELECT' AND NOT acl.is_grantable)
    OR EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
         COALESCE(c.relacl, pg_catalog.acldefault('r', c.relowner))) acl
       WHERE acl.grantee = 0::pg_catalog.oid
         OR (acl.grantee = $5 AND (acl.grantor <> c.relowner
           OR acl.is_grantable OR acl.privilege_type <> 'SELECT')))
    OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a
       CROSS JOIN LATERAL pg_catalog.aclexplode(a.attacl) acl
       WHERE a.attrelid = c.oid AND (acl.grantee = 0::pg_catalog.oid
         OR (acl.grantee = $5 AND (acl.grantor <> c.relowner
           OR acl.is_grantable OR acl.privilege_type <> 'SELECT')))))
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute a WHERE a.attrelid = $1 AND a.attnum > 0) = 4
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attribute a WHERE a.attrelid = $2 AND a.attnum > 0) = 1
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a WHERE a.attrelid IN ($1, $2) AND a.attnum > 0 AND (
    a.attisdropped OR a.atthasmissing OR a.attgenerated <> '' OR a.attidentity <> ''
    OR NOT a.attislocal OR a.attinhcount <> 0 OR a.attndims <> 0 OR a.atttypmod <> -1
    OR a.attoptions IS NOT NULL OR a.attfdwoptions IS NOT NULL
    OR (a.attrelid = $1 AND (
      a.atthasdef <> (a.attnum = 2)
      OR (a.attnum, a.attname, a.atttypid, a.attnotnull, a.attcollation) NOT IN (
        (1, 'id', 23::pg_catalog.oid, true, 0::pg_catalog.oid),
        (2, 'epoch', 20::pg_catalog.oid, true, 0::pg_catalog.oid),
        (3, 'activated_at', 1184::pg_catalog.oid, false, 0::pg_catalog.oid),
        (4, 'lineage_uuid', 2950::pg_catalog.oid, false, 0::pg_catalog.oid))))
    OR (a.attrelid = $2 AND (
      a.atthasdef OR (a.attnum, a.attname, a.atttypid, a.attnotnull, a.attcollation)
        <> (1, 'table_name', 25::pg_catalog.oid, true, 100::pg_catalog.oid)))))
  AND NOT EXISTS (SELECT 1 FROM targets c WHERE
    (SELECT pg_catalog.count(*) FROM pg_catalog.pg_index i WHERE i.indrelid = c.oid) <> 1
    OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index i
      JOIN pg_catalog.pg_class idx ON idx.oid = i.indexrelid
      JOIN pg_catalog.pg_am am ON am.oid = idx.relam
      JOIN pg_catalog.pg_opclass opclass ON opclass.oid = i.indclass[0]
      JOIN pg_catalog.pg_opfamily family ON family.oid = opclass.opcfamily
      WHERE i.indrelid = c.oid AND (NOT c.is_epoch OR idx.oid = $6)
        AND idx.relnamespace = $3 AND idx.relowner = $4
        AND idx.relkind = 'i' AND idx.relpersistence = 'p'
        AND idx.relam = 403::pg_catalog.oid AND am.amhandler = 330::pg_catalog.oid
        AND am.amname = 'btree' AND am.amtype = 'i'
        AND opclass.oid = CASE WHEN c.is_epoch THEN 1978::pg_catalog.oid ELSE 3126::pg_catalog.oid END
        AND opclass.opcnamespace = 11::pg_catalog.oid AND opclass.opcmethod = 403::pg_catalog.oid
        AND opclass.opcintype = CASE WHEN c.is_epoch THEN 23::pg_catalog.oid ELSE 25::pg_catalog.oid END
        AND opclass.opcdefault AND opclass.opckeytype = 0::pg_catalog.oid
        AND family.oid = CASE WHEN c.is_epoch THEN 1976::pg_catalog.oid ELSE 1994::pg_catalog.oid END
        AND family.opfnamespace = 11::pg_catalog.oid AND family.opfmethod = 403::pg_catalog.oid
        AND i.indisprimary AND i.indisunique AND i.indisvalid AND i.indisready
        AND i.indislive AND i.indimmediate AND NOT i.indisexclusion AND NOT i.indnullsnotdistinct
        AND i.indnatts = 1 AND i.indnkeyatts = 1 AND i.indkey[0] = 1
        AND i.indcollation[0] = CASE WHEN c.is_epoch THEN 0::pg_catalog.oid ELSE 100::pg_catalog.oid END
        AND i.indoption[0] = 0 AND i.indexprs IS NULL AND i.indpred IS NULL
        AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amproc ap
          JOIN pg_catalog.pg_proc p ON p.oid = ap.amproc
          WHERE ap.amprocfamily = CASE WHEN c.is_epoch THEN 1976::pg_catalog.oid ELSE 1994::pg_catalog.oid END
            AND p.pronamespace <> 11::pg_catalog.oid)
        AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_amop ao
          JOIN pg_catalog.pg_operator o ON o.oid = ao.amopopr
          JOIN pg_catalog.pg_proc p ON p.oid = o.oprcode
          WHERE ao.amopfamily = CASE WHEN c.is_epoch THEN 1976::pg_catalog.oid ELSE 1994::pg_catalog.oid END
            AND (o.oprnamespace <> 11::pg_catalog.oid OR p.pronamespace <> 11::pg_catalog.oid))))
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = $1) = 4
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = $2) = 1
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint x WHERE x.conrelid IN ($1, $2) AND (
    NOT x.convalidated OR NOT x.conislocal OR x.coninhcount <> 0 OR x.conparentid <> 0
    OR x.condeferrable OR x.condeferred
    OR (x.contype = 'c' AND (x.conrelid <> $1 OR x.connoinherit OR x.conbin IS NULL))
    OR (x.contype = 'p' AND (x.conkey IS DISTINCT FROM ARRAY[1]::pg_catalog.int2[] OR x.conbin IS NOT NULL
      OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_index i
                     WHERE i.indrelid = x.conrelid AND i.indexrelid = x.conindid AND i.indisprimary)))
    OR x.contype NOT IN ('c', 'p')))
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = $1 AND x.contype = 'c') = 3
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = $1 AND x.contype = 'p') = 1
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint x WHERE x.conrelid = $2 AND x.contype = 'p') = 1
  AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_attrdef d WHERE d.adrelid = $1 AND d.adnum = 2) = 1
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d WHERE d.adrelid IN ($1, $2) AND (d.adrelid <> $1 OR d.adnum <> 2))
  AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend dep
    WHERE ((dep.classid = 'pg_catalog.pg_constraint'::pg_catalog.regclass
            AND dep.objid IN (SELECT x.oid FROM pg_catalog.pg_constraint x WHERE x.conrelid IN ($1, $2)))
       OR (dep.classid = 'pg_catalog.pg_attrdef'::pg_catalog.regclass
            AND dep.objid IN (SELECT d.oid FROM pg_catalog.pg_attrdef d WHERE d.adrelid IN ($1, $2))))
      AND NOT (
        (dep.refclassid = 'pg_catalog.pg_class'::pg_catalog.regclass AND dep.refobjid IN ($1, $2))
        OR (dep.refclassid = 'pg_catalog.pg_proc'::pg_catalog.regclass AND EXISTS
            (SELECT 1 FROM pg_catalog.pg_proc p WHERE p.oid = dep.refobjid AND p.pronamespace = 11::pg_catalog.oid))
        OR (dep.refclassid = 'pg_catalog.pg_operator'::pg_catalog.regclass AND EXISTS
            (SELECT 1 FROM pg_catalog.pg_operator o JOIN pg_catalog.pg_proc p ON p.oid = o.oprcode
             WHERE o.oid = dep.refobjid AND o.oprnamespace = 11::pg_catalog.oid AND p.pronamespace = 11::pg_catalog.oid))
        OR (dep.refclassid = 'pg_catalog.pg_type'::pg_catalog.regclass AND EXISTS
            (SELECT 1 FROM pg_catalog.pg_type t WHERE t.oid = dep.refobjid AND t.typnamespace = 11::pg_catalog.oid))
        OR (dep.refclassid = 'pg_catalog.pg_collation'::pg_catalog.regclass AND EXISTS
            (SELECT 1 FROM pg_catalog.pg_collation co WHERE co.oid = dep.refobjid AND co.collnamespace = 11::pg_catalog.oid))))
"#;

const EXPRESSIONS: &str = r#"
SELECT (SELECT pg_catalog.count(DISTINCT pg_catalog.pg_get_expr(x.conbin, x.conrelid))
        FROM pg_catalog.pg_constraint x WHERE x.conrelid = $1 AND x.contype = 'c'
          AND pg_catalog.pg_get_expr(x.conbin, x.conrelid) IN (
            '(id = 1)',
            '((epoch >= 0) AND (epoch <= ''4294967295''::bigint))',
            '((epoch = 0) = ((activated_at IS NULL) AND (lineage_uuid IS NULL)))')) = 3
   AND EXISTS (SELECT 1 FROM pg_catalog.pg_attrdef d
               WHERE d.adrelid = $1 AND d.adnum = 2
                 AND pg_catalog.pg_get_expr(d.adbin, d.adrelid) = '0')
"#;
