//! SpiceDB-backed permission system for Waddle.

pub mod actor;
mod check;
mod schema;
mod spicedb;
mod tuple;

#[allow(unused_imports)]
pub use actor::{
    CheckPermission, DeleteTuple, EnsureSchema, ListRelations, ListSubjects, LookupResources,
    LookupSubjects, Permission, PermissionActor, WriteTuple,
};
#[allow(unused_imports)]
pub use check::{CheckRequest, CheckResponse, PermissionChecker};
#[allow(unused_imports)]
pub use schema::{ComputedPermission, ObjectTypeSchema, PermissionSchema};
#[allow(unused_imports)]
pub use tuple::{Object, ObjectType, Relation, Subject, SubjectType, Tuple, TupleStore};

use thiserror::Error;

/// Permission-specific errors.
#[derive(Error, Debug)]
pub enum PermissionError {
    #[allow(dead_code)]
    #[error("Permission denied: {0}")]
    Denied(String),

    #[error("Invalid tuple format: {0}")]
    InvalidTuple(String),

    #[error("Invalid object: {0}")]
    InvalidObject(String),

    #[error("Invalid subject: {0}")]
    InvalidSubject(String),

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

impl From<crate::db::DatabaseError> for PermissionError {
    fn from(err: crate::db::DatabaseError) -> Self {
        PermissionError::DatabaseError(err.to_string())
    }
}
