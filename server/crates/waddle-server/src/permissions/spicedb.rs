use std::collections::HashSet;
use std::str::FromStr;

use futures::StreamExt;
use prescience::{
    Client, Consistency, ObjectReference, PermissionResult, Relationship, RelationshipFilter,
    RelationshipUpdate, SubjectFilter, SubjectReference,
};
use tracing::info;

use crate::config::SpiceDbConfig;

use super::{CheckResponse, Object, ObjectType, PermissionError, Subject, SubjectType, Tuple};

const SCHEMA_VERSION_MARKER_A: &str = "waddle-schema-version:";
const SCHEMA_VERSION_MARKER_B: &str = "waddle_schema_version:";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaBootstrapHook {
    pub expected_version: u64,
    pub schema_text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemaBootstrapStatus {
    UpToDate,
    Applied { previous_version: Option<u64> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaBootstrapResult {
    pub status: SchemaBootstrapStatus,
    pub current_version: Option<u64>,
    pub target_version: u64,
}

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

    pub async fn delete_tuple(&self, tuple: &Tuple) -> Result<(), PermissionError> {
        if !self.relationship_exists(tuple).await? {
            return Err(PermissionError::TupleNotFound);
        }

        self.client
            .delete_relationships(exact_relationship_filter(tuple))
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
            .read_relationships(filter_for_object_and_subject(object, subject))
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
            .read_relationships(filter_for_object_and_relation(object, relation))
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
                    resources.push(Object::new(object_type, item.resource_id));
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
                    subjects.push(Subject {
                        subject_type,
                        id: item.subject_id,
                        relation: None,
                    });
                }
                PermissionResult::Denied => {}
                PermissionResult::Conditional { missing_fields } => {
                    return Err(PermissionError::ConditionalPermission(missing_fields));
                }
            }
        }
        Ok(subjects)
    }

    pub async fn bootstrap_schema(
        &self,
        hook: &SchemaBootstrapHook,
    ) -> Result<SchemaBootstrapResult, PermissionError> {
        let target_version = extract_schema_version(&hook.schema_text).ok_or_else(|| {
            PermissionError::SchemaError(format!(
                "target schema missing version marker '{} <number>'",
                SCHEMA_VERSION_MARKER_A
            ))
        })?;
        if target_version != hook.expected_version {
            return Err(PermissionError::SchemaError(format!(
                "target schema version {} does not match expected version {}",
                target_version, hook.expected_version
            )));
        }

        let current_schema = match self.client.read_schema().await {
            Ok((schema, _)) => schema,
            Err(prescience::Error::Status { code, .. }) if format!("{code:?}") == "NotFound" => {
                String::new()
            }
            Err(error) => return Err(map_spicedb_error(error)),
        };
        let current_version = if current_schema.trim().is_empty() {
            None
        } else {
            Some(extract_schema_version(&current_schema).ok_or_else(|| {
                PermissionError::SchemaError(format!(
                    "current schema missing version marker '{} <number>'",
                    SCHEMA_VERSION_MARKER_A
                ))
            })?)
        };

        if let Some(current_version) = current_version {
            if current_version > hook.expected_version {
                return Err(PermissionError::SchemaError(format!(
                    "current schema version {} is newer than expected {}",
                    current_version, hook.expected_version
                )));
            }
            if current_version == hook.expected_version {
                if current_schema.trim() == hook.schema_text.trim() {
                    return Ok(SchemaBootstrapResult {
                        status: SchemaBootstrapStatus::UpToDate,
                        current_version: Some(current_version),
                        target_version: hook.expected_version,
                    });
                }
                return Err(PermissionError::SchemaError(format!(
                    "schema version {} already exists but schema differs; bump WADDLE_SPICEDB_SCHEMA_VERSION",
                    current_version
                )));
            }
        }

        self.client
            .write_schema(&hook.schema_text)
            .await
            .map_err(map_spicedb_error)?;

        info!(
            previous_version = ?current_version,
            new_version = hook.expected_version,
            "Bootstrapped SpiceDB schema"
        );

        Ok(SchemaBootstrapResult {
            status: SchemaBootstrapStatus::Applied {
                previous_version: current_version,
            },
            current_version,
            target_version: hook.expected_version,
        })
    }

    async fn relationship_exists(&self, tuple: &Tuple) -> Result<bool, PermissionError> {
        let mut stream = self
            .client
            .read_relationships(exact_relationship_filter(tuple))
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

fn object_reference(object: &Object) -> Result<ObjectReference, PermissionError> {
    ObjectReference::new(object.object_type.to_string(), object.id.clone())
        .map_err(|e| PermissionError::InvalidObject(e.to_string()))
}

fn subject_reference(subject: &Subject) -> Result<SubjectReference, PermissionError> {
    let object = ObjectReference::new(subject.subject_type.to_string(), subject.id.clone())
        .map_err(|e| PermissionError::InvalidSubject(e.to_string()))?;
    SubjectReference::new(object, subject.relation.clone())
        .map_err(|e| PermissionError::InvalidSubject(e.to_string()))
}

fn subject_from_reference(reference: &SubjectReference) -> Result<Subject, PermissionError> {
    let subject_type = SubjectType::from_str(reference.object().object_type())
        .map_err(|e| PermissionError::InvalidSubject(e.to_string()))?;
    Ok(Subject {
        subject_type,
        id: reference.object().object_id().to_string(),
        relation: reference.optional_relation().map(ToString::to_string),
    })
}

fn relationship_from_tuple(tuple: &Tuple) -> Result<Relationship, PermissionError> {
    Ok(Relationship::new(
        object_reference(&tuple.object)?,
        tuple.relation.name.clone(),
        subject_reference(&tuple.subject)?,
    ))
}

fn exact_relationship_filter(tuple: &Tuple) -> RelationshipFilter {
    let subject_filter = subject_filter(&tuple.subject);
    RelationshipFilter::new(tuple.object.object_type.to_string())
        .resource_id(tuple.object.id.clone())
        .relation(tuple.relation.name.clone())
        .subject_filter(subject_filter)
}

fn filter_for_object_and_subject(object: &Object, subject: &Subject) -> RelationshipFilter {
    RelationshipFilter::new(object.object_type.to_string())
        .resource_id(object.id.clone())
        .subject_filter(subject_filter(subject))
}

fn filter_for_object_and_relation(object: &Object, relation: &str) -> RelationshipFilter {
    RelationshipFilter::new(object.object_type.to_string())
        .resource_id(object.id.clone())
        .relation(relation.to_string())
}

fn subject_filter(subject: &Subject) -> SubjectFilter {
    let filter =
        SubjectFilter::new(subject.subject_type.to_string()).subject_id(subject.id.clone());
    if let Some(relation) = &subject.relation {
        filter.relation(relation.clone())
    } else {
        filter
    }
}

pub fn extract_schema_version(schema: &str) -> Option<u64> {
    schema.lines().find_map(|line| {
        let line = line.trim();
        if !line.starts_with("//") {
            return None;
        }

        let marker = if line.contains(SCHEMA_VERSION_MARKER_A) {
            SCHEMA_VERSION_MARKER_A
        } else if line.contains(SCHEMA_VERSION_MARKER_B) {
            SCHEMA_VERSION_MARKER_B
        } else {
            return None;
        };

        line.split_once(marker)
            .and_then(|(_, value)| value.trim().parse::<u64>().ok())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_schema_version_from_supported_markers() {
        assert_eq!(
            extract_schema_version("// waddle-schema-version: 3\ndefinition user {}"),
            Some(3)
        );
        assert_eq!(
            extract_schema_version("// waddle_schema_version: 4\ndefinition user {}"),
            Some(4)
        );
    }

    #[test]
    fn returns_none_when_schema_marker_is_missing() {
        assert_eq!(extract_schema_version("definition user {}"), None);
    }
}
