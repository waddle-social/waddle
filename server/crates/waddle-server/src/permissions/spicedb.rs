use std::collections::HashSet;
use std::str::FromStr;

use futures::StreamExt;
use prescience::{
    Client, Consistency, ObjectReference, PermissionResult, Precondition, Relationship,
    RelationshipFilter, RelationshipUpdate, SubjectFilter, SubjectReference,
};

use crate::config::SpiceDbConfig;

use super::actor::{ExclusiveRelationSwap, ReplaceExclusiveRelation, SwapExclusiveRelation};
use super::authz_id::{decode_object, decode_subject, EncodedObjectId, EncodedSubjectId};
use super::{
    CheckResponse, Object, ObjectType, PermissionError, Relation, Subject, SubjectType, Tuple,
};

#[derive(Clone)]
pub struct SpiceDbPermissionBackend {
    client: Client,
}

impl SpiceDbPermissionBackend {
    pub async fn connect(config: &SpiceDbConfig) -> Result<Self, PermissionError> {
        let client = Client::builder(&config.endpoint, &config.preshared_key)
            .insecure(config.insecure)
            .build()
            .await
            .map_err(map_spicedb_error)?;
        Ok(Self { client })
    }

    pub async fn ensure_schema(&self, schema: &str) -> Result<(), PermissionError> {
        self.client
            .write_schema(schema)
            .await
            .map_err(map_spicedb_error)?;
        Ok(())
    }

    pub async fn check(
        &self,
        subject: &Subject,
        permission: &str,
        object: &Object,
    ) -> Result<CheckResponse, PermissionError> {
        let resource = object_reference(object)?;
        let subject = subject_reference(subject)?;
        let result = self
            .client
            .check_permission(&resource, permission, &subject)
            .consistency(Consistency::FullyConsistent)
            .await
            .map_err(map_spicedb_error)?;

        match result {
            PermissionResult::Allowed => Ok(CheckResponse::allowed("spicedb")),
            PermissionResult::Denied => Ok(CheckResponse::denied()),
            PermissionResult::Conditional { missing_fields } => {
                Err(PermissionError::ConditionalPermission(missing_fields))
            }
        }
    }

    pub async fn write_tuple(&self, tuple: Tuple) -> Result<(), PermissionError> {
        if self.relationship_exists(&tuple).await? {
            return Err(PermissionError::TupleAlreadyExists);
        }

        let relationship = relationship_from_tuple(&tuple)?;
        self.client
            .write_relationships(vec![RelationshipUpdate::create(relationship)])
            .await
            .map_err(map_spicedb_error)?;
        Ok(())
    }

    /// The complete family is written in one SpiceDB transaction, which
    /// serializes with the preconditions of conditional rollback swaps.
    pub async fn replace_exclusive_relation(
        &self,
        msg: ReplaceExclusiveRelation,
    ) -> Result<(), PermissionError> {
        let updates = msg
            .family
            .iter()
            .map(|relation| {
                let tuple = Tuple::new(msg.object.clone(), relation.clone(), msg.subject.clone());
                let relationship = relationship_from_tuple(&tuple)?;
                Ok(if msg.replacement.as_ref() == Some(relation) {
                    RelationshipUpdate::touch(relationship)
                } else {
                    RelationshipUpdate::delete(relationship)
                })
            })
            .collect::<Result<Vec<_>, PermissionError>>()?;
        self.client
            .write_relationships(updates)
            .await
            .map_err(map_spicedb_error)?;
        Ok(())
    }

    /// One `WriteRelationships` request: SpiceDB checks that the family holds
    /// exactly `expected` and applies the delete/touch atomically, so no
    /// concurrent writer can slip between the comparison and the mutation.
    pub async fn swap_exclusive_relation(
        &self,
        msg: SwapExclusiveRelation,
    ) -> Result<ExclusiveRelationSwap, PermissionError> {
        let tuple_for = |relation: &Relation| {
            Tuple::new(
                msg.object.clone(),
                Relation::new(relation.name.clone()),
                msg.subject.clone(),
            )
        };
        // Reject unencodable identifiers even when the swap is a no-op.
        EncodedObjectId::encode(&msg.object)?;
        EncodedSubjectId::encode(&msg.subject)?;
        if msg.expected.as_ref().map(|relation| &relation.name)
            == msg.replacement.as_ref().map(|relation| &relation.name)
        {
            return Ok(ExclusiveRelationSwap::Swapped);
        }
        let preconditions = msg
            .family
            .iter()
            .map(|relation| {
                let filter = exact_relationship_filter(&tuple_for(relation))?;
                let is_expected = msg
                    .expected
                    .as_ref()
                    .is_some_and(|expected| expected.name == relation.name);
                Ok(if is_expected {
                    Precondition::must_exist(filter)
                } else {
                    Precondition::must_not_exist(filter)
                })
            })
            .collect::<Result<Vec<_>, PermissionError>>()?;
        let mut updates = Vec::new();
        if let Some(expected) = &msg.expected {
            updates.push(RelationshipUpdate::delete(relationship_from_tuple(
                &tuple_for(expected),
            )?));
        }
        if let Some(replacement) = &msg.replacement {
            updates.push(RelationshipUpdate::touch(relationship_from_tuple(
                &tuple_for(replacement),
            )?));
        }
        match self
            .client
            .write_relationships(updates)
            .preconditions(preconditions)
            .await
        {
            Ok(_) => Ok(ExclusiveRelationSwap::Swapped),
            Err(prescience::Error::Status { code, .. })
                if format!("{code:?}") == "FailedPrecondition" =>
            {
                Ok(ExclusiveRelationSwap::Mismatch)
            }
            Err(error) => Err(map_spicedb_error(error)),
        }
    }

