//! Operator-supplied identity for the restricted PostgreSQL startup path.

use std::str::FromStr;

use serde::Deserialize;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use thiserror::Error;
use uuid::Uuid;

use crate::config::LineageConfig;

use super::bootstrap::{ControlPlanePoolLimits, RuntimeAttachmentError, RuntimePoolLimits};
use super::lineage::{
    AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseIdentity, PgDatabaseName, PgIdentity,
    PgSchemaIdentity, PgSchemaName, PgSystemIdentifier,
};
use super::physical_attestation::{
    DarkPhysicalAttestationPins, PgRoleName, PhysicalAttestationConfig, PhysicalAttestationError,
    PinnedRuntimeAuthority,
};
use super::{DarkMigrationLedgerPins, DatabasePool};

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
    #[error("restricted runtime bootstrap still has unconverted ledger and transaction entry")]
    UnconvertedPinnedBootstrap,
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
    pins: OperatorPinsDocument,
}

/// The operator records these values outside the runtime connection. The
/// installer function OID is added only after positive installation or a
/// same-operation reconciliation on the fenced primary.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorPinsDocument {
    database_owner_role_oid: u32,
    schema_owner_role_oid: u32,
    runtime_role_oid: u32,
    installer_role_oid: u32,
    definer_role_oid: u32,
    trusted_schema_oid: u32,
    lineage_relation_oid: u32,
    lineage_primary_index_oid: u32,
    attestor_function_oid: u32,
    installation_operation_id: String,
    ledger_relation_oid: u32,
    ledger_primary_index_oid: u32,
    epoch_relation_oid: u32,
    epoch_primary_index_oid: u32,
    guard_manifest_relation_oid: u32,
}

/// Validated operator input used for an attested runtime attachment.
pub struct RuntimeStartupIdentity {
    expected: AttestedLineage,
    _pins: PinnedRuntimeAuthority,
}

impl RuntimeStartupIdentity {
    pub fn from_json(document: &str, lineage: &LineageConfig) -> Result<Self, RuntimeStartupError> {
        if document.len() > 8 * 1024 {
            return Err(RuntimeStartupError::InvalidDocument);
        }
        let doc: IdentityDocument =
            serde_json::from_str(document).map_err(|_| RuntimeStartupError::InvalidDocument)?;
        if doc.version != 2 || doc.database_oid == 0 || doc.schema_oid == 0 {
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
        let pin_doc = doc.pins;
        let installation_operation_id = Uuid::parse_str(&pin_doc.installation_operation_id)
            .map_err(|_| RuntimeStartupError::InvalidDocument)?;
        let attestor = DarkPhysicalAttestationPins {
            database_owner_role_oid: pin_doc.database_owner_role_oid,
            schema_owner_role_oid: pin_doc.schema_owner_role_oid,
            runtime_role_oid: pin_doc.runtime_role_oid,
            installer_role_oid: pin_doc.installer_role_oid,
            definer_role_oid: pin_doc.definer_role_oid,
            trusted_schema_oid: pin_doc.trusted_schema_oid,
            lineage_relation_oid: pin_doc.lineage_relation_oid,
            lineage_primary_index_oid: pin_doc.lineage_primary_index_oid,
            attestor_function_oid: pin_doc.attestor_function_oid,
            installation_operation_id,
        };
        let ledger = DarkMigrationLedgerPins {
            schema_oid: doc.schema_oid,
            schema_owner_role_oid: pin_doc.schema_owner_role_oid,
            runtime_role_oid: pin_doc.runtime_role_oid,
            ledger_relation_oid: pin_doc.ledger_relation_oid,
            ledger_primary_index_oid: pin_doc.ledger_primary_index_oid,
        };
        let pins = PinnedRuntimeAuthority::new(
            &attestation,
            attestor,
            ledger,
            pin_doc.epoch_relation_oid,
            pin_doc.epoch_primary_index_oid,
            pin_doc.guard_manifest_relation_oid,
        )?;
        Ok(Self {
            expected,
            _pins: pins,
        })
    }

    /// Refuse attachment until the pinned ledger and transaction-entry proof
    /// is wired into the bootstrap path. The legacy attachment enters a
    /// name-based REPEATABLE READ ledger check before this proof, so opening
    /// even a dark pool here would cross an unsafe catalog boundary.
    pub async fn attach(
        &self,
        database_url: &str,
        _limits: RuntimePoolLimits,
        _control_plane_limits: Option<ControlPlanePoolLimits>,
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
        Err(RuntimeStartupError::UnconvertedPinnedBootstrap)
    }
}
