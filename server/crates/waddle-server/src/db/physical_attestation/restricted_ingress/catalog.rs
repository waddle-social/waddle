//! Restricted role, relation and ACL profile. This never installs the profile.

use sqlx::postgres::types::Oid;
use sqlx::{PgConnection, Row};

use super::{RestrictedIngressProfileError, Result};
use crate::ingress_substrate::EPOCH_GUARDED_TABLES;

pub(super) async fn verify_roles_and_tables(
    connection: &mut PgConnection,
    schema: &str,
    schema_oid: u32,
    runtime: &str,
    api: &str,
    epoch_oid: u32,
) -> Result<(u32, u32)> {
    let row = sqlx::query(r#"
        SELECT r.oid AS runtime_oid, a.oid AS api_oid,
          r.rolcanlogin AND NOT a.rolcanlogin
          AND NOT (r.rolsuper OR r.rolcreatedb OR r.rolcreaterole OR r.rolreplication OR r.rolbypassrls)
          AND NOT (a.rolsuper OR a.rolcreatedb OR a.rolcreaterole OR a.rolreplication OR a.rolbypassrls)
          AND r.oid<>a.oid AND n.nspowner NOT IN (r.oid,a.oid) AND db.datdba NOT IN (r.oid,a.oid)
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m WHERE m.member IN (r.oid,a.oid) OR m.roleid IN (r.oid,a.oid))
          AND NOT pg_catalog.has_schema_privilege(r.oid,n.oid,'CREATE')
          AND NOT pg_catalog.has_schema_privilege(a.oid,n.oid,'CREATE')
          AND pg_catalog.has_schema_privilege(r.oid,n.oid,'USAGE')
          AND pg_catalog.has_schema_privilege(a.oid,n.oid,'USAGE')
          AND NOT pg_catalog.has_database_privilege(r.oid,db.oid,'CREATE,TEMP')
          AND NOT pg_catalog.has_database_privilege(a.oid,db.oid,'CREATE,TEMP')
          AND NOT pg_catalog.has_parameter_privilege(r.oid,'session_replication_role','SET')
          AND NOT pg_catalog.has_parameter_privilege(a.oid,'session_replication_role','SET')
          AND n.oid=$4 AND epoch.oid=$5 AS exact
        FROM pg_catalog.pg_namespace n
        JOIN pg_catalog.pg_roles r ON r.rolname=$2
        JOIN pg_catalog.pg_roles a ON a.rolname=$3
        JOIN pg_catalog.pg_database db ON db.datname=pg_catalog.current_database()
        JOIN pg_catalog.pg_class epoch ON epoch.relnamespace=n.oid AND epoch.relname='ingress_protocol_epoch'
        WHERE n.nspname=$1
    "#).bind(schema).bind(runtime).bind(api).bind(Oid(schema_oid)).bind(Oid(epoch_oid))
        .fetch_optional(&mut *connection).await?
        .ok_or(RestrictedIngressProfileError::Mismatch("schema/epoch/role identity"))?;
    if !row.try_get::<bool, _>("exact")? {
        return Err(RestrictedIngressProfileError::Mismatch(
            "role privileges/identity",
        ));
    }
    let runtime_oid = row.try_get::<Oid, _>("runtime_oid")?.0;
    let api_oid = row.try_get::<Oid, _>("api_oid")?.0;
    let mut expected: Vec<String> = EPOCH_GUARDED_TABLES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    expected.extend([
        "ingress_protocol_epoch".to_owned(),
        "ingress_epoch_guard_manifest".to_owned(),
    ]);
    expected.sort();
    let live: Vec<String> = sqlx::query_scalar("SELECT c.relname::text FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND pg_catalog.starts_with(c.relname,'ingress_') AND c.relkind NOT IN ('i','I') ORDER BY c.relname")
        .bind(schema).fetch_all(&mut *connection).await?;
    if live != expected {
        return Err(RestrictedIngressProfileError::Mismatch(
            "guarded relation inventory",
        ));
    }
    let guarded: Vec<String> = EPOCH_GUARDED_TABLES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let exact: bool = sqlx::query_scalar(r#"
        SELECT COALESCE(bool_and(
          c.relkind='r' AND c.relpersistence='p' AND c.relowner=n.nspowner
          AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relispartition
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy pol WHERE pol.polrelid=c.oid)
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits i WHERE i.inhrelid=c.oid OR i.inhparent=c.oid)
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(c.relacl,pg_catalog.acldefault('r',c.relowner))) acl
            WHERE acl.grantee<>c.relowner AND (
              acl.is_grantable OR NOT (
                (acl.grantee=monitor.oid AND acl.privilege_type='SELECT')
                OR (c.relname=ANY($4) AND acl.grantee=a.oid AND acl.privilege_type IN ('SELECT','INSERT','UPDATE','DELETE'))
                OR (c.relname='ingress_protocol_epoch' AND acl.grantee IN (a.oid,r.oid) AND acl.privilege_type='SELECT')
                OR (c.relname='ingress_epoch_guard_manifest' AND acl.grantee=r.oid AND acl.privilege_type='SELECT'))))
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute att CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) acl
            WHERE att.attrelid=c.oid AND acl.grantee<>c.relowner AND NOT (
              c.relname='ingress_protocol_epoch' AND att.attname='id' AND acl.grantee=a.oid
              AND acl.privilege_type='UPDATE' AND NOT acl.is_grantable))
          AND CASE WHEN c.relname=ANY($4) THEN
            pg_catalog.has_table_privilege(a.oid,c.oid,'SELECT')
            AND pg_catalog.has_table_privilege(a.oid,c.oid,'INSERT')
            AND pg_catalog.has_table_privilege(a.oid,c.oid,'UPDATE')
            AND pg_catalog.has_table_privilege(a.oid,c.oid,'DELETE')
            AND NOT pg_catalog.has_table_privilege(r.oid,c.oid,'SELECT,INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER,MAINTAIN')
            AND NOT pg_catalog.has_any_column_privilege(r.oid,c.oid,'SELECT,INSERT,UPDATE,REFERENCES')
          WHEN c.relname='ingress_protocol_epoch' THEN
            pg_catalog.has_table_privilege(r.oid,c.oid,'SELECT')
            AND pg_catalog.has_table_privilege(a.oid,c.oid,'SELECT')
            AND pg_catalog.has_column_privilege(a.oid,c.oid,'id','UPDATE')
          ELSE pg_catalog.has_table_privilege(r.oid,c.oid,'SELECT') END
        ),false)
        FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
        JOIN pg_catalog.pg_roles r ON r.rolname=$2
        JOIN pg_catalog.pg_roles a ON a.rolname=$3
        JOIN pg_catalog.pg_roles monitor ON monitor.rolname='pg_monitor'
        WHERE n.nspname=$1 AND c.relname=ANY($5)
    "#).bind(schema).bind(runtime).bind(api).bind(&guarded).bind(&expected)
        .fetch_one(&mut *connection).await?;
    if !exact {
        return Err(RestrictedIngressProfileError::Mismatch(
            "guarded relation/ACL/row security",
        ));
    }
    // The fixed API reads session and legacy pending-delivery custody outside
    // the guarded graph. It may lock only sessions.id, never mutate sessions.
    let dependencies: i64 = sqlx::query_scalar(r#"
        SELECT count(*) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
        JOIN pg_catalog.pg_roles a ON a.rolname=$2
        WHERE n.nspname=$1 AND c.relname IN ('sessions','pending_delivery')
          AND c.relkind='r' AND c.relpersistence='p' AND c.relowner=n.nspowner
          AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity AND NOT c.relispartition
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy p WHERE p.polrelid=c.oid)
          AND pg_catalog.has_table_privilege(a.oid,c.oid,'SELECT')
          AND NOT pg_catalog.has_table_privilege(a.oid,c.oid,'INSERT,UPDATE,DELETE,TRUNCATE,TRIGGER,REFERENCES,MAINTAIN')
          AND NOT pg_catalog.has_table_privilege(a.oid,c.oid,'SELECT WITH GRANT OPTION')
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute att
            WHERE att.attrelid=c.oid AND att.attnum>0 AND NOT att.attisdropped AND (
              pg_catalog.has_column_privilege(a.oid,c.oid,att.attnum,'INSERT,REFERENCES')
              OR (NOT (c.relname='sessions' AND att.attname='id')
                  AND pg_catalog.has_column_privilege(a.oid,c.oid,att.attnum,'UPDATE'))))
          AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute att CROSS JOIN LATERAL pg_catalog.aclexplode(att.attacl) acl
            WHERE att.attrelid=c.oid AND acl.grantee=a.oid AND NOT (
              c.relname='sessions' AND att.attname='id' AND acl.privilege_type='UPDATE' AND NOT acl.is_grantable))
          AND (c.relname<>'sessions' OR pg_catalog.has_column_privilege(a.oid,c.oid,'id','UPDATE'))
    "#).bind(schema).bind(api).fetch_one(&mut *connection).await?;
    if dependencies != 2 {
        return Err(RestrictedIngressProfileError::Mismatch(
            "API dependency privileges",
        ));
    }
    let actual_manifest: Vec<String> = sqlx::query_scalar(&format!(
        "SELECT table_name::text FROM {}.ingress_epoch_guard_manifest ORDER BY table_name",
        crate::db::physical_attestation::quote_identifier(schema),
    ))
    .fetch_all(&mut *connection)
    .await?;
    let mut expected_manifest = guarded;
    expected_manifest.sort();
    if actual_manifest != expected_manifest {
        return Err(RestrictedIngressProfileError::Mismatch(
            "guard manifest rows",
        ));
    }
    Ok((runtime_oid, api_oid))
}
