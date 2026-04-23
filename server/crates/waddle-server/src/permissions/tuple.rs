//! Permission tuple types and storage
//!
//! Tuples are the fundamental unit of the permission system.
//! Format: `<object>#<relation>@<subject>`

use std::fmt;
use std::str::FromStr;
#[cfg(test)]
use std::sync::Arc;

use kameo::actor::ActorRef;
use serde::{Deserialize, Serialize};
use tracing::{debug, instrument};
use uuid::Uuid;

use super::PermissionError;
use crate::db::actor::{DbActor, DbExecute, DbQuery, DbQueryOne, RowValues};
use crate::db::{row_value, ValueExt};

/// Types of objects that can be protected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectType {
    Space,
    Channel,
    Message,
    Dm,
    Role,
}

impl fmt::Display for ObjectType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectType::Space => write!(f, "space"),
            ObjectType::Channel => write!(f, "channel"),
            ObjectType::Message => write!(f, "message"),
            ObjectType::Dm => write!(f, "dm"),
            ObjectType::Role => write!(f, "role"),
        }
    }
}

impl FromStr for ObjectType {
    type Err = PermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "space" => Ok(ObjectType::Space),
            "channel" => Ok(ObjectType::Channel),
            "message" => Ok(ObjectType::Message),
            "dm" => Ok(ObjectType::Dm),
            "role" => Ok(ObjectType::Role),
            _ => Err(PermissionError::InvalidObject(format!(
                "Unknown object type: {}",
                s
            ))),
        }
    }
}

/// Types of subjects that can access objects
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubjectType {
    User,
    Space,
    Channel,
    Role,
}

impl fmt::Display for SubjectType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SubjectType::User => write!(f, "user"),
            SubjectType::Space => write!(f, "space"),
            SubjectType::Channel => write!(f, "channel"),
            SubjectType::Role => write!(f, "role"),
        }
    }
}

impl FromStr for SubjectType {
    type Err = PermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "user" => Ok(SubjectType::User),
            "space" => Ok(SubjectType::Space),
            "channel" => Ok(SubjectType::Channel),
            "role" => Ok(SubjectType::Role),
            _ => Err(PermissionError::InvalidSubject(format!(
                "Unknown subject type: {}",
                s
            ))),
        }
    }
}

/// An object in the permission system (e.g., space:penguin-club)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Object {
    pub object_type: ObjectType,
    pub id: String,
}

impl Object {
    /// Create a new object
    pub fn new(object_type: ObjectType, id: impl Into<String>) -> Self {
        Self {
            object_type,
            id: id.into(),
        }
    }

    /// Parse from string format "type:id"
    pub fn parse(s: &str) -> Result<Self, PermissionError> {
        let parts: Vec<&str> = s.splitn(2, ':').collect();
        if parts.len() != 2 {
            return Err(PermissionError::InvalidObject(format!(
                "Invalid object format '{}', expected 'type:id'",
                s
            )));
        }
        let object_type = ObjectType::from_str(parts[0])?;
        Ok(Self {
            object_type,
            id: parts[1].to_string(),
        })
    }
}

impl fmt::Display for Object {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.object_type, self.id)
    }
}

impl FromStr for Object {
    type Err = PermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Object::parse(s)
    }
}

/// A relation between an object and subject (e.g., owner, member, viewer)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Relation {
    pub name: String,
}

impl Relation {
    /// Create a new relation
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl fmt::Display for Relation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.name)
    }
}

impl FromStr for Relation {
    type Err = PermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(PermissionError::InvalidRelation(
                "Relation cannot be empty".to_string(),
            ));
        }
        Ok(Self {
            name: s.to_string(),
        })
    }
}

/// A subject in the permission system
///
/// Can be:
/// - A direct user: `user:user-alice`
/// - A userset: `space:penguin-club#member` (all members of penguin-club)
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Subject {
    pub subject_type: SubjectType,
    pub id: String,
    /// For userset subjects, the relation (e.g., "member" in space:penguin-club#member)
    pub relation: Option<String>,
}

