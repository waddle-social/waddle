//! Initial schema migration (version 1), which builds the complete schema.
//!
//! Version 1 (`sql/v1/sqlite.sql` and `sql/v1/postgres.sql`) is the single
//! initial entry of the server's migration catalog ([`MigrationRunner::single`]).
//! On PostgreSQL the SQL is a template whose schema placeholders (`__SCHEMA__`,
//! `__SCHEMA_QUOTED__`, `__SCHEMA_LITERAL__`) are rendered for the active
//! schema on the search path when applied.
//!
//! [`MigrationRunner::single`]: super::MigrationRunner::single

use thiserror::Error;

use super::{Migration, MigrationLedgerError};
use crate::db::{DatabaseDriver, DatabaseError, Transaction};

/// Ledger version of the initial schema migration.
pub const INITIAL_SCHEMA_VERSION: i64 = 1;

/// Compatibility alias for [`INITIAL_SCHEMA_VERSION`].
pub const BASELINE_VERSION: i64 = INITIAL_SCHEMA_VERSION;

const INITIAL_SCHEMA_DESCRIPTION: &str = "initial schema v1";

/// Legacy environment variable retained for startup config validation.
pub const BASELINE_ACTION_ENV: &str = "WADDLE_DB_BASELINE_ACTION";

const POSTGRES_SQL: &str = include_str!("../../../sql/v1/postgres.sql");
const SQLITE_SQL: &str = include_str!("../../../sql/v1/sqlite.sql");

/// Legacy baseline action setting accepted at startup.
///
/// The migration catalog is now a single `v1` schema with no retired ledger to
/// stamp, so both accepted values simply allow normal migration execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BaselineAction {
    /// Apply pending migrations cleanly.
    #[default]
    Apply,
    /// Accepted for backward-compatible CLI/environment parsing.
    Stamp,
}

/// The value of `WADDLE_DB_BASELINE_ACTION` is not a recognized action.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("WADDLE_DB_BASELINE_ACTION must be unset, 'apply' or 'stamp'")]
pub struct BaselineActionError;

impl BaselineAction {
    /// Parse the variable's value. Unset defaults to [`Self::Apply`]; any
    /// unrecognized value is refused so typos fail fast at startup.
    pub fn parse(value: Option<&str>) -> Result<Self, BaselineActionError> {
        match value {
            None | Some("apply") => Ok(Self::Apply),
            Some("stamp") => Ok(Self::Stamp),
            Some(_) => Err(BaselineActionError),
        }
    }

