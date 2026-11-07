//! Catalog-only checks before relation reads or expression deparsing.
//!
//! This is not a catalog-generation barrier. The observer still requires
//! independently quiescent trusted catalog and role mutation.

use serde_json::{json, Value};
use sqlx::PgConnection;

use super::manifest::Manifest;
use super::{RestrictedIngressProfileError, Result};
use crate::ingress_substrate::EPOCH_GUARDED_TABLES;

const PREFLIGHT_SQL: &str = include_str!("preflight.sql");
const EXPRESSION_DEPENDENCIES_SQL: &str = include_str!("expression_dependencies.sql");
const INVENTORY_BOUNDS: &str = include_str!("inventory_bounds.sql");
const PHYSICAL_BOUNDS: &str = include_str!("physical_bounds.sql");
const STORAGE_BOUNDS: &str = include_str!("storage_bounds.sql");
const ARRAY_BOUNDS: &str = include_str!("array_bounds.sql");

/// Bound inventories first, then raw variable-length inputs before any ACL
/// expansion, catalog JSON aggregation, expression deparsing, or SQLx decoding.
/// Both queries run under the caller's cooperative catalog mutation fence.
pub(super) async fn bound(connection: &mut PgConnection, schema: &str) -> Result<()> {
    for statement in [
        INVENTORY_BOUNDS,
        PHYSICAL_BOUNDS,
        STORAGE_BOUNDS,
        ARRAY_BOUNDS,
    ] {
        let valid: bool = sqlx::query_scalar(statement)
            .bind(schema)
            .fetch_one(&mut *connection)
            .await?;
        if !valid {
            return Err(RestrictedIngressProfileError::Mismatch(
                "bounded catalog inventory/storage",
            ));
        }
    }
    Ok(())
}

pub(super) async fn verify_expression_dependencies(
    connection: &mut PgConnection,
    schema: &str,
    tables: &[&str],
) -> Result<()> {
    let unsafe_dependency: bool = sqlx::query_scalar(EXPRESSION_DEPENDENCIES_SQL)
        .bind(schema)
        .bind(tables)
        .fetch_one(&mut *connection)
        .await?;
    if unsafe_dependency {
        return Err(RestrictedIngressProfileError::Mismatch(
            "expression dependency preflight",
        ));
    }
    Ok(())
}

pub(super) async fn verify(
    connection: &mut PgConnection,
    schema: &str,
    manifest: &Manifest,
) -> Result<()> {
    let mut tables = EPOCH_GUARDED_TABLES.to_vec();
    tables.extend([
        "ingress_protocol_epoch",
        "ingress_epoch_guard_manifest",
        "sessions",
        "pending_delivery",
    ]);
    tables.sort_unstable();

    // No trigger in the pinned profile has a WHEN expression. Reject one
    // without asking pg_get_expr to interpret its potentially hostile nodes.
    let qualified_trigger: bool = sqlx::query_scalar(
        r#"SELECT EXISTS (
          SELECT 1 FROM pg_catalog.pg_trigger t
          JOIN pg_catalog.pg_class c ON c.oid=t.tgrelid
          JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace
          WHERE n.nspname=$1 AND c.relname=ANY($2) AND t.tgqual IS NOT NULL)"#,
    )
    .bind(schema)
    .bind(&tables)
    .fetch_one(&mut *connection)
    .await?;
    if qualified_trigger {
        return Err(RestrictedIngressProfileError::Mismatch(
            "trigger expression preflight",
        ));
    }

    // Deparsing a Const can invoke its type's output function. Besides exact
    // built-in column types/collations, reject user-defined dependencies of
    // defaults, constraints and indexes before calling any deparser.
    verify_expression_dependencies(connection, schema, &tables).await?;

    let expected = expected_shape(manifest, schema, &tables)?;
    let observed: Value = sqlx::query_scalar(PREFLIGHT_SQL)
        .bind(schema)
        .bind(&tables)
        .fetch_one(&mut *connection)
        .await?;
    if observed != expected {
        return Err(RestrictedIngressProfileError::Mismatch(
            "column/trigger preflight",
        ));
    }
    Ok(())
}

fn expected_shape(manifest: &Manifest, schema: &str, tables: &[&str]) -> Result<Value> {
    let bad_manifest = || RestrictedIngressProfileError::Mismatch("preflight manifest encoding");
    let structure = manifest.structure_for_schema(schema);
    let mut expected = Vec::new();
    for table in structure.as_array().ok_or_else(bad_manifest)? {
        let name = table["name"].as_str().ok_or_else(bad_manifest)?;
        let mut columns = Vec::new();
        for column in table["columns"].as_array().ok_or_else(bad_manifest)? {
            let properties = &column["properties"];
            let ty = properties["atttypid"].as_str().ok_or_else(bad_manifest)?;
            let collation = properties["attcollation"]
                .as_str()
                .ok_or_else(bad_manifest)?;
            if !matches!(
                (ty, collation),
                ("25", "100") | ("17" | "20" | "21" | "23" | "1184" | "1700" | "2950", "0")
            ) || properties["attisdropped"] != false
            {
                return Err(bad_manifest());
            }
            columns.push(json!({
                "attname": properties["attname"],
                "attnum": properties["attnum"],
                "atttypid": ty,
                "attcollation": collation,
                "attisdropped": false,
            }));
        }
        let triggers = table["triggers"].as_array().ok_or_else(bad_manifest)?;
        if triggers.iter().any(|trigger| !trigger["when"].is_null()) {
            return Err(bad_manifest());
        }
        expected.push(json!({"name": name, "columns": columns, "triggers": triggers}));
    }
    expected.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    if expected
        .iter()
        .map(|t| t["name"].as_str())
        .ne(tables.iter().copied().map(Some))
    {
        return Err(bad_manifest());
    }
    Ok(Value::Array(expected))
}
