use thiserror::Error;

use super::{
    DeploymentUuid, PgDatabaseIdentity, PgSchemaIdentity, PgSystemIdentifier,
};

/// Fail-closed lineage attestation errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LineageError {
    #[error("database lineage row is missing")]
    MissingRow,

    #[error("database lineage uses unknown format {found}")]
    UnknownFormat { found: i64 },

    #[error("database lineage table is malformed: {detail}")]
    MalformedTable { detail: String },

    #[error("database lineage {field} is not a UUID: {value}")]
    InvalidUuid { field: &'static str, value: String },

    #[error("database lineage {field} is not a valid PostgreSQL identity value: {value}")]
    InvalidPostgresIdentity { field: &'static str, value: String },

    #[error("configured deployment UUID {configured} does not match persisted deployment UUID {persisted}")]
    DeploymentUuidMismatch {
        configured: DeploymentUuid,
        persisted: DeploymentUuid,
    },

    #[error("database lineage is bound to deployment UUID {persisted}, but WADDLE_DEPLOYMENT_UUID is unset")]
    DeploymentUuidUnconfigured { persisted: DeploymentUuid },

    #[error("WADDLE_DEPLOYMENT_UUID must be configured for lineage enrollment or adoption")]
    DeploymentUuidRequired,

    #[error(
        "live PostgreSQL system identifier {live} does not match persisted identifier {persisted}"
    )]
    SystemIdentifierMismatch {
        live: PgSystemIdentifier,
        persisted: PgSystemIdentifier,
    },

    #[error(
        "live PostgreSQL database identity {}:{} does not match persisted {}:{}",
        live.oid, live.name, persisted.oid, persisted.name
    )]
    DatabaseIdentityMismatch {
        live: PgDatabaseIdentity,
        persisted: PgDatabaseIdentity,
    },

    #[error(
        "live PostgreSQL schema identity {}:{} does not match persisted {}:{}",
        live.oid, live.name, persisted.oid, persisted.name
    )]
    SchemaIdentityMismatch {
        live: PgSchemaIdentity,
        persisted: PgSchemaIdentity,
    },

    #[error(
        "PostgreSQL pg_control_system() is unavailable; grant EXECUTE on it to the database role"
    )]
    SystemIdentifierUnavailable,
}