impl Subject {
    /// Create a new direct user subject
    #[allow(dead_code)]
    pub fn user(user_id: impl Into<String>) -> Self {
        Self {
            subject_type: SubjectType::User,
            id: user_id.into(),
            relation: None,
        }
    }

    /// Create a userset subject (e.g., all members of a space)
    #[allow(dead_code)]
    pub fn userset(
        subject_type: SubjectType,
        id: impl Into<String>,
        relation: impl Into<String>,
    ) -> Self {
        Self {
            subject_type,
            id: id.into(),
            relation: Some(relation.into()),
        }
    }

    /// Check if this subject is a userset (has a relation)
    pub fn is_userset(&self) -> bool {
        self.relation.is_some()
    }

    /// Parse from string format "type:id" or "type:id#relation"
    pub fn parse(s: &str) -> Result<Self, PermissionError> {
        // Check for userset format: type:id#relation
        if let Some(hash_pos) = s.rfind('#') {
            let (type_id, relation) = s.split_at(hash_pos);
            let relation = &relation[1..]; // Skip the #

            let parts: Vec<&str> = type_id.splitn(2, ':').collect();
            if parts.len() != 2 {
                return Err(PermissionError::InvalidSubject(format!(
                    "Invalid subject format '{}', expected 'type:id#relation'",
                    s
                )));
            }

            let subject_type = SubjectType::from_str(parts[0])?;
            Ok(Self {
                subject_type,
                id: parts[1].to_string(),
                relation: Some(relation.to_string()),
            })
        } else {
            // Direct subject format: type:id
            let parts: Vec<&str> = s.splitn(2, ':').collect();
            if parts.len() != 2 {
                return Err(PermissionError::InvalidSubject(format!(
                    "Invalid subject format '{}', expected 'type:id'",
                    s
                )));
            }

            let subject_type = SubjectType::from_str(parts[0])?;
            Ok(Self {
                subject_type,
                id: parts[1].to_string(),
                relation: None,
            })
        }
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(ref relation) = self.relation {
            write!(f, "{}:{}#{}", self.subject_type, self.id, relation)
        } else {
            write!(f, "{}:{}", self.subject_type, self.id)
        }
    }
}

impl FromStr for Subject {
    type Err = PermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Subject::parse(s)
    }
}

/// A permission tuple: object#relation@subject
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tuple {
    pub id: String,
    pub object: Object,
    pub relation: Relation,
    pub subject: Subject,
    pub created_at: Option<String>,
}

impl Tuple {
    /// Create a new tuple with a generated ID
    pub fn new(object: Object, relation: Relation, subject: Subject) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            object,
            relation,
            subject,
            created_at: None,
        }
    }

    /// Create a tuple with a specific ID (for loading from database)
    pub fn with_id(
        id: String,
        object: Object,
        relation: Relation,
        subject: Subject,
        created_at: Option<String>,
    ) -> Self {
        Self {
            id,
            object,
            relation,
            subject,
            created_at,
        }
    }

    /// Parse from string format "object#relation@subject"
    pub fn parse(s: &str) -> Result<Self, PermissionError> {
        // Split by # first
        let hash_pos = s
            .find('#')
            .ok_or_else(|| PermissionError::InvalidTuple(format!("Missing '#' in tuple: {}", s)))?;

        let object_str = &s[..hash_pos];
        let rest = &s[hash_pos + 1..];

        // Split rest by @
        let at_pos = rest
            .find('@')
            .ok_or_else(|| PermissionError::InvalidTuple(format!("Missing '@' in tuple: {}", s)))?;

        let relation_str = &rest[..at_pos];
        let subject_str = &rest[at_pos + 1..];

        let object = Object::parse(object_str)?;
        let relation = Relation::from_str(relation_str)?;
        let subject = Subject::parse(subject_str)?;

        Ok(Self::new(object, relation, subject))
    }
}

