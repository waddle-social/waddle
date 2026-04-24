//! Zanzibar-inspired permission system for spaces
//!
//! This module implements a relationship-based access control (ReBAC) model
//! inspired by Google's Zanzibar paper. The core concepts are:
//!
//! - **Tuples**: `<object>#<relation>@<subject>` - the fundamental unit
//! - **Objects**: Resources that are protected (space, channel, message, etc.)
//! - **Subjects**: Entities that access objects (users, groups of users)
//! - **Relations**: Named connections between objects and subjects
//!
//! # Example
//!
//! ```ignore
//! // Alice is owner of penguin-club space
//! space:penguin-club#owner@user:user-alice
//!
//! // Channel general belongs to penguin-club
//! channel:general#parent@space:penguin-club
//!
//! // All penguin-club members can view general channel
//! channel:general#viewer@space:penguin-club#member
//! ```

mod check;
mod schema;
mod tuple;

pub use check::{CheckRequest, CheckResponse, PermissionChecker};
#[allow(unused_imports)]
pub use schema::{ComputedPermission, ObjectTypeSchema, PermissionSchema};
#[allow(unused_imports)]
pub use tuple::{Object, ObjectType, Relation, Subject, SubjectType, Tuple, TupleStore};

use kameo::actor::ActorRef;
use thiserror::Error;

use crate::db::actor::DbActor;

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

    #[cfg(test)]
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
    pub fn new(actor: ActorRef<DbActor>) -> Self {
        let tuple_store = TupleStore::new(actor.clone());
        let schema = PermissionSchema::default();
        let checker = PermissionChecker::new(actor, schema);

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
    #[cfg(test)]
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
    use crate::db::Database;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_permission_service_basic() {
        let db = Database::in_memory("test-permissions").await.unwrap();
        let db = Arc::new(db);

        // Run migrations
        let runner = crate::db::MigrationRunner::global();
        runner.run(&db).await.unwrap();

        let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
        let service = PermissionService::new(actor);

        // Create a tuple: user:alice is owner of space:test
        let tuple = Tuple::new(
            Object::new(ObjectType::Space, "test-space"),
            Relation::new("owner"),
            Subject::user("user-alice"),
        );

        // Write the tuple
        service.write_tuple(tuple.clone()).await.unwrap();

        // Check permission - owner should have delete permission
        let subject = Subject::user("user-alice");
        let object = Object::new(ObjectType::Space, "test-space");

        let response = service.check(&subject, "delete", &object).await.unwrap();
        assert!(response.allowed);

        // Delete the tuple
        service.delete_tuple(&tuple).await.unwrap();
    }
}
