//! Permission schema definitions
//!
//! Defines the permission model for space object types following
//! the Zanzibar-inspired approach with computed permissions.

use std::collections::HashMap;

use super::tuple::ObjectType;

/// How a permission is computed
#[derive(Debug, Clone, PartialEq)]
pub enum ComputedPermission {
    /// Permission is granted if the subject has the specified direct relation
    DirectRelation(String),

    /// Permission is granted if ANY of the specified permissions/relations are satisfied
    Union(Vec<ComputedPermission>),

    /// Permission is granted if ALL of the specified permissions/relations are satisfied
    Intersection(Vec<ComputedPermission>),

    /// Permission is granted by following a relation to a parent object and checking a permission there
    /// Format: (relation_to_follow, permission_on_parent)
    /// Example: ("parent", "admin") means follow the "parent" relation and check for "admin" permission
    Arrow(String, String),
}

impl ComputedPermission {
    /// Create a direct relation permission
    pub fn direct(relation: &str) -> Self {
        ComputedPermission::DirectRelation(relation.to_string())
    }

    /// Create a union of permissions
    pub fn union(permissions: Vec<ComputedPermission>) -> Self {
        ComputedPermission::Union(permissions)
    }

    /// Create an arrow permission
    pub fn arrow(relation: &str, permission: &str) -> Self {
        ComputedPermission::Arrow(relation.to_string(), permission.to_string())
    }
}

/// Schema definition for an object type
#[derive(Debug, Clone)]
pub struct ObjectTypeSchema {
    /// Valid relations for this object type
    pub relations: Vec<String>,
    /// Computed permissions (permission_name -> how to compute it)
    pub permissions: HashMap<String, ComputedPermission>,
}

impl ObjectTypeSchema {
    /// Create a new object type schema
    pub fn new() -> Self {
        Self {
            relations: Vec::new(),
            permissions: HashMap::new(),
        }
    }

    /// Add a valid relation
    pub fn with_relation(mut self, relation: &str) -> Self {
        self.relations.push(relation.to_string());
        self
    }

    /// Add a permission with its computation
    pub fn with_permission(mut self, permission: &str, computation: ComputedPermission) -> Self {
        self.permissions.insert(permission.to_string(), computation);
        self
    }

    /// Check if a relation is valid for this object type
    pub fn is_valid_relation(&self, relation: &str) -> bool {
        self.relations.contains(&relation.to_string())
    }

    /// Get the computation for a permission
    pub fn get_permission(&self, permission: &str) -> Option<&ComputedPermission> {
        self.permissions.get(permission)
    }
}

impl Default for ObjectTypeSchema {
    fn default() -> Self {
        Self::new()
    }
}

/// The complete permission schema for all object types
#[derive(Debug, Clone)]
pub struct PermissionSchema {
    schemas: HashMap<ObjectType, ObjectTypeSchema>,
}

impl PermissionSchema {
    /// Create a new empty permission schema
    pub fn new() -> Self {
        Self {
            schemas: HashMap::new(),
        }
    }

    /// Add a schema for an object type
    pub fn with_type(mut self, object_type: ObjectType, schema: ObjectTypeSchema) -> Self {
        self.schemas.insert(object_type, schema);
        self
    }

    /// Get the schema for an object type
    #[cfg(test)]
    pub fn get_schema(&self, object_type: ObjectType) -> Option<&ObjectTypeSchema> {
        self.schemas.get(&object_type)
    }

    /// Check if a relation is valid for an object type
    #[cfg(test)]
    pub fn is_valid_relation(&self, object_type: ObjectType, relation: &str) -> bool {
        self.schemas
            .get(&object_type)
            .map(|s| s.is_valid_relation(relation))
            .unwrap_or(false)
    }

    /// Get the permission computation for an object type
    pub fn get_permission(
        &self,
        object_type: ObjectType,
        permission: &str,
    ) -> Option<&ComputedPermission> {
        self.schemas
            .get(&object_type)
            .and_then(|s| s.get_permission(permission))
    }
}

impl Default for PermissionSchema {
    /// Create the default space permission schema
    fn default() -> Self {
        Self::new()
            .with_type(ObjectType::Server, server_schema())
            .with_type(ObjectType::Space, space_schema())
            .with_type(ObjectType::Channel, channel_schema())
            .with_type(ObjectType::Message, message_schema())
            .with_type(ObjectType::Dm, dm_schema())
            .with_type(ObjectType::Role, role_schema())
    }
}

pub const SPICEDB_SCHEMA: &str = r#"
definition user {}

definition server {
  relation owner: user
  relation admin: user
  relation member: user

  permission create_space = owner
  permission create_muc = owner
}

definition space {
  relation owner: user
  relation admin: user
  relation moderator: user
  relation member: user

  permission delete = owner
  permission manage_settings = owner + admin
  permission manage_roles = owner + admin
  permission manage_members = owner + admin + moderator
  permission create_channel = owner
  permission view = member
  permission update = owner + admin
}

definition channel {
  relation parent: space
  relation owner: user
  relation admin: user
  relation member: user
  relation outcast: user
  relation manager: user
  relation moderator: user
  relation writer: user
  relation viewer: user

  permission delete = parent->admin
  permission manage = owner + admin + manager + parent->admin
  permission moderate = owner + admin + moderator + manager + parent->moderator + parent->admin
  permission send_message = owner + admin + member + writer + moderator + manager + parent->view
  permission read = owner + admin + member + viewer + writer + moderator + manager + parent->view
  permission view = owner + admin + member + viewer + writer + moderator + manager + parent->view
}

