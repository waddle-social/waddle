//! Composition of the restricted-startup handoff on the owner connection.
//!
//! The handoff has two sources. The database pins (lineage, physical
//! identity, and the OIDs of the roles, schemas, relations, indexes and the
//! attestor function) are read from the catalog through the schema owner,
//! never through the runtime login the handoff will later be checked
//! against. The operator inputs (the release Git SHA, the role and schema
//! names, the attestor installation's operation ID and the cutover record)
//! are supplied by the caller and are not derived from the database.
//!
//! Only the release Git SHA changes between releases of one prepared
//! database, so a deployment composes the handoff for a new image by
//! replacing that one field; the pins stay valid until a pinned object is
//! recreated.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::config::LineageConfig;
use crate::db::lineage::{
    self, AttestedLineage, DeploymentUuid, LineageUuid, PgDatabaseIdentity, PgDatabaseName,
    PgIdentity, PgSchemaIdentity, PgSchemaName, PgSystemIdentifier,
};
use crate::db::physical_attestation::{
    installation_marker, PgRoleName, PhysicalAttestationConfig, PhysicalAttestationError,
};
use crate::db::{Database, DatabaseError};

use super::{
    IdentityDocument, OperatorCutoverDocument, OperatorPinsDocument, RuntimeStartupError,
    RuntimeStartupIdentity,
};

/// A handoff could not be composed or does not match the database.
#[derive(Debug, Error)]
pub enum HandoffError {
    #[error("the database identity document is not a valid version 1 identity")]
    InvalidIdentityDocument,
    #[error("the owner database has no PostgreSQL physical identity")]
    MissingPostgresIdentity,
    #[error("the cutover record is not a valid cutover object: {reason}")]
    InvalidCutoverRecord { reason: String },
    #[error("the handoff to verify is not a version 3 handoff document: {reason}")]
    InvalidDocument { reason: String },
    #[error("could not attest the owner database lineage")]
    Lineage(#[source] DatabaseError),
    #[error("could not read the startup pins from the catalog")]
    Catalog(#[source] DatabaseError),
    #[error(
        "the handoff is composed on the owner connection; this session's login {login} is the \
         runtime login the handoff will be checked against"
    )]
    RuntimeConnection { login: String },
    #[error("{object} does not exist in this database; the owner phase and the attestor installation come first")]
    MissingObject { object: &'static str },
    #[error(
        "the attestor function in schema {trusted_schema} was not installed by operation \
         {operation_id} for this database identity and these roles"
    )]
    AttestorInstallationMismatch {
        trusted_schema: String,
        operation_id: Uuid,
    },
    #[error("the restricted server would refuse the composed handoff at startup")]
    Refused(#[source] RuntimeStartupError),
    #[error(
        "the handoff differs from what this database and these inputs produce at: {}",
        fields.join(", ")
    )]
    Drift { fields: Vec<String> },
    #[error(transparent)]
    Attestation(#[from] PhysicalAttestationError),
}

/// The lineage and physical identity an owner phase bound its work to, as
/// the attestor installation takes it: an input recorded on the owner
/// connection, not read back through the installer login.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DatabaseIdentityDocument {
    version: u8,
    deployment_uuid: String,
    lineage_uuid: String,
    system_identifier: String,
    database_oid: u32,
    database_name: String,
    schema_oid: u32,
    schema_name: String,
}

/// Render the identity an owner phase attested as a version 1 document.
pub fn database_identity_json(lineage: &AttestedLineage) -> Result<String, HandoffError> {
    let postgres = lineage
        .postgres_identity
        .as_ref()
        .ok_or(HandoffError::MissingPostgresIdentity)?;
    serde_json::to_string_pretty(&DatabaseIdentityDocument {
        version: 1,
        deployment_uuid: lineage.deployment_uuid.to_string(),
        lineage_uuid: lineage.lineage_uuid.to_string(),
        system_identifier: postgres.system_identifier.to_string(),
        database_oid: postgres.database.oid,
        database_name: postgres.database.name.0.clone(),
        schema_oid: postgres.schema.oid,
        schema_name: postgres.schema.name.0.clone(),
    })
    .map_err(|_| HandoffError::InvalidIdentityDocument)
}

