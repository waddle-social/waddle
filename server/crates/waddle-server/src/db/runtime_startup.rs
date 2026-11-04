//! Operator-supplied identity for the restricted PostgreSQL startup path.

use std::str::FromStr;

use serde::Deserialize;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use thiserror::Error;

use crate::config::LineageConfig;

use super::bootstrap::{
    attach_verified_global_runtime, attach_verified_global_runtime_with_control_plane,
    ControlPlanePoolLimits, RuntimeAttachmentError, RuntimePoolLimits,
};
use super::lineage::{
    AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseIdentity, PgDatabaseName, PgIdentity,
    PgSchemaIdentity, PgSchemaName, PgSystemIdentifier,
};
use super::physical_attestation::{
    PgRoleName, PhysicalAttestationConfig, PhysicalAttestationError,
};
use super::DatabasePool;

/// Operator identity handoff and restricted runtime startup errors.
#[derive(Debug, Error)]
pub enum RuntimeStartupError {
    #[error("the operator-supplied database identity document is invalid")]
    InvalidDocument,
    #[error("restricted runtime requires a direct, operator-fenced primary assertion")]
    DirectPrimaryAssertionRequired,
    #[error("restricted runtime cannot run a lineage enrollment or adoption action")]
    OperatorActionConfigured,
    #[error("the operator-supplied deployment identity differs from WADDLE_DEPLOYMENT_UUID")]
    DeploymentMismatch,
    #[error("restricted runtime requires a valid PostgreSQL connection URL")]
    InvalidDatabaseUrl,
    #[error("restricted runtime connection does not target the configured database")]
    DatabaseNameMismatch,
    #[error("restricted runtime TCP connections require PostgreSQL sslmode=verify-full")]
    UnauthenticatedTcp,
    #[error(transparent)]
    PhysicalAttestation(#[from] PhysicalAttestationError),
    #[error(transparent)]
    Attachment(#[from] RuntimeAttachmentError),
}

/// Versioned operator handoff, kept separate from the connection URL and
/// every identity reported by the database. The assertion is an operational
/// prerequisite, not proof that a primary or prior writers were fenced.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityDocument {
    version: u8,
    operator_asserts_direct_fenced_primary: bool,
    deployment_uuid: String,
    lineage_uuid: String,
    system_identifier: String,
    database_oid: u32,
    database_name: String,
    schema_oid: u32,
    schema_name: String,
    installer_login: String,
    runtime_login: String,
    definer_role: String,
    trusted_schema: String,
}

/// Validated operator input used for an attested runtime attachment.
pub struct RuntimeStartupIdentity {
    expected: AttestedLineage,
    attestation: PhysicalAttestationConfig,
}

impl RuntimeStartupIdentity {
    pub fn from_json(document: &str, lineage: &LineageConfig) -> Result<Self, RuntimeStartupError> {
        let doc: IdentityDocument =
            serde_json::from_str(document).map_err(|_| RuntimeStartupError::InvalidDocument)?;
        if doc.version != 1 || doc.database_oid == 0 || doc.schema_oid == 0 {
            return Err(RuntimeStartupError::InvalidDocument);
        }
        if !doc.operator_asserts_direct_fenced_primary {
            return Err(RuntimeStartupError::DirectPrimaryAssertionRequired);
        }
        if lineage.action.is_some() {
            return Err(RuntimeStartupError::OperatorActionConfigured);
        }
        let deployment_uuid: DeploymentUuid = doc
            .deployment_uuid
            .parse()
            .map_err(|_| RuntimeStartupError::InvalidDocument)?;
        let lineage_uuid: LineageUuid = doc
            .lineage_uuid
            .parse()
            .map_err(|_| RuntimeStartupError::InvalidDocument)?;
        let system_identifier: PgSystemIdentifier = doc
            .system_identifier
            .parse()
            .map_err(|_| RuntimeStartupError::InvalidDocument)?;
        if deployment_uuid.0.is_nil()
            || lineage_uuid.0.is_nil()
            || system_identifier.0 == 0
            || doc.database_name.is_empty()
            || doc.database_name.len() > 63
            || doc.database_name.contains('\0')
        {
            return Err(RuntimeStartupError::InvalidDocument);
        }
        if lineage.deployment_uuid != Some(deployment_uuid) {
            return Err(RuntimeStartupError::DeploymentMismatch);
        }
        let expected = AttestedLineage {
            deployment_uuid,
            lineage_uuid,
            postgres_identity: Some(PgIdentity {
                system_identifier,
                database: PgDatabaseIdentity {
                    oid: doc.database_oid,
                    name: PgDatabaseName(doc.database_name),
                },
                schema: PgSchemaIdentity {
                    oid: doc.schema_oid,
                    name: PgSchemaName(doc.schema_name),
                },
            }),
        };
        let attestation = PhysicalAttestationConfig::new(
            expected.clone(),
            PgRoleName::new(doc.installer_login)?,
            PgRoleName::new(doc.runtime_login)?,
            PgRoleName::new(doc.definer_role)?,
            PgSchemaName(doc.trusted_schema),
        )?;
        Ok(Self {
            expected,
            attestation,
        })
    }

    /// Attach using the exact hook-bearing pool. `database_url` is never
    /// retained as a reconnect URL by the returned `DatabasePool`.
    pub async fn attach(
        &self,
        database_url: &str,
        limits: RuntimePoolLimits,
        control_plane_limits: Option<ControlPlanePoolLimits>,
    ) -> Result<DatabasePool, RuntimeStartupError> {
        let options = PgConnectOptions::from_str(database_url)
            .map_err(|_| RuntimeStartupError::InvalidDatabaseUrl)?;
        let expected_database = &self
            .expected
            .postgres_identity
            .as_ref()
            .ok_or(RuntimeStartupError::InvalidDocument)?
            .database
            .name
            .0;
        if options.get_database() != Some(expected_database.as_str()) {
            return Err(RuntimeStartupError::DatabaseNameMismatch);
        }
        // Unix socket transport depends on operator-controlled filesystem
        // permissions. TCP must authenticate the named server certificate.
        let unix_socket = options.get_socket().is_some() || options.get_host().starts_with('/');
        if !unix_socket && !matches!(options.get_ssl_mode(), PgSslMode::VerifyFull) {
            return Err(RuntimeStartupError::UnauthenticatedTcp);
        }
        let attachment = match control_plane_limits {
            Some(control_plane_limits) => {
                attach_verified_global_runtime_with_control_plane(
                    options.clone(),
                    options,
                    self.attestation.clone(),
                    &self.expected,
                    limits,
                    control_plane_limits,
                )
                .await?
            }
            None => {
                attach_verified_global_runtime(
                    options,
                    self.attestation.clone(),
                    &self.expected,
                    limits,
                )
                .await?
            }
        };
        Ok(attachment.into_pool())
    }
}