definition message {
  relation channel: channel
  relation author: user

  permission read = channel->read
  permission edit = author
  permission delete = author + channel->moderate
  permission react = channel->read
}

definition direct_message {
  relation participant: user

  permission read = participant
  permission send = participant
  permission add_participant = participant
  permission leave = participant
}

definition role {
  relation parent: space
  relation assignee: user
  relation manager: user

  permission assign = manager + parent->admin
  permission manage = manager + parent->admin
}
"#;

/// Schema for deployment/server objects.
fn server_schema() -> ObjectTypeSchema {
    ObjectTypeSchema::new()
        .with_relation("owner")
        .with_relation("admin")
        .with_relation("member")
        .with_permission("owner", ComputedPermission::direct("owner"))
        .with_permission(
            "admin",
            ComputedPermission::union(vec![
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission(
            "member",
            ComputedPermission::union(vec![
                ComputedPermission::direct("member"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission("create_space", ComputedPermission::direct("owner"))
        .with_permission("create_muc", ComputedPermission::direct("owner"))
}

/// Schema for Space objects
fn space_schema() -> ObjectTypeSchema {
    ObjectTypeSchema::new()
        // Relations
        .with_relation("owner")
        .with_relation("admin")
        .with_relation("moderator")
        .with_relation("member")
        // Permissions
        .with_permission("owner", ComputedPermission::direct("owner"))
        .with_permission(
            "admin",
            ComputedPermission::union(vec![
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission(
            "moderator",
            ComputedPermission::union(vec![
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission(
            "member",
            ComputedPermission::union(vec![
                ComputedPermission::direct("member"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission("delete", ComputedPermission::direct("owner"))
        .with_permission(
            "manage_settings",
            ComputedPermission::union(vec![
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission(
            "manage_roles",
            ComputedPermission::union(vec![
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission(
            "manage_members",
            ComputedPermission::union(vec![
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission("create_channel", ComputedPermission::direct("owner"))
        .with_permission(
            "view",
            ComputedPermission::union(vec![
                ComputedPermission::direct("member"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
        .with_permission(
            "update",
            ComputedPermission::union(vec![
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("owner"),
            ]),
        )
}

/// Schema for Channel objects
fn channel_schema() -> ObjectTypeSchema {
    ObjectTypeSchema::new()
        // Relations
        .with_relation("parent")
        .with_relation("owner")
        .with_relation("admin")
        .with_relation("member")
        .with_relation("outcast")
        .with_relation("manager")
        .with_relation("moderator")
        .with_relation("writer")
        .with_relation("viewer")
        // Permissions
        .with_permission(
            "delete",
            // Only space admin can delete channels
            ComputedPermission::arrow("parent", "admin"),
        )
        .with_permission(
            "manage",
            ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "admin"),
            ]),
        )
        .with_permission(
            "moderate",
            ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "moderator"),
                ComputedPermission::arrow("parent", "admin"),
            ]),
        )
        .with_permission(
            "send_message",
            ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("member"),
                ComputedPermission::direct("writer"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "view"),
            ]),
        )
        .with_permission(
            "read",
            ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("member"),
                ComputedPermission::direct("viewer"),
                ComputedPermission::direct("writer"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "view"),
            ]),
        )
        .with_permission(
            "view",
            ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("member"),
                ComputedPermission::direct("viewer"),
                ComputedPermission::direct("writer"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "view"),
            ]),
        )
}

/// Schema for Message objects
fn message_schema() -> ObjectTypeSchema {
    ObjectTypeSchema::new()
        // Relations
        .with_relation("channel")
        .with_relation("author")
        // Permissions
        .with_permission(
            "delete",
            ComputedPermission::union(vec![
                ComputedPermission::direct("author"),
                ComputedPermission::arrow("channel", "moderate"),
            ]),
        )
        .with_permission("edit", ComputedPermission::direct("author"))
        .with_permission("react", ComputedPermission::arrow("channel", "view"))
        .with_permission("read", ComputedPermission::arrow("channel", "view"))
}

/// Schema for DM (direct message) objects
fn dm_schema() -> ObjectTypeSchema {
    ObjectTypeSchema::new()
        // Relations
        .with_relation("participant")
        .with_relation("owner")
        // Permissions
        .with_permission("read", ComputedPermission::direct("participant"))
        .with_permission("send", ComputedPermission::direct("participant"))
        .with_permission(
            "manage",
            ComputedPermission::union(vec![ComputedPermission::direct("owner")]),
        )
        .with_permission("add_participant", ComputedPermission::direct("owner"))
        .with_permission("leave", ComputedPermission::direct("participant"))
}

/// Schema for Role objects
fn role_schema() -> ObjectTypeSchema {
    ObjectTypeSchema::new()
        // Relations
        .with_relation("space")
        .with_relation("member")
        .with_relation("manager")
        // Permissions
        .with_permission(
            "assign",
            ComputedPermission::union(vec![
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("space", "admin"),
            ]),
        )
        .with_permission(
            "delete",
            ComputedPermission::union(vec![
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("space", "admin"),
            ]),
        )
        .with_permission(
            "edit",
            ComputedPermission::union(vec![
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("space", "admin"),
            ]),
        )
}

#[cfg(test)]
mod tests;
