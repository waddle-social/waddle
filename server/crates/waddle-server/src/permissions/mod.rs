//! SpiceDB-backed permission system for Waddle.

use crate::db::DatabaseError;
pub mod actor;
mod authz_id;
mod authz_incarnation;
mod authz_schema_version;
mod check;
pub mod host_grant;
mod schema;
mod spicedb;
pub mod spicedb_tenant;
mod tuple;

pub use actor::{
    CheckPermission, ClearHostGrantWitnesses, DeleteTuple, EnsureAuthzCodecIncarnation,
    EnsureSchema, ExclusiveRelationSwap, ListHostGrantWitnesses, ListRelations, ListSubjects,
    LookupResources, LookupSubjects, ObserveHostGrantProjection, Permission, PermissionActor,
    ReadHostGrantWitness, RecordAuthzSchemaVersion, ReplaceExclusiveRelation,
    ReplaceHostGrantWitnesses, SwapExclusiveRelation, TakeOverHostGrantProjection,
    WriteHostGrantProjection, WriteTuple,
};
pub use authz_id::{
    decode_object, decode_subject, decode_tenant_scoped_id, encode_tenant_scoped_id, AuthzIdError,
    AuthzOpaqueId, AuthzUserJid, EncodedObjectId, EncodedSubjectId, AUTHZ_ID_WIRE_PREFIX,
    MAX_AUTHZ_ID_BYTES, MAX_AUTHZ_WIRE_BYTES,
};
pub use authz_incarnation::{
    classify_codec_markers, guarded_resource_definitions, AuthzIncarnationError, CodecMarkerRow,
    CodecMarkerState, AUTHZ_CODEC_MARKER_DEFINITION, AUTHZ_CODEC_MARKER_ID,
    AUTHZ_CODEC_MARKER_READ_LIMIT, AUTHZ_CODEC_MARKER_RELATION, AUTHZ_CODEC_VERSION,
};
pub use authz_schema_version::{
    authz_schema_retaining_version, authz_schema_version_relation, builtin_authz_schema,
    classify_schema_markers, declared_authz_schema_versions, parse_authz_schema_version_relation,
    unfinished_upgrade_refusal, AuthzSchemaVersionError, SchemaMarkerRow, SchemaMarkerState,
    AUTHZ_SCHEMA_MARKER_DEFINITION, AUTHZ_SCHEMA_MARKER_ID, AUTHZ_SCHEMA_MARKER_READ_LIMIT,
    AUTHZ_SCHEMA_VERSION, AUTHZ_SCHEMA_VERSION_RELATION_PREFIX,
};
pub use check::{CheckRequest, CheckResponse, PermissionChecker};
pub use schema::{ComputedPermission, ObjectTypeSchema, PermissionSchema, SPICEDB_SCHEMA};
pub use spicedb_tenant::TenantSpiceDbPermissionBackend;
pub use tuple::{Object, ObjectType, Relation, Subject, SubjectType, Tuple, TupleStore};
pub use waddle_broker::authz::local::LocalPermissionBackend;
pub use waddle_broker::authz::openfga::{
    parse_openfga_consistency, InProcessOpenFgaServer, OpenFgaCheckWireRequest, OpenFgaConfig,
    OpenFgaConsistency, OpenFgaPermissionBackend, OpenFgaTupleKey, RecordedOpenFgaRequest,
};
pub use waddle_broker::authz::openfga_model::{
    openfga_authorization_model_json, openfga_module_grant_object, openfga_store_grant_object,
    OPENFGA_CHAT_MODULE_DSL, OPENFGA_INFERENCE_MODULE_DSL, OPENFGA_SCHEMA_VERSION,
};
pub use waddle_broker::authz::AuthzCallerRole;
pub use waddle_broker_core::permission::PermissionBackend;

/// The SpiceDB backend's own identifier conversions, exposed to
/// `test_support` so integration tests observe the exact wire values.
#[cfg(feature = "test-support")]
pub(crate) use spicedb::{
    exact_relationship_filter, filter_for_object_and_relation, filter_for_object_and_subject,
    object_from_resource_id, object_reference, relationship_from_tuple, subject_from_lookup_id,
    subject_from_reference, subject_reference,
};

use thiserror::Error;

/// Permission-specific errors.
#[derive(Error, Debug)]
pub enum PermissionError {
    #[error("Permission denied: {0}")]
    Denied(String),

    #[error("Invalid tuple format: {0}")]
    InvalidTuple(String),

    #[error("Invalid object: {0}")]
    InvalidObject(String),

    #[error("Invalid subject: {0}")]
    InvalidSubject(String),

    #[error("Invalid authorization identifier: {0}")]
    InvalidAuthzId(#[from] AuthzIdError),

    #[error("Authorization store refused: {0}")]
    AuthzIncarnation(#[from] AuthzIncarnationError),

    #[error("Authorization store refused: {0}")]
    AuthzSchemaVersion(#[from] AuthzSchemaVersionError),

    #[error("Invalid relation: {0}")]
    InvalidRelation(String),

    #[error("Tuple not found")]
    TupleNotFound,

    #[error("Tuple already exists")]
    TupleAlreadyExists,

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Check depth exceeded maximum of {0}")]
    MaxDepthExceeded(usize),

    #[error("SpiceDB error: {0}")]
    SpiceDbError(String),

    #[error("Conditional permission requires additional context fields: {0:?}")]
    ConditionalPermission(Vec<String>),

    #[error(
        "SpiceDB configuration is required; set WADDLE_SPICEDB_ENDPOINT and WADDLE_SPICEDB_PRESHARED_KEY"
    )]
    SpiceDbConfigMissing,

    #[error("Unsupported operation for '{0}' permission backend")]
    UnsupportedOperation(&'static str),

    #[error("Host-grant projection refused: {0}")]
    HostGrantProjection(#[from] host_grant::ProjectionRefusal),

    /// The object belongs to a scope whose grants the host-grant authority
    /// owns. Only its canonical operations change them.
    #[error(
        "the object belongs to an enrolled host-grant scope; legacy permission writes are refused"
    )]
    HostGrantScopeEnrolled,

    /// Whether the object belongs to an enrolled scope could not be read, so
    /// the legacy write is refused rather than risk writing into one.
    #[error("the host-grant authority could not be read; the permission write is refused")]
    HostGrantAuthorityUnavailable,

    /// A privileged or room-moderation relation was to be written for an
    /// account whose home is neither a domain this deployment serves nor a
    /// trusted home (ADR-0020 D8).
    #[error(
        "a privileged or moderator relation for a remote account requires its home to be a \
         trusted home"
    )]
    UntrustedPrivilegedSubject,
}

impl From<DatabaseError> for PermissionError {
    fn from(err: DatabaseError) -> Self {
        PermissionError::DatabaseError(err.to_string())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod parent_move_native_tests;