/// Parse a document written by [`database_identity_json`].
pub fn database_identity_from_json(document: &str) -> Result<AttestedLineage, HandoffError> {
    let invalid = |_| HandoffError::InvalidIdentityDocument;
    let doc: DatabaseIdentityDocument =
        serde_json::from_str(document).map_err(|_| HandoffError::InvalidIdentityDocument)?;
    let deployment_uuid: DeploymentUuid = doc.deployment_uuid.parse().map_err(invalid)?;
    let lineage_uuid: LineageUuid = doc.lineage_uuid.parse().map_err(invalid)?;
    let system_identifier: PgSystemIdentifier = doc.system_identifier.parse().map_err(invalid)?;
    if doc.version != 1
        || deployment_uuid.0.is_nil()
        || lineage_uuid.0.is_nil()
        || system_identifier.0 == 0
        || doc.database_oid == 0
        || doc.schema_oid == 0
        || doc.database_name.is_empty()
        || doc.schema_name.is_empty()
    {
        return Err(HandoffError::InvalidIdentityDocument);
    }
    Ok(AttestedLineage {
        lineage_uuid,
        deployment_uuid,
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
    })
}

/// Operator inputs that are the same for every handoff of one deployment
/// and release. None of them is read from the database.
#[derive(Clone, Copy, Debug)]
pub struct HandoffInputs<'a> {
    /// The exact `WADDLE_GIT_SHA` the server image is started with.
    pub release_git_sha: &'a str,
    pub deployment_uuid: DeploymentUuid,
    pub installer_login: &'a PgRoleName,
    pub runtime_login: &'a PgRoleName,
    pub definer_role: &'a PgRoleName,
    pub trusted_schema: &'a PgSchemaName,
}

/// The operator's record of one cutover and attestor installation.
#[derive(Clone, Copy, Debug)]
pub struct OperatorRecord<'a> {
    /// Operation ID the attestor was installed under.
    pub installation_operation_id: Uuid,
    /// The handoff's `cutover` object as JSON: the transition, the primary
    /// endpoint the runtime connects to, the retired login and the cutoff
    /// record references.
    pub cutover: &'a str,
}

/// A handoff the restricted server accepts for the release it names.
pub struct ComposedHandoff {
    document: String,
    identity: RuntimeStartupIdentity,
}

impl ComposedHandoff {
    /// The `WADDLE_DB_RUNTIME_ATTESTATION` value.
    pub fn document(&self) -> &str {
        &self.document
    }

    /// The parsed identity, for running the pinned preflight against a
    /// runtime connection before the document is handed over.
    pub fn identity(&self) -> &RuntimeStartupIdentity {
        &self.identity
    }
}

/// Compose the handoff for `inputs` and `record` from the pins `owner`
/// reads, and refuse a document the restricted server would not parse for
/// the same release and deployment.
///
/// `owner` is the owner-phase handle: the migrator login acting as the
/// schema owner, with the application schema first on its `search_path`.
pub async fn compose_handoff(
    owner: &Database,
    inputs: &HandoffInputs<'_>,
    record: &OperatorRecord<'_>,
) -> Result<ComposedHandoff, HandoffError> {
    let cutover = parse_cutover(record.cutover)?;
    let document = compose(owner, inputs, record.installation_operation_id, cutover).await?;
    seal(document, inputs)
}

/// Check that `existing` is the handoff [`compose_handoff`] produces now for
/// `inputs`, taking the cutover record and the installation operation ID
/// from `existing` itself. Every differing field is named.
pub async fn verify_handoff(
    owner: &Database,
    inputs: &HandoffInputs<'_>,
    existing: &str,
) -> Result<ComposedHandoff, HandoffError> {
    let existing: IdentityDocument =
        serde_json::from_str(existing).map_err(|error| HandoffError::InvalidDocument {
            reason: error.to_string(),
        })?;
    let operation_id = Uuid::parse_str(&existing.pins.installation_operation_id).map_err(|_| {
        HandoffError::InvalidDocument {
            reason: "pins.installation_operation_id is not a UUID".to_owned(),
        }
    })?;
    let current = compose(owner, inputs, operation_id, existing.cutover.clone()).await?;
    let mut fields = Vec::new();
    diff(
        &to_value(&existing)?,
        &to_value(&current)?,
        String::new(),
        &mut fields,
    );
    if !fields.is_empty() {
        return Err(HandoffError::Drift { fields });
    }
    seal(current, inputs)
}

