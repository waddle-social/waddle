//! Zanzibar-inspired permission system for Waddle
//!
//! This module implements a relationship-based access control (ReBAC) model
//! inspired by Google's Zanzibar paper. The core concepts are:
//!
//! - **Tuples**: `<object>#<relation>@<subject>` - the fundamental unit
//! - **Objects**: Resources that are protected (waddle, channel, message, etc.)
//! - **Subjects**: Entities that access objects (users, groups of users)
//! - **Relations**: Named connections between objects and subjects
//!
//! # Example
//!
//! ```ignore
//! // Alice is owner of penguin-club waddle
//! waddle:penguin-club#owner@user:did:plc:alice
//!
//! // Channel general belongs to penguin-club
//! channel:general#parent@waddle:penguin-club
//!
//! // All penguin-club members can view general channel
//! channel:general#viewer@waddle:penguin-club#member
//! ```

mod check;
mod schema;
mod tuple;

pub use check::{CheckRequest, CheckResponse, PermissionChecker};
#[allow(unused_imports)]
pub use schema::{ComputedPermission, ObjectTypeSchema, PermissionSchema};
#[allow(unused_imports)]
pub use tuple::{Object, ObjectType, Relation, Subject, SubjectType, Tuple, TupleStore};

use std::sync::Arc;
use thiserror::Error;

use crate::db::Database;

/// Permission-specific errors
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

    #[allow(dead_code)]
    #[error("Schema error: {0}")]
    SchemaError(String),

    #[error("Check depth exceeded maximum of {0}")]
    MaxDepthExceeded(usize),
}

impl From<crate::db::DatabaseError> for PermissionError {
    fn from(err: crate::db::DatabaseError) -> Self {
        PermissionError::DatabaseError(err.to_string())
    }
}

/// Main permission service that combines tuple storage and permission checking
pub struct PermissionService {
    pub tuple_store: TupleStore,
    pub checker: PermissionChecker,
}

impl PermissionService {
    /// Create a new permission service
    pub fn new(db: Arc<Database>) -> Self {
        let tuple_store = TupleStore::new(Arc::clone(&db));
        let schema = PermissionSchema::default();
        let checker = PermissionChecker::new(Arc::clone(&db), schema);

        Self {
            tuple_store,
            checker,
        }
    }

    /// Check if a subject has a permission on an object
    pub async fn check(
        &self,
        subject: &Subject,
        permission: &str,
        object: &Object,
    ) -> Result<CheckResponse, PermissionError> {
        let request = CheckRequest {
            subject: subject.clone(),
            permission: permission.to_string(),
            object: object.clone(),
        };
        self.checker.check(request).await
    }

    /// Write a new permission tuple
    pub async fn write_tuple(&self, tuple: Tuple) -> Result<(), PermissionError> {
        self.tuple_store.write(tuple).await
    }

    /// Delete a permission tuple
    pub async fn delete_tuple(&self, tuple: &Tuple) -> Result<(), PermissionError> {
        self.tuple_store.delete(tuple).await
    }

    /// List all relations a subject has on an object
    pub async fn list_relations(
        &self,
        subject: &Subject,
        object: &Object,
    ) -> Result<Vec<String>, PermissionError> {
        self.tuple_store.list_relations(subject, object).await
    }

    /// List all subjects with a specific relation on an object
    #[allow(dead_code)]
    pub async fn list_subjects(
        &self,
        object: &Object,
        relation: &str,
    ) -> Result<Vec<Subject>, PermissionError> {
        self.tuple_store.list_subjects(object, relation).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_permission_service_basic() {
        let db = Database::in_memory("test-permissions").await.unwrap();
        let db = Arc::new(db);

        // Run migrations
        let runner = crate::db::MigrationRunner::global();
        runner.run(&db).await.unwrap();

        let service = PermissionService::new(db);

        // Create a tuple: user:alice is owner of waddle:test
        let tuple = Tuple::new(
            Object::new(ObjectType::Waddle, "test-waddle"),
            Relation::new("owner"),
            Subject::user("did:plc:alice"),
        );

        // Write the tuple
        service.write_tuple(tuple.clone()).await.unwrap();

        // Check permission - owner should have delete permission
        let subject = Subject::user("did:plc:alice");
        let object = Object::new(ObjectType::Waddle, "test-waddle");

        let response = service.check(&subject, "delete", &object).await.unwrap();
        assert!(response.allowed);

        // Delete the tuple
        service.delete_tuple(&tuple).await.unwrap();
    }
}