    pub async fn delete_tuple(&self, tuple: &Tuple) -> Result<(), PermissionError> {
        if !self.relationship_exists(tuple).await? {
            return Err(PermissionError::TupleNotFound);
        }

        self.client
            .delete_relationships(exact_relationship_filter(tuple)?)
            .await
            .map_err(map_spicedb_error)?;
        Ok(())
    }

    pub async fn list_relations(
        &self,
        subject: &Subject,
        object: &Object,
    ) -> Result<Vec<String>, PermissionError> {
        let mut stream = self
            .client
            .read_relationships(filter_for_object_and_subject(object, subject)?)
            .consistency(Consistency::FullyConsistent)
            .send()
            .await
            .map_err(map_spicedb_error)?;

        let mut relations = HashSet::new();
        while let Some(item) = stream.next().await {
            let item = item.map_err(map_spicedb_error)?;
            relations.insert(item.relationship.relation);
        }

        let mut relations: Vec<String> = relations.into_iter().collect();
        relations.sort();
        Ok(relations)
    }

    pub async fn list_subjects(
        &self,
        object: &Object,
        relation: &str,
    ) -> Result<Vec<Subject>, PermissionError> {
        let mut stream = self
            .client
            .read_relationships(filter_for_object_and_relation(object, relation)?)
            .consistency(Consistency::FullyConsistent)
            .send()
            .await
            .map_err(map_spicedb_error)?;

        let mut subjects = Vec::new();
        while let Some(item) = stream.next().await {
            let item = item.map_err(map_spicedb_error)?;
            subjects.push(subject_from_reference(&item.relationship.subject)?);
        }

        Ok(subjects)
    }

    pub async fn lookup_resources(
        &self,
        subject: &Subject,
        permission: &str,
        object_type: ObjectType,
    ) -> Result<Vec<Object>, PermissionError> {
        let subject = subject_reference(subject)?;
        let mut stream = self
            .client
            .lookup_resources(object_type.to_string(), permission, &subject)
            .consistency(Consistency::FullyConsistent)
            .send()
            .await
            .map_err(map_spicedb_error)?;

        let mut resources = Vec::new();
        while let Some(item) = stream.next().await {
            let item = item.map_err(map_spicedb_error)?;
            match item.permission {
                PermissionResult::Allowed => {
                    resources.push(object_from_resource_id(object_type, &item.resource_id)?);
                }
                PermissionResult::Denied => {}
                PermissionResult::Conditional { missing_fields } => {
                    return Err(PermissionError::ConditionalPermission(missing_fields));
                }
            }
        }
        Ok(resources)
    }

    pub async fn lookup_subjects(
        &self,
        object: &Object,
        permission: &str,
        subject_type: SubjectType,
    ) -> Result<Vec<Subject>, PermissionError> {
        let resource = object_reference(object)?;
        let mut stream = self
            .client
            .lookup_subjects(&resource, permission, subject_type.to_string())
            .consistency(Consistency::FullyConsistent)
            .send()
            .await
            .map_err(map_spicedb_error)?;

        let mut subjects = Vec::new();
        while let Some(item) = stream.next().await {
            let item = item.map_err(map_spicedb_error)?;
            match item.permission {
                PermissionResult::Allowed => {
                    subjects.push(subject_from_lookup_id(subject_type, &item.subject_id)?);
                }
                PermissionResult::Denied => {}
                PermissionResult::Conditional { missing_fields } => {
                    return Err(PermissionError::ConditionalPermission(missing_fields));
                }
            }
        }
        Ok(subjects)
    }