    /// Read `WADDLE_DB_BASELINE_ACTION`.
    pub fn from_env() -> Result<Self, BaselineActionError> {
        match std::env::var(BASELINE_ACTION_ENV) {
            Ok(value) => Self::parse(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::parse(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(BaselineActionError),
        }
    }
}

/// Validate `WADDLE_DB_BASELINE_ACTION` if set in the environment.
pub fn select_from_env() -> Result<(), String> {
    let _ = BaselineAction::from_env().map_err(|error| error.to_string())?;
    Ok(())
}

/// The initial v1 schema as a catalog entry. Its PostgreSQL SQL is a template;
/// the checksum is of the template, so it does not depend on the schema's name.
pub(super) fn migration() -> Migration {
    Migration {
        version: INITIAL_SCHEMA_VERSION,
        description: INITIAL_SCHEMA_DESCRIPTION.to_string(),
        sql_sqlite: SQLITE_SQL,
        sql_postgres: POSTGRES_SQL,
    }
}

/// The body of a function the PostgreSQL v1 schema creates, exactly as
/// PostgreSQL stores it (`pg_proc.prosrc`). A template function's body keeps
/// its schema placeholders; [`render_postgres_template`] fills them. Code
/// that pins an installed function compares its source with this.
pub(crate) fn postgres_function_body(name: &str) -> Option<&'static str> {
    let header = format!("\nCREATE FUNCTION {name}(");
    let (_, definition) = POSTGRES_SQL.split_once(&header)?;
    let (_, body) = definition.split_once("\nAS $function$")?;
    let (body, _) = body.split_once("$function$;\n")?;
    Some(body)
}

/// Replace the schema placeholders of PostgreSQL template SQL. `ident` and
/// `literal` are the server's `quote_ident` and `quote_literal` of the
/// schema's name.
pub(crate) fn render_postgres_template(
    text: &str,
    name: &str,
    ident: &str,
    literal: &str,
) -> String {
    let quoted = format!("\"{}\"", name.replace('"', "\"\""));
    text.replace("__SCHEMA_QUOTED__", &quoted)
        .replace("__SCHEMA_LITERAL__", literal)
        .replace("__SCHEMA__", ident)
}

/// Apply the v1 schema SQL in the migration transaction. On PostgreSQL the
/// template's placeholders are replaced by the name of the schema that leads
/// the search path.
pub(super) async fn apply(
    tx: &mut Transaction<'_>,
    driver: DatabaseDriver,
    migration: &Migration,
) -> Result<(), DatabaseError> {
    match driver {
        DatabaseDriver::Sqlite => tx.execute_batch(migration.sql_sqlite).await,
        DatabaseDriver::Postgres => {
            let mut rows = tx
                .query(
                    "SELECT pg_catalog.quote_ident(pg_catalog.current_schema()::text), \
                            pg_catalog.quote_literal(pg_catalog.current_schema()::text), \
                            pg_catalog.current_schema()::text",
                    (),
                )
                .await?;
            let row = rows
                .next()
                .await?
                .ok_or(MigrationLedgerError::UnsafeTargetSchema)?;
            let (bare, literal, name): (String, String, String) =
                (row.get(0)?, row.get(1)?, row.get(2)?);
            drop(rows);
            let sql = render_postgres_template(migration.sql_postgres, &name, &bare, &literal);
            tx.execute_batch(&sql).await
        }
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;
    use crate::db::migrations::migration_checksum;

    #[test]
    fn the_action_defaults_to_apply_and_rejects_unknown_values() {
        assert_eq!(BaselineAction::parse(None), Ok(BaselineAction::Apply));
        assert_eq!(
            BaselineAction::parse(Some("apply")),
            Ok(BaselineAction::Apply)
        );
        assert_eq!(
            BaselineAction::parse(Some("stamp")),
            Ok(BaselineAction::Stamp)
        );
        for value in ["", "Stamp", "apply ", "true"] {
            assert_eq!(BaselineAction::parse(Some(value)), Err(BaselineActionError));
        }
    }

    #[test]
    fn function_bodies_are_read_from_the_postgres_v1_schema() {
        let guard = postgres_function_body("waddle_ingress_truncate_guard").expect("guard");
        assert!(guard.contains("TRUNCATE is not permitted"));
        assert!(!guard.contains("$function$"));
        let receipt = postgres_function_body("waddle_p21_record_effect_receipt").expect("receipt");
        assert!(receipt.contains("__SCHEMA__.p21_receipt_operation_results"));
        let account_guard =
            postgres_function_body("reject_immutable_account_id_change").expect("account_id guard");
        assert!(account_guard.contains("account_id is immutable"));
        assert_eq!(postgres_function_body("waddle_ingress_epoch"), None);
        assert_eq!(
            render_postgres_template(
                "__SCHEMA__ __SCHEMA_QUOTED__ __SCHEMA_LITERAL__",
                "tenant_a",
                "tenant_a",
                "'tenant_a'"
            ),
            "tenant_a \"tenant_a\" 'tenant_a'"
        );
    }

    #[test]
    fn the_v1_checksum_does_not_depend_on_the_schema() {
        let initial = migration();
        assert_eq!(initial.version, INITIAL_SCHEMA_VERSION);
        assert_eq!(initial.version, 1);
        assert_eq!(initial.description, "initial schema v1");
        assert!(initial.sql_postgres.contains("__SCHEMA"));
        assert_eq!(
            migration_checksum(&initial, DatabaseDriver::Postgres),
            hex::encode(Sha256::digest(POSTGRES_SQL.as_bytes()))
        );
    }
}
