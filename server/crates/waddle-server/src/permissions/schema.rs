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

    /// Permission is granted if the first computation is satisfied and the
    /// second is not, like SpiceDB's `-` operator. The subtracted computation
    /// wins over every grant in the first, including grants reached through
    /// arrows and usersets.
    /// Format: (granted, subtracted)
    Exclusion(Box<ComputedPermission>, Box<ComputedPermission>),
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

    /// Create a permission granted by `granted` unless `subtracted` holds
    pub fn exclusion(granted: ComputedPermission, subtracted: ComputedPermission) -> Self {
        ComputedPermission::Exclusion(Box::new(granted), Box::new(subtracted))
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
    pub fn permission(&self, permission: &str) -> Option<&ComputedPermission> {
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
    pub fn schema(&self, object_type: ObjectType) -> Option<&ObjectTypeSchema> {
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
    pub fn permission(
        &self,
        object_type: ObjectType,
        permission: &str,
    ) -> Option<&ComputedPermission> {
        self.schemas
            .get(&object_type)
            .and_then(|s| s.permission(permission))
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

/// The built-in SpiceDB schema, version [`AUTHZ_SCHEMA_VERSION`].
///
/// Startup writes it only to install or upgrade a store, and never over a
/// store recorded at another version (`permissions::authz_schema_version`).
/// Any change to this text therefore needs a new schema version: bump
/// [`AUTHZ_SCHEMA_VERSION`] and rename the `version_N` relation of
/// `waddle_authz_schema` to match.
///
/// Channel outcast precedence: every channel permission subtracts `outcast`,
/// including what a subject holds through the parent Space (`parent->...`), so
/// a room ban wins over every channel grant and over Space-inherited access.
/// Message permissions that go through the channel (`read`, `react`, and a
/// moderator's `delete` through `channel->moderate`) inherit it. A message
/// author's own `edit` and `delete` derive from the message's `author`
/// relation, so they subtract the channel's `outcast` explicitly
/// (`channel->outcast`): an author banned from a room loses control over the
/// messages they wrote there, while the room's moderators can still remove
/// them.
///
///
/// Host-grant projection marker: `waddle_grant_projection` holds one marker
/// relationship per enrolled scope lifecycle and confers no permission. The
/// host-grant authority writes it in the same request as the relationships
/// it describes (`permissions::host_grant::projection`).
///
/// [`AUTHZ_SCHEMA_VERSION`]: super::authz_schema_version::AUTHZ_SCHEMA_VERSION
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

  permission delete = (parent->admin) - outcast
  permission manage = (owner + admin + manager + parent->admin) - outcast
  permission moderate = (owner + admin + moderator + manager + parent->moderator + parent->admin) - outcast
  permission send_message = (owner + admin + member + writer + moderator + manager + parent->view) - outcast
  permission read = (owner + admin + member + viewer + writer + moderator + manager + parent->view) - outcast
  permission view = (owner + admin + member + viewer + writer + moderator + manager + parent->view) - outcast
}

definition message {
  relation channel: channel
  relation author: user

  permission read = channel->read
  permission edit = author - channel->outcast
  permission delete = (author - channel->outcast) + channel->moderate
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

definition extension {}

definition tenant_credential {
  relation tenant_admin: user
  relation allowed_extension: extension
  relation revoked: user

  permission use = (allowed_extension + tenant_admin) - revoked
  permission manage = tenant_admin
}

definition user_credential {
  relation owner: user
  relation consented_extension: extension

  permission use = owner
  permission invoke_with_consent = owner + consented_extension
  permission manage = owner
}

definition installed_extension {
  relation space: space
  relation installer: user

  permission invoke = space->member + installer
}

definition room_inference_policy {
  relation channel: channel
  relation tenant_funded_allowed: user
  relation member_funded_allowed: user
  relation disabled: user

  permission allow_tenant_funded = (tenant_funded_allowed + channel->read) - disabled - channel->outcast
  permission allow_member_funded = (member_funded_allowed + channel->send_message) - disabled - channel->outcast
}

definition waddle_grant_projection {
  relation marker: waddle_grant_projection
}

definition waddle_authz_codec {
  relation codec: waddle_authz_codec
}

definition waddle_authz_schema {
  relation version_3: waddle_authz_schema
}
"#;

/// The name of every definition in [`SPICEDB_SCHEMA`], in schema order.
pub(crate) fn spicedb_definition_names() -> impl Iterator<Item = &'static str> {
    SPICEDB_SCHEMA.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("definition ")?
            .split(|c: char| c == '{' || c.is_whitespace())
            .next()
            .filter(|name| !name.is_empty())
    })
}

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

/// Schema for Channel objects. Mirrors the `channel` definition of
/// [`SPICEDB_SCHEMA`], including outcast precedence on every permission.
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
            without_outcast(ComputedPermission::arrow("parent", "admin")),
        )
        .with_permission(
            "manage",
            without_outcast(ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "admin"),
            ])),
        )
        .with_permission(
            "moderate",
            without_outcast(ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "moderator"),
                ComputedPermission::arrow("parent", "admin"),
            ])),
        )
        .with_permission(
            "send_message",
            without_outcast(ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("member"),
                ComputedPermission::direct("writer"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "view"),
            ])),
        )
        .with_permission(
            "read",
            without_outcast(ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("member"),
                ComputedPermission::direct("viewer"),
                ComputedPermission::direct("writer"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "view"),
            ])),
        )
        .with_permission(
            "view",
            without_outcast(ComputedPermission::union(vec![
                ComputedPermission::direct("owner"),
                ComputedPermission::direct("admin"),
                ComputedPermission::direct("member"),
                ComputedPermission::direct("viewer"),
                ComputedPermission::direct("writer"),
                ComputedPermission::direct("moderator"),
                ComputedPermission::direct("manager"),
                ComputedPermission::arrow("parent", "view"),
            ])),
        )
}

/// A channel permission that a channel `outcast` never holds.
fn without_outcast(granted: ComputedPermission) -> ComputedPermission {
    ComputedPermission::exclusion(granted, ComputedPermission::direct("outcast"))
}

/// Schema for Message objects. The author's own `edit` and `delete` subtract
/// a ban in the message's channel; see [`SPICEDB_SCHEMA`].
fn message_schema() -> ObjectTypeSchema {
    ObjectTypeSchema::new()
        // Relations
        .with_relation("channel")
        .with_relation("author")
        // Permissions
        .with_permission(
            "delete",
            ComputedPermission::union(vec![
                author_unless_outcast(),
                ComputedPermission::arrow("channel", "moderate"),
            ]),
        )
        .with_permission("edit", author_unless_outcast())
        .with_permission("react", ComputedPermission::arrow("channel", "view"))
        .with_permission("read", ComputedPermission::arrow("channel", "view"))
}

/// A message's author, unless they are an outcast of the message's channel.
fn author_unless_outcast() -> ComputedPermission {
    ComputedPermission::exclusion(
        ComputedPermission::direct("author"),
        ComputedPermission::arrow("channel", "outcast"),
    )
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
