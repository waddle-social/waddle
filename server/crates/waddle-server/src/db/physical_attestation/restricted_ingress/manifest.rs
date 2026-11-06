//! Versioned PG17 function and non-internal trigger catalog contract.
//! Bodies are byte slices of the checksum-pinned, unregistered source assets;
//! no live function is invoked to decide whether its body can be trusted.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, Row};

use super::{RestrictedIngressProfileError, Result};
use crate::db::physical_attestation::quote_identifier;

const CANDIDATE: &str = include_str!("../../migrations/owner/ingress_definer_api_candidate.sql");
const RELATION: &str =
    include_str!("../../migrations/owner/ingress_epoch_relation_guard_candidate.sql");
const GLOBAL: &str = include_str!("../../migrations/global.rs");
const WADDLE: &str = include_str!("../../migrations/waddle.rs");
const PENDING_SCHEMA: &str = include_str!("../../../pending_delivery/database/schema.rs");
const PENDING_RECOVERY: &str = include_str!("../../../pending_delivery/database/recovery_order.rs");
const STRUCTURE_SQL: &str = include_str!("structure.sql");
const MANIFEST: &str = include_str!("../restricted_ingress_manifest_v2.json");

#[derive(Deserialize)]
pub(super) struct Manifest {
    version: u32,
    postgres_major: u32,
    source_sha256: BTreeMap<String, String>,
    functions: Vec<Function>,
    triggers: Vec<Value>,
    structure: Value,
}

impl Manifest {
    pub(super) fn structure_for_schema(&self, schema: &str) -> Value {
        fn replace(value: &Value, schema: &str) -> Value {
            match value {
                Value::String(value) => Value::String(value.replace("__SCHEMA_NAME__", schema)),
                Value::Array(values) => {
                    Value::Array(values.iter().map(|value| replace(value, schema)).collect())
                }
                Value::Object(values) => Value::Object(
                    values
                        .iter()
                        .map(|(key, value)| (key.clone(), replace(value, schema)))
                        .collect(),
                ),
                _ => value.clone(),
            }
        }
        replace(&self.structure, schema)
    }
}

#[derive(Deserialize)]
struct Function {
    name: String,
    properties: Value,
    source_file: String,
    source_start: usize,
    source_end: usize,
    owner: String,
    public_execute: bool,
}

fn source(key: &str) -> Result<&'static str> {
    match key {
        "candidate" => Ok(CANDIDATE),
        "relation" => Ok(RELATION),
        "waddle" => Ok(WADDLE),
        "global" => Ok(GLOBAL),
        "pending_schema" => Ok(PENDING_SCHEMA),
        "pending_recovery" => Ok(PENDING_RECOVERY),
        "structure_query" => Ok(STRUCTURE_SQL),
        _ => Err(RestrictedIngressProfileError::Mismatch("manifest source")),
    }
}

fn render(body: &str, schema: &str, runtime: &str, api: &str) -> String {
    body.replace("__SCHEMA_PATH__", &quote_identifier(schema))
        .replace("__SCHEMA__", &quote_identifier(schema))
        .replace("__API_OWNER_LITERAL__", api)
        .replace("__RUNTIME_ROLE_LITERAL__", runtime)
}

pub(super) fn load() -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_str(MANIFEST)
        .map_err(|_| RestrictedIngressProfileError::Mismatch("manifest encoding"))?;
    if manifest.version != super::MANIFEST_VERSION
        || manifest.postgres_major != 17
        || manifest.source_sha256.len() != 7
    {
        return Err(RestrictedIngressProfileError::Mismatch("manifest version"));
    }
    for (key, expected) in &manifest.source_sha256 {
        if hex::encode(Sha256::digest(source(key)?.as_bytes())) != *expected {
            return Err(RestrictedIngressProfileError::Mismatch(
                "manifest source digest",
            ));
        }
    }
    Ok(manifest)
}

