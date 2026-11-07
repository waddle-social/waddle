//! Operator-recorded identity for the restricted PostgreSQL startup path.
//!
//! The handoff records the operator's cutover procedure and independently
//! supplied PostgreSQL pins. Its cutover record IDs are audit references, not
//! cryptographic evidence: this binary has no trusted operator signing key or
//! durable anti-replay store, and it cannot continuously enforce old-writer,
//! primary, listener, or privileged-mutator fencing.

use std::str::FromStr;

use serde::Deserialize;
use sqlx::postgres::{PgConnectOptions, PgSslMode};
use thiserror::Error;
use uuid::Uuid;

use crate::config::LineageConfig;

use super::bootstrap::RuntimePoolLimits;
use super::lineage::{
    AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseIdentity, PgDatabaseName, PgIdentity,
    PgSchemaIdentity, PgSchemaName, PgSystemIdentifier,
};
use super::physical_attestation::{
    pinned_attested_pool, DarkPhysicalAttestationPins, DarkPinnedEntryError, PgRoleName,
    PhysicalAttestationConfig, PhysicalAttestationError, PinnedRuntimeAuthority,
};
use super::DarkMigrationLedgerPins;

/// Restricted-startup handoff and preflight errors.
#[derive(Debug, Error)]
pub enum RuntimeStartupError {
    #[error("the operator-supplied database handoff is invalid")]
    InvalidDocument,
    #[error("restricted startup requires WADDLE_GIT_SHA to bind the handoff to this release")]
    ReleaseIdentityMissing,
    #[error("the operator-supplied release identity differs from WADDLE_GIT_SHA")]
    ReleaseMismatch,
    #[error("restricted runtime cannot run a lineage enrollment or adoption action")]
    OperatorActionConfigured,
    #[error("the operator-supplied deployment identity differs from WADDLE_DEPLOYMENT_UUID")]
    DeploymentMismatch,
    #[error("restricted runtime requires a valid PostgreSQL connection URL")]
    InvalidDatabaseUrl,
    #[error("restricted runtime connection does not target the configured database")]
    DatabaseNameMismatch,
    #[error(
        "restricted runtime connection host, port, or socket differs from the operator-recorded primary endpoint"
    )]
    PrimaryEndpointMismatch,
    #[error("restricted runtime TCP connections require PostgreSQL sslmode=verify-full")]
    UnauthenticatedTcp,
    #[error(
        "the operator-recorded old runtime principal must differ from the new runtime principal"
    )]
    OldRuntimePrincipalMismatch,
    #[error("cooperative catalog fencing is not held through this startup preflight; require operator quiescence")]
    CooperativeMutatorPolicyUnavailable,
    #[error("the pinned startup preflight did not complete; reconcile this attempt on the original primary before retrying")]
    PinnedPreflightIndeterminate(#[source] DarkPinnedEntryError),
    #[error("the pinned startup preflight pool close timed out; reconcile the original backend before retrying")]
    PreflightPoolCloseIndeterminate,
    #[error("restricted startup preflight passed, but enabled stores and transaction entry remain unconverted")]
    UnconvertedStores,
    #[error(transparent)]
    PhysicalAttestation(#[from] PhysicalAttestationError),
}

/// Strict, versioned operator procedure handoff. It binds independently
/// recorded database pins and cutoff references to a reported release and
/// endpoint. The cutoff references remain operator records; parsing them does
/// not prove their claims or establish ongoing enforcement.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityDocument {
    version: u8,
    release_git_sha: String,
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
    cutover: OperatorCutoverDocument,
    pins: OperatorPinsDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorCutoverDocument {
    transition_id: String,
    primary: OperatorPrimaryObservationDocument,
    old_runtime: OperatorOldRuntimeDocument,
    listener_cutoff_record_id: String,
    backend_cutoff_record_id: String,
    prepared_transaction_cutoff_record_id: String,
    mutator_policy: OperatorMutatorPolicyDocument,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorPrimaryObservationDocument {
    provider: String,
    cluster_id: String,
    instance_id: String,
    endpoint_host: String,
    endpoint_port: u16,
    endpoint_socket_dir: Option<String>,
    observation_record_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorOldRuntimeDocument {
    login: String,
    role_oid: u32,
    credential_cutoff_record_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OperatorMutatorPolicyDocument {
    mode: OperatorMutatorPolicyMode,
    record_id: String,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OperatorMutatorPolicyMode {
    CooperativeCatalogFenceV1,
    QuiescedForServingInterval,
}

/// Opaque operator-recorded cutover references retained after validation.
#[derive(Clone, Debug)]
struct OperatorCutoverRecord {
    transition_id: Uuid,
    primary_endpoint_host: String,
    primary_endpoint_port: u16,
    primary_endpoint_socket_dir: Option<String>,
    _old_runtime_login: PgRoleName,
    _old_runtime_role_oid: u32,
    _primary_provider: String,
    _primary_cluster_id: String,
    _primary_instance_id: String,
    _primary_observation_record_id: Uuid,
    _credential_cutoff_record_id: Uuid,
    _listener_cutoff_record_id: Uuid,
    _backend_cutoff_record_id: Uuid,
    _prepared_transaction_cutoff_record_id: Uuid,
    _mutator_policy_mode: OperatorMutatorPolicyMode,
    _mutator_policy_record_id: Uuid,
}

/// Validated operator input used only for a pinned, read-only startup preflight.
pub struct RuntimeStartupIdentity {
    expected: AttestedLineage,
    runtime_login: PgRoleName,
    physical: PhysicalAttestationConfig,
    pins: PinnedRuntimeAuthority,
    cutover: OperatorCutoverRecord,
}

/// A sealed result showing that one pinned, same-backend read-only preflight
/// completed. This is deliberately not a database pool or serving capability.
#[must_use = "restricted startup preparation does not authorize serving"]
pub struct RestrictedStartupPreparation {
    transition_id: Uuid,
    _expected: AttestedLineage,
}

impl RestrictedStartupPreparation {
    /// Operator transition identifier associated with this completed preflight.
    pub fn transition_id(&self) -> Uuid {
        self.transition_id
    }
}

impl RuntimeStartupIdentity {
    /// Parse and bind a strict handoff to the release identity provided by the
    /// launcher. The release value is a runtime-reported Git SHA, not a signed
    /// artifact identity.
    pub fn from_json(
        document: &str,
        lineage: &LineageConfig,
        release_git_sha: &str,
    ) -> Result<Self, RuntimeStartupError> {
        if document.len() > 16 * 1024 {
            return Err(RuntimeStartupError::InvalidDocument);
        }
        if !valid_git_sha(release_git_sha) {
            return Err(RuntimeStartupError::ReleaseIdentityMissing);
        }
        let doc: IdentityDocument =
            serde_json::from_str(document).map_err(|_| RuntimeStartupError::InvalidDocument)?;
        if doc.version != 3 || doc.database_oid == 0 || doc.schema_oid == 0 {
            return Err(RuntimeStartupError::InvalidDocument);
        }
        if !valid_git_sha(&doc.release_git_sha) || doc.release_git_sha != release_git_sha {
            return Err(RuntimeStartupError::ReleaseMismatch);
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
            || !valid_postgres_identifier(&doc.database_name)
            || !valid_postgres_identifier(&doc.schema_name)
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
        let installer_login = PgRoleName::new(doc.installer_login)?;
        let runtime_login = PgRoleName::new(doc.runtime_login)?;
        let definer_role = PgRoleName::new(doc.definer_role)?;
        let physical = PhysicalAttestationConfig::new(
            expected.clone(),
            installer_login.clone(),
            runtime_login.clone(),
            definer_role.clone(),
            PgSchemaName(doc.trusted_schema),
        )?;
        let pin_doc = doc.pins;
        let cutover = validate_cutover(
            doc.cutover,
            &runtime_login,
            pin_doc.runtime_role_oid,
            [
                (installer_login.as_str(), pin_doc.installer_role_oid),
                (definer_role.as_str(), pin_doc.definer_role_oid),
            ],
        )?;
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
            &physical,
            attestor,
            ledger,
            pin_doc.epoch_relation_oid,
            pin_doc.epoch_primary_index_oid,
            pin_doc.guard_manifest_relation_oid,
        )?;
        Ok(Self {
            expected,
            runtime_login,
            physical,
            pins,
            cutover,
        })
    }

    /// Run the pinned physical, complete-ledger, and epoch-manifest preflight
    /// on one checked-out pool connection, then await the throwaway pool's
    /// local close. SQLx close completion does not prove that PostgreSQL has
    /// exited the backend; cancellation or transport loss is indeterminate.
    /// No pool escapes or serving authority is returned.
    pub async fn preflight(
        &self,
        database_url: &str,
        limits: RuntimePoolLimits,
    ) -> Result<RestrictedStartupPreparation, RuntimeStartupError> {
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
        let effective_socket_dir = effective_socket_dir(&options);
        if options.get_host() != self.cutover.primary_endpoint_host.as_str()
            || options.get_port() != self.cutover.primary_endpoint_port
            || effective_socket_dir.and_then(std::path::Path::to_str)
                != self.cutover.primary_endpoint_socket_dir.as_deref()
        {
            return Err(RuntimeStartupError::PrimaryEndpointMismatch);
        }
        if options.get_username() != self.runtime_login.as_str() {
            return Err(PhysicalAttestationError::WrongConfiguredLogin.into());
        }
        // Unix socket transport depends on operator-controlled filesystem
        // permissions. TCP must authenticate the named server certificate.
        let unix_socket = effective_socket_dir.is_some();
        if !unix_socket && !matches!(options.get_ssl_mode(), PgSslMode::VerifyFull) {
            return Err(RuntimeStartupError::UnauthenticatedTcp);
        }
        if limits.main_connections == 0 || limits.ingress_connections == 0 {
            return Err(RuntimeStartupError::InvalidDocument);
        }

        // This single-use pool never becomes a DatabasePool. Its hook checks
        // physical identity before lending; the entry proof then rechecks
        // physical identity, the complete pinned ledger, and epoch manifest
        // in one read-only transaction on that checked-out backend.
        let pool = pinned_attested_pool(
            options,
            self.physical.clone(),
            self.pins.clone(),
            1,
            limits.acquire_timeout,
        )?;
        let preflight = pool.inspect_dark_pinned_read_only_entry().await;
        let close = tokio::time::timeout(limits.acquire_timeout, pool.close()).await;
        if close.is_err() {
            return Err(RuntimeStartupError::PreflightPoolCloseIndeterminate);
        }
        // SQLx close returns no server-side exit acknowledgement. The original
        // primary/backend still needs external custody before a later cutover.
        preflight.map_err(RuntimeStartupError::PinnedPreflightIndeterminate)?;

        Ok(RestrictedStartupPreparation {
            transition_id: self.cutover.transition_id,
            _expected: self.expected.clone(),
        })
    }
}

fn validate_cutover(
    cutover: OperatorCutoverDocument,
    runtime_login: &PgRoleName,
    runtime_role_oid: u32,
    other_known_roles: [(&str, u32); 2],
) -> Result<OperatorCutoverRecord, RuntimeStartupError> {
    let transition_id = parse_record_id(&cutover.transition_id)?;
    let observation_record_id = parse_record_id(&cutover.primary.observation_record_id)?;
    let credential_cutoff_record_id =
        parse_record_id(&cutover.old_runtime.credential_cutoff_record_id)?;
    let listener_cutoff_record_id = parse_record_id(&cutover.listener_cutoff_record_id)?;
    let backend_cutoff_record_id = parse_record_id(&cutover.backend_cutoff_record_id)?;
    let prepared_transaction_cutoff_record_id =
        parse_record_id(&cutover.prepared_transaction_cutoff_record_id)?;
    let mutator_policy_record_id = parse_record_id(&cutover.mutator_policy.record_id)?;
    let record_ids = [
        transition_id,
        observation_record_id,
        credential_cutoff_record_id,
        listener_cutoff_record_id,
        backend_cutoff_record_id,
        prepared_transaction_cutoff_record_id,
        mutator_policy_record_id,
    ];
    if record_ids.iter().any(|id| id.is_nil())
        || record_ids
            .iter()
            .enumerate()
            .any(|(index, id)| record_ids[..index].contains(id))
        || cutover.old_runtime.role_oid == 0
    {
        return Err(RuntimeStartupError::InvalidDocument);
    }
    let old_runtime_login = PgRoleName::new(cutover.old_runtime.login)?;
    if runtime_role_oid == 0
        || old_runtime_login.as_str() == runtime_login.as_str()
        || cutover.old_runtime.role_oid == runtime_role_oid
    {
        return Err(RuntimeStartupError::OldRuntimePrincipalMismatch);
    }
    // These names and OIDs are supplied together in the same handoff. Catch
    // internal contradictions without claiming the old role still exists in
    // the live catalog after credential cutoff.
    if other_known_roles.iter().any(|(login, oid)| {
        (old_runtime_login.as_str() == *login) != (cutover.old_runtime.role_oid == *oid)
    }) {
        return Err(RuntimeStartupError::InvalidDocument);
    }
    if matches!(
        cutover.mutator_policy.mode,
        OperatorMutatorPolicyMode::CooperativeCatalogFenceV1
    ) {
        return Err(RuntimeStartupError::CooperativeMutatorPolicyUnavailable);
    }
    if !valid_operator_label(&cutover.primary.provider)
        || !valid_operator_label(&cutover.primary.cluster_id)
        || !valid_operator_label(&cutover.primary.instance_id)
        || !valid_operator_label(&cutover.primary.endpoint_host)
        || cutover.primary.endpoint_port == 0
        || cutover
            .primary
            .endpoint_socket_dir
            .as_deref()
            .is_some_and(|path| !valid_socket_directory(path))
    {
        return Err(RuntimeStartupError::InvalidDocument);
    }
    Ok(OperatorCutoverRecord {
        transition_id,
        primary_endpoint_host: cutover.primary.endpoint_host,
        primary_endpoint_port: cutover.primary.endpoint_port,
        primary_endpoint_socket_dir: cutover.primary.endpoint_socket_dir,
        _old_runtime_login: old_runtime_login,
        _old_runtime_role_oid: cutover.old_runtime.role_oid,
        _primary_provider: cutover.primary.provider,
        _primary_cluster_id: cutover.primary.cluster_id,
        _primary_instance_id: cutover.primary.instance_id,
        _primary_observation_record_id: observation_record_id,
        _credential_cutoff_record_id: credential_cutoff_record_id,
        _listener_cutoff_record_id: listener_cutoff_record_id,
        _backend_cutoff_record_id: backend_cutoff_record_id,
        _prepared_transaction_cutoff_record_id: prepared_transaction_cutoff_record_id,
        _mutator_policy_mode: cutover.mutator_policy.mode,
        _mutator_policy_record_id: mutator_policy_record_id,
    })
}

fn parse_record_id(value: &str) -> Result<Uuid, RuntimeStartupError> {
    Uuid::parse_str(value).map_err(|_| RuntimeStartupError::InvalidDocument)
}

fn valid_git_sha(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_postgres_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 63 && !value.contains('\0')
}

fn valid_operator_label(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && !value.contains('\0')
}

fn valid_socket_directory(value: &str) -> bool {
    valid_operator_label(value) && std::path::Path::new(value).is_absolute()
}

/// SQLx selects a Unix socket when the effective host is an absolute path,
/// even if `get_socket()` is empty (for example, when PGHOST supplies it).
fn effective_socket_dir(options: &PgConnectOptions) -> Option<&std::path::Path> {
    options
        .get_socket()
        .map(std::path::PathBuf::as_path)
        .or_else(|| {
            options
                .get_host()
                .starts_with('/')
                .then(|| std::path::Path::new(options.get_host()))
        })
}

#[cfg(test)]
mod tests {
    use super::effective_socket_dir;
    use sqlx::postgres::PgConnectOptions;

    #[test]
    fn endpoint_records_effective_sqlx_socket_transport() {
        let socket = PgConnectOptions::new_without_pgpass().socket("/private/tmp");
        assert_eq!(
            effective_socket_dir(&socket).and_then(|path| path.to_str()),
            Some("/private/tmp")
        );
        let host_path = PgConnectOptions::new_without_pgpass().host("/private/tmp");
        assert!(host_path.get_socket().is_none());
        assert_eq!(
            effective_socket_dir(&host_path).and_then(|path| path.to_str()),
            Some("/private/tmp")
        );
        let tcp = PgConnectOptions::new_without_pgpass().host("db-primary.example.com");
        assert!(effective_socket_dir(&tcp).is_none());
    }
}

/// The operator records these values outside the runtime connection. They are
/// not derived from or trusted because of the connection being checked.
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
