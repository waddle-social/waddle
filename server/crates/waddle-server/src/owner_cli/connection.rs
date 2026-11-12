//! Database connections of the operator subcommands.

use std::str::FromStr;

use sqlx::postgres::PgConnectOptions;
use url::Url;

use crate::db::physical_attestation::PgRoleName;
use crate::db::{Database, DatabaseConfig, DatabaseDriver};

use super::args::OwnerConnectionArgs;
use super::error::OwnerCliError;

const DATABASE_URL: &str = "WADDLE_DATABASE_URL";

/// The URL this invocation connects with. Its value is a credential, so
/// errors name the variable and never quote it.
pub(super) fn database_url() -> Result<String, OwnerCliError> {
    secret_env(DATABASE_URL)?
        .ok_or_else(|| OwnerCliError::Configuration(format!("{DATABASE_URL} is required")))
}

/// A secret environment variable: absent, or present and not blank.
pub(super) fn secret_env(name: &str) -> Result<Option<String>, OwnerCliError> {
    match std::env::var(name) {
        Ok(value) if value.trim().is_empty() => Err(OwnerCliError::Configuration(format!(
            "{name} is set but empty"
        ))),
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(std::env::VarError::NotUnicode(_)) => Err(OwnerCliError::Configuration(format!(
            "{name} is not valid Unicode"
        ))),
    }
}

/// Environment variables the PostgreSQL client library applies to every
/// connection. Set, they would add startup options or rename the session
/// behind the subcommand's back.
const AMBIENT_SESSION_SETTINGS: [&str; 2] = ["PGOPTIONS", "PGAPPNAME"];

/// Parse `url` as PostgreSQL connection options for `login`.
///
/// The subcommand decides the session's role, search path and name. A URL
/// that sets startup options (`options` or `options[...]`) or an
/// `application_name`, and an environment that sets `PGOPTIONS` or
/// `PGAPPNAME`, are refused: either could put the session under another
/// role or search path, or make it indistinguishable from another client.
pub(super) fn connect_options(
    name: &str,
    url: &str,
    login: &PgRoleName,
) -> Result<PgConnectOptions, OwnerCliError> {
    for variable in AMBIENT_SESSION_SETTINGS {
        if std::env::var_os(variable).is_some() {
            return Err(OwnerCliError::Configuration(format!(
                "{variable} must not be set; the subcommand sets its own session options and name"
            )));
        }
    }
    let invalid =
        || OwnerCliError::Configuration(format!("{name} is not a PostgreSQL connection URL"));
    let parsed = Url::parse(url).map_err(|_| invalid())?;
    if parsed.query_pairs().any(|(key, _)| {
        key == "options" || key.starts_with("options[") || key == "application_name"
    }) {
        return Err(OwnerCliError::Configuration(format!(
            "{name} must not set `options`, `options[...]` or `application_name`; the \
             subcommand sets the role, the search path and its session name"
        )));
    }
    let options = PgConnectOptions::from_str(url).map_err(|_| invalid())?;
    if options.get_username() != login.as_str() {
        return Err(OwnerCliError::Configuration(format!(
            "{name} must connect as the login {}",
            login.as_str()
        )));
    }
    Ok(options)
}

/// Open the owner handle: the migrator login, acting as the schema owner,
/// with the application schema as its search path. Every session carries
/// `application_name`, which a clustered owner phase uses to tell its own
/// sessions from any other client's.
pub(super) async fn open_owner(
    args: &OwnerConnectionArgs,
    application_name: &str,
) -> Result<Database, OwnerCliError> {
    let base = database_url()?;
    connect_options(DATABASE_URL, &base, &args.migrator_login)?;
    let mut url = Url::parse(&base).map_err(|_| {
        OwnerCliError::Configuration(format!("{DATABASE_URL} is not a PostgreSQL connection URL"))
    })?;
    url.query_pairs_mut()
        .append_pair(
            "options",
            &format!(
                "-c role={} -c search_path={}",
                args.schema_owner.as_str(),
                args.schema.0
            ),
        )
        .append_pair("application_name", application_name);
    let mut config = DatabaseConfig::new(DatabaseDriver::Postgres, url.to_string());
    // The phase runs one step at a time. The second slot serves a step
    // that keeps one session open while it works through the pool.
    config.pool_size = 2;
    Database::from_config("runtime owner", &config)
        .await
        .map_err(OwnerCliError::Connect)
}