fn parse_cutover(cutover: &str) -> Result<OperatorCutoverDocument, HandoffError> {
    serde_json::from_str(cutover).map_err(|error| HandoffError::InvalidCutoverRecord {
        reason: error.to_string(),
    })
}

fn to_value(document: &IdentityDocument) -> Result<serde_json::Value, HandoffError> {
    serde_json::to_value(document).map_err(|error| HandoffError::InvalidDocument {
        reason: error.to_string(),
    })
}

/// Collect the dotted path of every leaf at which the two documents differ.
fn diff(left: &serde_json::Value, right: &serde_json::Value, path: String, out: &mut Vec<String>) {
    match (left, right) {
        (serde_json::Value::Object(left), serde_json::Value::Object(right)) => {
            for (key, value) in left {
                let path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                match right.get(key) {
                    Some(other) => diff(value, other, path, out),
                    None => out.push(path),
                }
            }
        }
        (left, right) if left == right => {}
        _ => out.push(path),
    }
}

/// Serialize and round-trip the document through the parser the restricted
/// server uses, bound to the same release and deployment.
fn seal(
    document: IdentityDocument,
    inputs: &HandoffInputs<'_>,
) -> Result<ComposedHandoff, HandoffError> {
    let document =
        serde_json::to_string_pretty(&document).map_err(|error| HandoffError::InvalidDocument {
            reason: error.to_string(),
        })?;
    let identity = RuntimeStartupIdentity::from_json(
        &document,
        &LineageConfig {
            deployment_uuid: Some(inputs.deployment_uuid),
            action: None,
        },
        inputs.release_git_sha,
    )
    .map_err(HandoffError::Refused)?;
    Ok(ComposedHandoff { document, identity })
}

const PINNED_OBJECTS: [&str; 14] = [
    "the database's owner role",
    "the application schema's owner role",
    "the runtime login",
    "the attestor installer login",
    "the attestor definer role",
    "the attestor schema",
    "the _lineage table",
    "the _lineage primary key",
    "the attestor function",
    "the _migrations table",
    "the _migrations primary key",
    "the ingress_protocol_epoch table",
    "the ingress_protocol_epoch primary key",
    "the ingress_epoch_guard_manifest table",
];