    async fn relationship_exists(&self, tuple: &Tuple) -> Result<bool, PermissionError> {
        let mut stream = self
            .client
            .read_relationships(exact_relationship_filter(tuple)?)
            .consistency(Consistency::FullyConsistent)
            .limit(1)
            .send()
            .await
            .map_err(map_spicedb_error)?;

        if let Some(item) = stream.next().await {
            item.map_err(map_spicedb_error)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

fn map_spicedb_error(error: prescience::Error) -> PermissionError {
    match error {
        prescience::Error::InvalidArgument(message) => PermissionError::InvalidTuple(message),
        prescience::Error::ConditionalPermission { missing_fields } => {
            PermissionError::ConditionalPermission(missing_fields)
        }
        prescience::Error::Status { code, message, .. } => match format!("{code:?}").as_str() {
            "AlreadyExists" => PermissionError::TupleAlreadyExists,
            "NotFound" => PermissionError::TupleNotFound,
            "InvalidArgument" => PermissionError::InvalidTuple(message),
            _ => PermissionError::SpiceDbError(format!("{code:?}: {message}")),
        },
        other => PermissionError::SpiceDbError(other.to_string()),
    }
}

pub(crate) fn object_reference(object: &Object) -> Result<ObjectReference, PermissionError> {
    let id = EncodedObjectId::encode(object)?;
    ObjectReference::new(object.object_type.to_string(), id.as_str())
        .map_err(|e| PermissionError::InvalidObject(e.to_string()))
}

pub(crate) fn subject_reference(subject: &Subject) -> Result<SubjectReference, PermissionError> {
    let id = EncodedSubjectId::encode(subject)?;
    let object = ObjectReference::new(subject.subject_type.to_string(), id.as_str())
        .map_err(|e| PermissionError::InvalidSubject(e.to_string()))?;
    SubjectReference::new(object, subject.relation.clone())
        .map_err(|e| PermissionError::InvalidSubject(e.to_string()))
}

/// Decode a relationship subject read back from SpiceDB. An ID that is not in
/// the current encoding fails the whole read rather than being skipped or
/// reinterpreted.
pub(crate) fn subject_from_reference(
    reference: &SubjectReference,
) -> Result<Subject, PermissionError> {
    let subject_type = SubjectType::from_str(reference.object().object_type())
        .map_err(|e| PermissionError::InvalidSubject(e.to_string()))?;
    Ok(decode_subject(
        subject_type,
        reference.object().object_id(),
        reference.optional_relation().map(ToString::to_string),
    )?)
}

/// Decode a `LookupResources` result for the requested object type.
pub(crate) fn object_from_resource_id(
    object_type: ObjectType,
    resource_id: &str,
) -> Result<Object, PermissionError> {
    Ok(decode_object(object_type, resource_id)?)
}

/// Decode a `LookupSubjects` result for the requested subject type.
pub(crate) fn subject_from_lookup_id(
    subject_type: SubjectType,
    subject_id: &str,
) -> Result<Subject, PermissionError> {
    Ok(decode_subject(subject_type, subject_id, None)?)
}

pub(crate) fn relationship_from_tuple(tuple: &Tuple) -> Result<Relationship, PermissionError> {
    Ok(Relationship::new(
        object_reference(&tuple.object)?,
        tuple.relation.name.clone(),
        subject_reference(&tuple.subject)?,
    ))
}

pub(crate) fn exact_relationship_filter(
    tuple: &Tuple,
) -> Result<RelationshipFilter, PermissionError> {
    let subject_filter = subject_filter(&tuple.subject)?;
    Ok(
        RelationshipFilter::new(tuple.object.object_type.to_string())
            .resource_id(EncodedObjectId::encode(&tuple.object)?.as_str())
            .relation(tuple.relation.name.clone())
            .subject_filter(subject_filter),
    )
}

pub(crate) fn filter_for_object_and_subject(
    object: &Object,
    subject: &Subject,
) -> Result<RelationshipFilter, PermissionError> {
    Ok(RelationshipFilter::new(object.object_type.to_string())
        .resource_id(EncodedObjectId::encode(object)?.as_str())
        .subject_filter(subject_filter(subject)?))
}

pub(crate) fn filter_for_object_and_relation(
    object: &Object,
    relation: &str,
) -> Result<RelationshipFilter, PermissionError> {
    Ok(RelationshipFilter::new(object.object_type.to_string())
        .resource_id(EncodedObjectId::encode(object)?.as_str())
        .relation(relation.to_string()))
}

fn subject_filter(subject: &Subject) -> Result<SubjectFilter, PermissionError> {
    let filter = SubjectFilter::new(subject.subject_type.to_string())
        .subject_id(EncodedSubjectId::encode(subject)?.as_str());
    Ok(if let Some(relation) = &subject.relation {
        filter.relation(relation.clone())
    } else {
        filter
    })
}
