use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::super::PermissionError;

/// Types of objects that can be protected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ObjectType {
    Server,
    Space,
    Channel,
    Message,
    Dm,
    Role,
}

impl fmt::Display for ObjectType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ObjectType::Server => write!(f, "server"),
            ObjectType::Space => write!(f, "space"),
            ObjectType::Channel => write!(f, "channel"),
            ObjectType::Message => write!(f, "message"),
            ObjectType::Dm => write!(f, "direct_message"),
            ObjectType::Role => write!(f, "role"),
        }
    }
}

impl FromStr for ObjectType {
    type Err = PermissionError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "server" => Ok(ObjectType::Server),
            "space" => Ok(ObjectType::Space),
            "channel" => Ok(ObjectType::Channel),
            "message" => Ok(ObjectType::Message),
            "direct_message" => Ok(ObjectType::Dm),
            "role" => Ok(ObjectType::Role),
            _ => Err(PermissionError::InvalidObject(format!(
                "Unknown object type: {}",
                s
            ))),
        }
    }
}

/// Types of subjects that can access objects.
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

/// An object in the permission system, e.g. `space:penguin-club`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Object {
    pub object_type: ObjectType,
    pub id: String,
}

impl Object {
    /// Create a new object.
    pub fn new(object_type: ObjectType, id: impl Into<String>) -> Self {
        Self {
            object_type,
            id: id.into(),
        }
    }

    /// Parse from string format `type:id`.
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

/// A relation between an object and subject, e.g. owner, member, viewer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Relation {
    pub name: String,
}

impl Relation {
    /// Create a new relation.
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

/// A subject in the permission system.
///
/// Can be:
/// - A direct user: `user:user-alice`
/// - A userset: `space:penguin-club#member`
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Subject {
    pub subject_type: SubjectType,
    pub id: String,
    /// For userset subjects, the relation, e.g. "member" in
    /// `space:penguin-club#member`.
    pub relation: Option<String>,
}

impl Subject {
    /// Create a new direct user subject.
    pub fn user(user_id: impl Into<String>) -> Self {
        Self {
            subject_type: SubjectType::User,
            id: user_id.into(),
            relation: None,
        }
    }

    /// Create a userset subject.
    pub fn userset(
        subject_type: SubjectType,
        id: impl Into<String>,
        relation: impl Into<String>,
    ) -> Self {
        let relation = relation.into();
        Self {
            subject_type,
            id: id.into(),
            relation: if relation.is_empty() {
                None
            } else {
                Some(relation)
            },
        }
    }

    /// Check if this subject is a userset.
    pub fn is_userset(&self) -> bool {
        self.relation.is_some()
    }

    /// Parse from string format `type:id` or `type:id#relation`.
    pub fn parse(s: &str) -> Result<Self, PermissionError> {
        if let Some(hash_pos) = s.rfind('#') {
            let (type_id, relation) = s.split_at(hash_pos);
            let relation = &relation[1..];

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

/// A permission tuple: `object#relation@subject`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Tuple {
    pub id: String,
    pub object: Object,
    pub relation: Relation,
    pub subject: Subject,
    pub created_at: Option<String>,
}

impl Tuple {
    /// Create a new tuple with a generated ID.
    pub fn new(object: Object, relation: Relation, subject: Subject) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            object,
            relation,
            subject,
            created_at: None,
        }
    }

    /// Create a tuple with a specific ID.
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

    /// Parse from string format `object#relation@subject`.
    pub fn parse(s: &str) -> Result<Self, PermissionError> {
        let hash_pos = s
            .find('#')
            .ok_or_else(|| PermissionError::InvalidTuple(format!("Missing '#' in tuple: {}", s)))?;

        let object_str = &s[..hash_pos];
        let rest = &s[hash_pos + 1..];

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