async fn compose(
    owner: &Database,
    inputs: &HandoffInputs<'_>,
    operation_id: Uuid,
    cutover: OperatorCutoverDocument,
) -> Result<IdentityDocument, HandoffError> {
    let lineage = lineage::verify(
        owner,
        &LineageConfig {
            deployment_uuid: Some(inputs.deployment_uuid),
            action: None,
        },
    )
    .await
    .map_err(HandoffError::Lineage)?;
    let postgres = lineage
        .postgres_identity
        .clone()
        .ok_or(HandoffError::MissingPostgresIdentity)?;
    let physical = PhysicalAttestationConfig::new(
        lineage.clone(),
        inputs.installer_login.clone(),
        inputs.runtime_login.clone(),
        inputs.definer_role.clone(),
        inputs.trusted_schema.clone(),
    )?;

    let read = async {
        let connection = owner.guard().await?;
        // Each pin is a scalar subquery, so a missing object yields NULL in
        // its own column and can be named.
        let mut rows = connection
            .query(
                "WITH app AS (SELECT oid, nspowner FROM pg_catalog.pg_namespace WHERE nspname = ?), \
                      trusted AS (SELECT oid FROM pg_catalog.pg_namespace WHERE nspname = ?), \
                      relation AS (SELECT c.oid, c.relname FROM pg_catalog.pg_class c, app \
                                    WHERE c.relnamespace = app.oid AND c.relkind = 'r') \
                 SELECT (SELECT datdba::bigint FROM pg_catalog.pg_database \
                          WHERE datname = pg_catalog.current_database()), \
                        (SELECT nspowner::bigint FROM app), \
                        (SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = ?), \
                        (SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = ?), \
                        (SELECT oid::bigint FROM pg_catalog.pg_roles WHERE rolname = ?), \
                        (SELECT oid::bigint FROM trusted), \
                        (SELECT oid::bigint FROM relation WHERE relname = '_lineage'), \
                        (SELECT i.indexrelid::bigint FROM pg_catalog.pg_index i, relation r \
                          WHERE i.indrelid = r.oid AND i.indisprimary AND r.relname = '_lineage'), \
                        (SELECT p.oid::bigint FROM pg_catalog.pg_proc p, trusted \
                          WHERE p.pronamespace = trusted.oid \
                            AND p.proname = 'read_physical_identity'), \
                        (SELECT oid::bigint FROM relation WHERE relname = '_migrations'), \
                        (SELECT i.indexrelid::bigint FROM pg_catalog.pg_index i, relation r \
                          WHERE i.indrelid = r.oid AND i.indisprimary \
                            AND r.relname = '_migrations'), \
                        (SELECT oid::bigint FROM relation WHERE relname = 'ingress_protocol_epoch'), \
                        (SELECT i.indexrelid::bigint FROM pg_catalog.pg_index i, relation r \
                          WHERE i.indrelid = r.oid AND i.indisprimary \
                            AND r.relname = 'ingress_protocol_epoch'), \
                        (SELECT oid::bigint FROM relation \
                          WHERE relname = 'ingress_epoch_guard_manifest'), \
                        (SELECT pg_catalog.obj_description(p.oid, 'pg_proc') \
                           FROM pg_catalog.pg_proc p, trusted \
                          WHERE p.pronamespace = trusted.oid \
                            AND p.proname = 'read_physical_identity'), \
                        session_user::text",
                crate::db_params![
                    postgres.schema.name.0.as_str(),
                    inputs.trusted_schema.0.as_str(),
                    inputs.runtime_login.as_str(),
                    inputs.installer_login.as_str(),
                    inputs.definer_role.as_str()
                ],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok::<_, DatabaseError>(None);
        };
        let mut oids = Vec::with_capacity(PINNED_OBJECTS.len());
        for index in 0..PINNED_OBJECTS.len() {
            oids.push(row.get::<Option<i64>>(index)?);
        }
        Ok(Some((
            oids,
            row.get::<Option<String>>(PINNED_OBJECTS.len())?,
            row.get::<String>(PINNED_OBJECTS.len() + 1)?,
        )))
    };
    let (oids, installed_marker, session_login) = read
        .await
        .map_err(HandoffError::Catalog)?
        .ok_or(HandoffError::MissingObject {
            object: PINNED_OBJECTS[0],
        })?;
    if session_login == inputs.runtime_login.as_str() {
        return Err(HandoffError::RuntimeConnection {
            login: session_login,
        });
    }
    let mut pins = [0_u32; PINNED_OBJECTS.len()];
    for (index, oid) in oids.into_iter().enumerate() {
        pins[index] = oid
            .and_then(|oid| u32::try_from(oid).ok())
            .filter(|oid| *oid != 0)
            .ok_or(HandoffError::MissingObject {
                object: PINNED_OBJECTS[index],
            })?;
    }
    if installed_marker.as_deref() != Some(installation_marker(&physical, operation_id).as_str()) {
        return Err(HandoffError::AttestorInstallationMismatch {
            trusted_schema: inputs.trusted_schema.0.clone(),
            operation_id,
        });
    }

    Ok(IdentityDocument {
        version: 3,
        release_git_sha: inputs.release_git_sha.to_owned(),
        deployment_uuid: lineage.deployment_uuid.to_string(),
        lineage_uuid: lineage.lineage_uuid.to_string(),
        system_identifier: postgres.system_identifier.to_string(),
        database_oid: postgres.database.oid,
        database_name: postgres.database.name.0,
        schema_oid: postgres.schema.oid,
        schema_name: postgres.schema.name.0,
        installer_login: inputs.installer_login.as_str().to_owned(),
        runtime_login: inputs.runtime_login.as_str().to_owned(),
        definer_role: inputs.definer_role.as_str().to_owned(),
        trusted_schema: inputs.trusted_schema.0.clone(),
        cutover,
        pins: OperatorPinsDocument {
            database_owner_role_oid: pins[0],
            schema_owner_role_oid: pins[1],
            runtime_role_oid: pins[2],
            installer_role_oid: pins[3],
            definer_role_oid: pins[4],
            trusted_schema_oid: pins[5],
            lineage_relation_oid: pins[6],
            lineage_primary_index_oid: pins[7],
            attestor_function_oid: pins[8],
            installation_operation_id: operation_id.to_string(),
            ledger_relation_oid: pins[9],
            ledger_primary_index_oid: pins[10],
            epoch_relation_oid: pins[11],
            epoch_primary_index_oid: pins[12],
            guard_manifest_relation_oid: pins[13],
        },
    })
}
