use std::str::FromStr;

use kameo::actor::ActorRef;
use tracing::{debug, instrument};

use super::super::PermissionError;
use super::types::{Object, ObjectType, Relation, Subject, SubjectType, Tuple};
use crate::db::actor::{DbActor, DbExecute, DbQuery, DbQueryOne, RowValues};
use crate::db::{row_value, ValueExt};

/// Storage layer for permission tuples.
pub struct TupleStore {
    actor: ActorRef<DbActor>,
}

impl TupleStore {
    /// Create a new tuple store.
    pub fn new(actor: ActorRef<DbActor>) -> Self {
        Self { actor }
    }

    /// Write a new tuple to the database.
    #[instrument(skip(self), fields(tuple = %tuple))]
    pub async fn write(&self, tuple: Tuple) -> Result<(), PermissionError> {
        debug!("Writing tuple: {}", tuple);

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

    /// Delete a tuple from the database.
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

    /// Check if a specific tuple exists.
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

    /// List all relations a subject has on an object.
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

    /// List all subjects with a specific relation on an object.
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

    /// Get all tuples for an object.
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

        Self::rows_to_tuples(rows)
    }

    fn rows_to_tuples(rows: Vec<RowValues>) -> Result<Vec<Tuple>, PermissionError> {
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
