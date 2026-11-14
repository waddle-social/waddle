//! The identifiers the production SpiceDB backend sends and accepts.
//!
//! Each function runs the backend's own conversion and returns the resulting
//! wire values as plain data, so integration tests can assert the encoding
//! without a SpiceDB server.

use prescience::{ObjectReference, RelationshipFilter, SubjectReference};

use crate::permissions::{
    exact_relationship_filter, filter_for_object_and_relation, filter_for_object_and_subject,
    object_from_resource_id, object_reference, relationship_from_tuple, subject_from_lookup_id,
    subject_from_reference, subject_reference, Object, ObjectType, PermissionError, Subject,
    SubjectType, Tuple,
};

/// A SpiceDB object or subject reference as sent on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireReference {
    pub object_type: String,
    pub object_id: String,
    pub relation: Option<String>,
}

/// A relationship as `WriteRelationships` / `DeleteRelationships` send it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireRelationship {
    pub resource: WireReference,
    pub relation: String,
    pub subject: WireReference,
}

/// A `ReadRelationships` / precondition filter as sent on the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireFilter {
    pub resource_type: String,
    pub resource_id: Option<String>,
    pub relation: Option<String>,
    pub subject: Option<WireSubjectFilter>,
}

/// The subject side of a [`WireFilter`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WireSubjectFilter {
    pub subject_type: String,
    pub subject_id: Option<String>,
    pub relation: Option<String>,
}

/// The resource reference used by `CheckPermission` and lookups.
pub fn check_resource(object: &Object) -> Result<WireReference, PermissionError> {
    let reference = object_reference(object)?;
    Ok(WireReference {
        object_type: reference.object_type().to_string(),
        object_id: reference.object_id().to_string(),
        relation: None,
    })
}

/// The subject reference used by `CheckPermission` and `LookupResources`.
pub fn check_subject(subject: &Subject) -> Result<WireReference, PermissionError> {
    Ok(wire_subject(&subject_reference(subject)?))
}

/// The relationship written for, or deleted with, `tuple`.
pub fn relationship(tuple: &Tuple) -> Result<WireRelationship, PermissionError> {
    let relationship = relationship_from_tuple(tuple)?;
    Ok(WireRelationship {
        resource: WireReference {
            object_type: relationship.resource.object_type().to_string(),
            object_id: relationship.resource.object_id().to_string(),
            relation: None,
        },
        relation: relationship.relation,
        subject: wire_subject(&relationship.subject),
    })
}

/// The exact filter used for existence checks, deletes and swap preconditions.
pub fn exact_filter(tuple: &Tuple) -> Result<WireFilter, PermissionError> {
    Ok(wire_filter(exact_relationship_filter(tuple)?))
}

/// The filter used by `ListRelations`.
pub fn object_and_subject_filter(
    object: &Object,
    subject: &Subject,
) -> Result<WireFilter, PermissionError> {
    Ok(wire_filter(filter_for_object_and_subject(object, subject)?))
}

/// The filter used by `ListSubjects`.
pub fn object_and_relation_filter(
    object: &Object,
    relation: &str,
) -> Result<WireFilter, PermissionError> {
    Ok(wire_filter(filter_for_object_and_relation(
        object, relation,
    )?))
}

/// Decode a relationship subject as `ListSubjects` does.
pub fn read_relationship_subject(
    subject_type: &str,
    subject_id: &str,
    relation: Option<&str>,
) -> Result<Subject, PermissionError> {
    let object = ObjectReference::new(subject_type, subject_id)
        .map_err(|error| PermissionError::InvalidSubject(error.to_string()))?;
    let reference = SubjectReference::new(object, relation.map(ToString::to_string))
        .map_err(|error| PermissionError::InvalidSubject(error.to_string()))?;
    subject_from_reference(&reference)
}

/// Decode a `LookupResources` result as the backend does.
pub fn read_lookup_resource(
    object_type: ObjectType,
    resource_id: &str,
) -> Result<Object, PermissionError> {
    object_from_resource_id(object_type, resource_id)
}

/// Decode a `LookupSubjects` result as the backend does.
pub fn read_lookup_subject(
    subject_type: SubjectType,
    subject_id: &str,
) -> Result<Subject, PermissionError> {
    subject_from_lookup_id(subject_type, subject_id)
}

fn wire_subject(reference: &SubjectReference) -> WireReference {
    WireReference {
        object_type: reference.object().object_type().to_string(),
        object_id: reference.object().object_id().to_string(),
        relation: reference.optional_relation().map(ToString::to_string),
    }
}

fn wire_filter(filter: RelationshipFilter) -> WireFilter {
    WireFilter {
        resource_type: filter.resource_type,
        resource_id: filter.optional_resource_id,
        relation: filter.optional_relation,
        subject: filter
            .optional_subject_filter
            .map(|subject| WireSubjectFilter {
                subject_type: subject.subject_type,
                subject_id: subject.optional_subject_id,
                relation: subject.optional_relation,
            }),
    }
}
