//! SpiceDB-backed permission system for Waddle.

use crate::db::DatabaseError;
pub mod actor;
mod authz_id;
mod check;
mod schema;
mod spicedb;
mod tuple;

pub use actor::{
    CheckPermission, DeleteTuple, EnsureSchema, ExclusiveRelationSwap, ListRelations, ListSubjects,
    LookupResources, LookupSubjects, Permission, PermissionActor, ReplaceExclusiveRelation,
    SwapExclusiveRelation, WriteTuple,
};
pub use authz_id::{
    decode_object, decode_subject, AuthzIdError, AuthzOpaqueId, AuthzUserJid, EncodedObjectId,
    EncodedSubjectId, AUTHZ_ID_WIRE_PREFIX, MAX_AUTHZ_ID_BYTES, MAX_AUTHZ_WIRE_BYTES,
};
pub use check::{CheckRequest, CheckResponse, PermissionChecker};
pub use schema::{ComputedPermission, ObjectTypeSchema, PermissionSchema};
pub use tuple::{Object, ObjectType, Relation, Subject, SubjectType, Tuple, TupleStore};

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
}

impl From<DatabaseError> for PermissionError {
    fn from(err: DatabaseError) -> Self {
        PermissionError::DatabaseError(err.to_string())
    }
}

#[cfg(all(test, target_os = "linux"))]
mod parent_move_native_tests;