impl fmt::Display for Tuple {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}@{}", self.object, self.relation, self.subject)
    }
}

impl FromStr for Tuple {
    type Err = PermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Tuple::parse(s)
    }
}

/// Storage layer for permission tuples
pub struct TupleStore {
    actor: ActorRef<DbActor>,
}

impl TupleStore {
    /// Create a new tuple store
    pub fn new(actor: ActorRef<DbActor>) -> Self {
        Self { actor }
    }

    /// Write a new tuple to the database
    #[instrument(skip(self), fields(tuple = %tuple))]
    pub async fn write(&self, tuple: Tuple) -> Result<(), PermissionError> {
        debug!("Writing tuple: {}", tuple);

        // First check if this tuple already exists
        // (SQLite's UNIQUE constraint doesn't work properly with NULL values)
        if self
            .exists(&tuple.object, &tuple.relation.name, &tuple.subject)
            .await?
        {
            return Err(PermissionError::TupleAlreadyExists);
        }

        let subject_relation = tuple.subject.relation.as_deref();
        self.actor
            .ask(DbExecute {
                sql: r#"
                    INSERT INTO permission_tuples (id, object_type, object_id, relation, subject_type, subject_id, subject_relation)
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                "#
                .to_string(),
                params: vec![
                    tuple.id.as_str().into(),
                    tuple.object.object_type.to_string().into(),
                    tuple.object.id.as_str().into(),
                    tuple.relation.name.as_str().into(),
                    tuple.subject.subject_type.to_string().into(),
                    tuple.subject.id.as_str().into(),
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                ],
            })
            .await
            .map_err(|e| {
                if e.to_string().contains("UNIQUE constraint failed") {
                    PermissionError::TupleAlreadyExists
                } else {
                    PermissionError::DatabaseError(e.to_string())
                }
            })?;

        Ok(())
    }

    /// Delete a tuple from the database
    #[instrument(skip(self), fields(tuple = %tuple))]
    pub async fn delete(&self, tuple: &Tuple) -> Result<(), PermissionError> {
        debug!("Deleting tuple: {}", tuple);

        let subject_relation = tuple.subject.relation.as_deref();
        let rows = self
            .actor
            .ask(DbExecute {
                sql: r#"
                    DELETE FROM permission_tuples
                    WHERE object_type = ? AND object_id = ? AND relation = ?
                    AND subject_type = ? AND subject_id = ?
                    AND (subject_relation = ? OR (subject_relation IS NULL AND ? IS NULL))
                "#
                .to_string(),
                params: vec![
                    tuple.object.object_type.to_string().into(),
                    tuple.object.id.as_str().into(),
                    tuple.relation.name.as_str().into(),
                    tuple.subject.subject_type.to_string().into(),
                    tuple.subject.id.as_str().into(),
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                ],
            })
            .await
            .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;

        if rows == 0 {
            return Err(PermissionError::TupleNotFound);
        }

        Ok(())
    }

    /// Check if a specific tuple exists
    #[instrument(skip(self))]
    pub async fn exists(
        &self,
        object: &Object,
        relation: &str,
        subject: &Subject,
    ) -> Result<bool, PermissionError> {
        let subject_relation = subject.relation.as_deref();
        let row = self
            .actor
            .ask(DbQueryOne {
                sql: r#"
                    SELECT 1 FROM permission_tuples
                    WHERE object_type = ? AND object_id = ? AND relation = ?
                    AND subject_type = ? AND subject_id = ?
                    AND (subject_relation = ? OR (subject_relation IS NULL AND ? IS NULL))
                    LIMIT 1
                "#
                .to_string(),
                params: vec![
                    object.object_type.to_string().into(),
                    object.id.as_str().into(),
                    relation.into(),
                    subject.subject_type.to_string().into(),
                    subject.id.as_str().into(),
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                ],
            })
            .await
            .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;

        Ok(row.is_some())
    }