/// Check functions and triggers before any application relation is read.
/// The journal profile separately pins its extra function before using it.
pub(super) async fn verify_functions_and_triggers(
    connection: &mut PgConnection,
    schema: &str,
    runtime: &str,
    api: &str,
    receipt_journal: bool,
    manifest: &Manifest,
) -> Result<()> {
    let function_sql = r#"
        SELECT p.proname::text AS name,
          (pg_catalog.to_jsonb(p) - ARRAY['oid','pronamespace','proowner','prolang','prosrc','proacl']) AS properties,
          p.prosrc, l.lanname::text AS language,
          p.proowner = CASE WHEN $4 = 'api' THEN api.oid ELSE n.nspowner END AS owner,
          NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode(
            COALESCE(p.proacl, pg_catalog.acldefault('f',p.proowner))) a
            WHERE a.privilege_type <> 'EXECUTE' OR a.is_grantable AND a.grantee <> p.proowner
              OR a.grantee NOT IN (p.proowner, CASE WHEN $5 THEN 0::oid ELSE r.oid END))
            AND EXISTS (SELECT 1 FROM pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a WHERE a.grantee=p.proowner AND a.privilege_type='EXECUTE')
            AND CASE WHEN $5 THEN EXISTS(SELECT 1 FROM pg_catalog.aclexplode(COALESCE(p.proacl,pg_catalog.acldefault('f',p.proowner))) a WHERE a.grantee=0)
                WHEN $6 THEN pg_catalog.has_function_privilege(r.oid,p.oid,'EXECUTE')
                ELSE NOT pg_catalog.has_function_privilege(r.oid,p.oid,'EXECUTE') END AS acl
        FROM pg_catalog.pg_proc p
        JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
        JOIN pg_catalog.pg_language l ON l.oid=p.prolang
        JOIN pg_catalog.pg_roles r ON r.rolname=$2
        JOIN pg_catalog.pg_roles api ON api.rolname=$3
        WHERE n.nspname=$1 AND p.proname=$7
    "#;
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace WHERE n.nspname=$1 AND (pg_catalog.starts_with(p.proname,'waddle_ingress_') OR p.proname='pending_notification_recovery_ordinal_immutable')",
    ).bind(schema).fetch_one(&mut *connection).await?;
    if usize::try_from(count).ok() != Some(manifest.functions.len()) {
        return Err(RestrictedIngressProfileError::Mismatch(
            "function inventory",
        ));
    }
    for expected in &manifest.functions {
        let executable = expected.source_file == "candidate"
            && !matches!(
                expected.name.as_str(),
                "waddle_ingress_definer_prepare" | "waddle_ingress_definer_epoch_guard"
            );
        let observed = sqlx::query(function_sql)
            .bind(schema)
            .bind(runtime)
            .bind(api)
            .bind(&expected.owner)
            .bind(expected.public_execute)
            .bind(executable)
            .bind(&expected.name)
            .fetch_all(&mut *connection)
            .await?;
        if observed.len() != 1 {
            return Err(RestrictedIngressProfileError::Mismatch(
                "function identity/overload",
            ));
        }
        let row = &observed[0];
        let body = source(&expected.source_file)?
            .get(expected.source_start..expected.source_end)
            .ok_or(RestrictedIngressProfileError::Mismatch(
                "manifest body slice",
            ))?;
        if row.try_get::<Value, _>("properties")? != expected.properties
            || row.try_get::<String, _>("prosrc")? != render(body, schema, runtime, api)
            || row.try_get::<String, _>("language")? != "plpgsql"
            || !row.try_get::<bool, _>("owner")?
            || !row.try_get::<bool, _>("acl")?
        {
            return Err(RestrictedIngressProfileError::Mismatch(
                "function body/properties/owner/ACL",
            ));
        }
    }
    // Reject alternate callable definers across the entire database, including
    // different owners with effective guarded DML or lock-taking column grants.
    // Reject even non-runtime-callable alternatives: otherwise differently owned
    // wrappers could chain EXECUTE grants to reach unmanifested authority.
    let names: Vec<&str> = manifest.functions.iter().map(|f| f.name.as_str()).collect();
    let api_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_proc p JOIN pg_catalog.pg_roles a ON a.oid=p.proowner WHERE a.rolname=$1")
        .bind(api).fetch_one(&mut *connection).await?;
    if usize::try_from(api_count).ok()
        != Some(
            manifest
                .functions
                .iter()
                .filter(|f| f.owner == "api")
                .count()
                + 2 * usize::from(receipt_journal),
        )
    {
        return Err(RestrictedIngressProfileError::Mismatch(
            "API owner function inventory",
        ));
    }
    let guarded: Vec<&str> = crate::ingress_substrate::EPOCH_GUARDED_TABLES.to_vec();
    let mut names = names;
    if receipt_journal {
        names.push("waddle_p21_record_effect_receipt");
        names.push("waddle_p21_lookup_receipt_operation");
    }
    let alternate: bool = sqlx::query_scalar(r#"
        SELECT EXISTS (
          SELECT 1 FROM pg_catalog.pg_proc p
          JOIN pg_catalog.pg_namespace n ON n.oid=p.pronamespace
          JOIN pg_catalog.pg_roles r ON r.rolname=$2
          WHERE p.prosecdef
            AND NOT (n.nspname=$1 AND p.proname=ANY($3))
            AND EXISTS (SELECT 1 FROM pg_catalog.pg_class c
              JOIN pg_catalog.pg_namespace target ON target.oid=c.relnamespace
              WHERE target.nspname=$1 AND c.relname=ANY($4)
                AND (pg_catalog.has_table_privilege(p.proowner,c.oid,'INSERT,UPDATE,DELETE,TRUNCATE')
                  OR pg_catalog.has_any_column_privilege(p.proowner,c.oid,'INSERT,UPDATE'))))
    "#).bind(schema).bind(runtime).bind(&names).bind(&guarded)
        .fetch_one(&mut *connection).await?;
    if alternate {
        return Err(RestrictedIngressProfileError::Mismatch(
            "alternate callable definer authority",
        ));
    }
    let triggers: Value = sqlx::query_scalar(r#"
        SELECT COALESCE(pg_catalog.jsonb_agg(pg_catalog.jsonb_build_object(
          'table',c.relname,'function',p.proname,
          'properties',(pg_catalog.to_jsonb(t)-ARRAY['oid','tgrelid','tgfoid','tgconstraint','tgconstrrelid','tgconstrindid','tgparentid'])) ORDER BY c.relname,t.tgname),'[]'::jsonb)
        FROM pg_catalog.pg_trigger t
        JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid
        JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
        JOIN pg_catalog.pg_proc p ON p.oid=t.tgfoid
        WHERE n.nspname=$1 AND pg_catalog.starts_with(c.relname,'ingress_') AND NOT t.tgisinternal
          AND p.pronamespace=n.oid AND t.tgconstraint=0 AND t.tgconstrrelid=0 AND t.tgconstrindid=0 AND t.tgparentid=0
    "#).bind(schema).fetch_one(&mut *connection).await?;
    let actual_count: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_trigger t JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND pg_catalog.starts_with(c.relname,'ingress_') AND NOT t.tgisinternal")
        .bind(schema).fetch_one(&mut *connection).await?;
    if triggers != Value::Array(manifest.triggers.clone())
        || triggers.as_array().map(Vec::len) != usize::try_from(actual_count).ok()
    {
        return Err(RestrictedIngressProfileError::Mismatch(
            "trigger inventory/properties",
        ));
    }
    Ok(())
}

pub(super) async fn verify_structure(
    connection: &mut PgConnection,
    schema: &str,
    manifest: &Manifest,
) -> Result<()> {
    let mut tables: Vec<&str> = crate::ingress_substrate::EPOCH_GUARDED_TABLES.to_vec();
    tables.extend([
        "ingress_protocol_epoch",
        "ingress_epoch_guard_manifest",
        "sessions",
        "pending_delivery",
    ]);
    let structure: Value = sqlx::query_scalar(STRUCTURE_SQL)
        .bind(schema)
        .bind(&tables)
        .fetch_one(&mut *connection)
        .await?;
    let expected_structure = manifest.structure_for_schema(schema);
    if structure != expected_structure {
        return Err(RestrictedIngressProfileError::Mismatch(
            "table structural manifest",
        ));
    }
    Ok(())
}