    /// List all relations a subject has on an object
    #[instrument(skip(self))]
    pub async fn list_relations(
        &self,
        subject: &Subject,
        object: &Object,
    ) -> Result<Vec<String>, PermissionError> {
        let subject_relation = subject.relation.as_deref();
        let rows = self
            .actor
            .ask(DbQuery {
                sql: r#"
                    SELECT DISTINCT relation FROM permission_tuples
                    WHERE object_type = ? AND object_id = ?
                    AND subject_type = ? AND subject_id = ?
                    AND (subject_relation = ? OR (subject_relation IS NULL AND ? IS NULL))
                "#
                .to_string(),
                params: vec![
                    object.object_type.to_string().into(),
                    object.id.as_str().into(),
                    subject.subject_type.to_string().into(),
                    subject.id.as_str().into(),
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                ],
            })
            .await
            .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;

        rows.into_iter()
            .map(|row| {
                row_value(&row, 0)
                    .and_then(ValueExt::as_string)
                    .map_err(|e| PermissionError::DatabaseError(e.to_string()))
            })
            .collect()
    }

    /// List all subjects with a specific relation on an object
    #[instrument(skip(self))]
    pub async fn list_subjects(
        &self,
        object: &Object,
        relation: &str,
    ) -> Result<Vec<Subject>, PermissionError> {
        let rows = self
            .actor
            .ask(DbQuery {
                sql: r#"
                    SELECT subject_type, subject_id, subject_relation FROM permission_tuples
                    WHERE object_type = ? AND object_id = ? AND relation = ?
                "#
                .to_string(),
                params: vec![
                    object.object_type.to_string().into(),
                    object.id.as_str().into(),
                    relation.into(),
                ],
            })
            .await
            .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;

        let mut subjects = Vec::new();
        for row in rows {
            let subject_type_str = row_value(&row, 0)
                .and_then(ValueExt::as_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let subject_id = row_value(&row, 1)
                .and_then(ValueExt::as_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let subject_relation = row_value(&row, 2)
                .and_then(ValueExt::as_optional_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;

            let subject_type = SubjectType::from_str(&subject_type_str)?;

            subjects.push(Subject {
                subject_type,
                id: subject_id,
                relation: subject_relation,
            });
        }

        Ok(subjects)
    }

    /// Get all tuples where the subject matches (for expanding usersets)
    #[allow(dead_code)]
    #[instrument(skip(self))]
    pub async fn get_tuples_for_subject(
        &self,
        subject_type: SubjectType,
        subject_id: &str,
        subject_relation: Option<&str>,
    ) -> Result<Vec<Tuple>, PermissionError> {
        let rows = self
            .actor
            .ask(DbQuery {
                sql: r#"
                    SELECT id, object_type, object_id, relation, subject_type, subject_id, subject_relation, created_at
                    FROM permission_tuples
                    WHERE subject_type = ? AND subject_id = ?
                    AND (subject_relation = ? OR (subject_relation IS NULL AND ? IS NULL))
                "#
                .to_string(),
                params: vec![
                    subject_type.to_string().into(),
                    subject_id.into(),
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                    match subject_relation {
                        Some(relation) => relation.into(),
                        None => crate::db::Value::Null,
                    },
                ],
            })
            .await
            .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;

        self.rows_to_tuples(rows)
    }

    /// Get all tuples for an object (optionally filtered by relation)
    #[instrument(skip(self))]
    pub async fn get_tuples_for_object(
        &self,
        object: &Object,
        relation: Option<&str>,
    ) -> Result<Vec<Tuple>, PermissionError> {
        let rows = if let Some(rel) = relation {
            self.actor
                .ask(DbQuery {
                    sql: r#"
                        SELECT id, object_type, object_id, relation, subject_type, subject_id, subject_relation, created_at
                        FROM permission_tuples
                        WHERE object_type = ? AND object_id = ? AND relation = ?
                    "#
                    .to_string(),
                    params: vec![
                        object.object_type.to_string().into(),
                        object.id.as_str().into(),
                        rel.into(),
                    ],
                })
                .await
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?
        } else {
            self.actor
                .ask(DbQuery {
                    sql: r#"
                        SELECT id, object_type, object_id, relation, subject_type, subject_id, subject_relation, created_at
                        FROM permission_tuples
                        WHERE object_type = ? AND object_id = ?
                    "#
                    .to_string(),
                    params: vec![object.object_type.to_string().into(), object.id.as_str().into()],
                })
                .await
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?
        };

        self.rows_to_tuples(rows)
    }

    /// Helper to convert database rows to tuples
    fn rows_to_tuples(&self, rows: Vec<RowValues>) -> Result<Vec<Tuple>, PermissionError> {
        let mut tuples = Vec::new();

        for row in rows {
            let id = row_value(&row, 0)
                .and_then(ValueExt::as_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let object_type_str = row_value(&row, 1)
                .and_then(ValueExt::as_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let object_id = row_value(&row, 2)
                .and_then(ValueExt::as_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let relation = row_value(&row, 3)
                .and_then(ValueExt::as_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let subject_type_str = row_value(&row, 4)
                .and_then(ValueExt::as_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let subject_id = row_value(&row, 5)
                .and_then(ValueExt::as_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let subject_relation = row_value(&row, 6)
                .and_then(ValueExt::as_optional_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;
            let created_at = row_value(&row, 7)
                .and_then(ValueExt::as_optional_string)
                .map_err(|e| PermissionError::DatabaseError(e.to_string()))?;

            let object_type = ObjectType::from_str(&object_type_str)?;
            let subject_type = SubjectType::from_str(&subject_type_str)?;

            tuples.push(Tuple::with_id(
                id,
                Object::new(object_type, object_id),
                Relation::new(relation),
                Subject {
                    subject_type,
                    id: subject_id,
                    relation: subject_relation,
                },
                created_at,
            ));
        }

        Ok(tuples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Database;

    #[test]
    fn test_object_parse() {
        let obj = Object::parse("space:penguin-club").unwrap();
        assert_eq!(obj.object_type, ObjectType::Space);
        assert_eq!(obj.id, "penguin-club");

        let obj = Object::parse("channel:general").unwrap();
        assert_eq!(obj.object_type, ObjectType::Channel);
        assert_eq!(obj.id, "general");
    }

    #[test]
    fn test_object_display() {
        let obj = Object::new(ObjectType::Space, "penguin-club");
        assert_eq!(obj.to_string(), "space:penguin-club");
    }

    #[test]
    fn test_subject_parse_direct() {
        let subj = Subject::parse("user:user-alice").unwrap();
        assert_eq!(subj.subject_type, SubjectType::User);
        assert_eq!(subj.id, "user-alice");
        assert_eq!(subj.relation, None);
    }

    #[test]
    fn test_subject_parse_userset() {
        let subj = Subject::parse("space:penguin-club#member").unwrap();
        assert_eq!(subj.subject_type, SubjectType::Space);
        assert_eq!(subj.id, "penguin-club");
        assert_eq!(subj.relation, Some("member".to_string()));
    }

    #[test]
    fn test_subject_display() {
        let subj = Subject::user("user-alice");
        assert_eq!(subj.to_string(), "user:user-alice");

        let subj = Subject::userset(SubjectType::Space, "penguin-club", "member");
        assert_eq!(subj.to_string(), "space:penguin-club#member");
    }

    #[test]
    fn test_tuple_parse() {
        let tuple = Tuple::parse("space:penguin-club#owner@user:user-alice").unwrap();
        assert_eq!(tuple.object.object_type, ObjectType::Space);
        assert_eq!(tuple.object.id, "penguin-club");
        assert_eq!(tuple.relation.name, "owner");
        assert_eq!(tuple.subject.subject_type, SubjectType::User);
        assert_eq!(tuple.subject.id, "user-alice");
    }

    #[test]
    fn test_tuple_display() {
        let tuple = Tuple::new(
            Object::new(ObjectType::Space, "penguin-club"),
            Relation::new("owner"),
            Subject::user("user-alice"),
        );
        assert_eq!(
            tuple.to_string(),
            "space:penguin-club#owner@user:user-alice"
        );
    }

    #[tokio::test]
    async fn test_tuple_store_write_and_exists() {
        let db = Database::in_memory("test-tuple-store").await.unwrap();
        let db = Arc::new(db);

        // Run migrations
        let runner = crate::db::MigrationRunner::global();
        runner.run(&db).await.unwrap();

        let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
        let store = TupleStore::new(actor);

        let object = Object::new(ObjectType::Space, "test-space");
        let subject = Subject::user("user-alice");

        // Initially should not exist
        assert!(!store.exists(&object, "owner", &subject).await.unwrap());

        // Write the tuple
        let tuple = Tuple::new(object.clone(), Relation::new("owner"), subject.clone());
        store.write(tuple).await.unwrap();

        // Now should exist
        assert!(store.exists(&object, "owner", &subject).await.unwrap());
    }

    #[tokio::test]
    async fn test_tuple_store_delete() {
        let db = Database::in_memory("test-tuple-store-delete")
            .await
            .unwrap();
        let db = Arc::new(db);

        // Run migrations
        let runner = crate::db::MigrationRunner::global();
        runner.run(&db).await.unwrap();

        let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
        let store = TupleStore::new(actor);

        let object = Object::new(ObjectType::Space, "test-space");
        let subject = Subject::user("user-alice");

        // Write the tuple
        let tuple = Tuple::new(object.clone(), Relation::new("owner"), subject.clone());
        store.write(tuple.clone()).await.unwrap();

        // Should exist
        assert!(store.exists(&object, "owner", &subject).await.unwrap());

        // Delete
        store.delete(&tuple).await.unwrap();

        // Should not exist
        assert!(!store.exists(&object, "owner", &subject).await.unwrap());
    }

    #[tokio::test]
    async fn test_tuple_store_list_subjects() {
        let db = Database::in_memory("test-tuple-store-list").await.unwrap();
        let db = Arc::new(db);

        // Run migrations
        let runner = crate::db::MigrationRunner::global();
        runner.run(&db).await.unwrap();

        let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
        let store = TupleStore::new(actor);

        let object = Object::new(ObjectType::Space, "test-space");

        // Add multiple members
        store
            .write(Tuple::new(
                object.clone(),
                Relation::new("member"),
                Subject::user("user-alice"),
            ))
            .await
            .unwrap();
        store
            .write(Tuple::new(
                object.clone(),
                Relation::new("member"),
                Subject::user("user-bob"),
            ))
            .await
            .unwrap();

        // List subjects
        let subjects = store.list_subjects(&object, "member").await.unwrap();
        assert_eq!(subjects.len(), 2);

        let ids: Vec<_> = subjects.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"user-alice"));
        assert!(ids.contains(&"user-bob"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_tuple_store_write_and_exists_file_backed() {
        let base_dir = std::env::temp_dir().join(format!("space-tuple-store-{}", Uuid::new_v4()));
        let db_path = base_dir.join("permissions.db");

        let db = Database::open_local("test-tuple-store-file", &db_path)
            .await
            .unwrap();
        let db = Arc::new(db);

        let runner = crate::db::MigrationRunner::global();
        runner.run(&db).await.unwrap();

        let actor = kameo::spawn(crate::db::actor::DbActor::new((*db).clone()));
        let store = TupleStore::new(actor);
        let object = Object::new(ObjectType::Space, "test-space");
        let subject = Subject::user("user-alice");
        let tuple = Tuple::new(object.clone(), Relation::new("owner"), subject.clone());

        store.write(tuple).await.unwrap();
        assert!(store.exists(&object, "owner", &subject).await.unwrap());

        drop(store);
        drop(db);
        std::fs::remove_dir_all(base_dir).unwrap();
    }
}
