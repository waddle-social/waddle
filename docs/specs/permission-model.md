# Permission Schema Specification

## Overview

This document specifies the Zanzibar-inspired relationship-based access control (ReBAC) model used in Waddle Social.

## Core Concepts

### Tuple

The fundamental unit of authorization:

```
<object>#<relation>@<subject>
```

Examples:
```
waddle:penguin-club#owner@user:did:plc:alice
waddle:penguin-club#member@user:did:plc:bob
channel:general#parent@waddle:penguin-club
```

### Object

Resources that are protected:
- `waddle:<id>` - Community
- `channel:<id>` - Channel
- `message:<id>` - Message
- `dm:<id>` - Direct message conversation
- `role:<id>` - Role definition

### Subject

Entities that access objects:
- `user:<did>` - User by DID
- `waddle:<id>#<relation>` - All users with relation to waddle
- `role:<id>#member` - All users with role

### Relation

Named connections between objects and subjects:
- Ownership: `owner`
- Membership: `member`, `admin`, `moderator`
- Access: `viewer`, `editor`, `manager`
- Hierarchy: `parent`

## Schema Definition

### Waddle Relations

```yaml
type waddle:
  relations:
    owner: user
    admin: user | waddle#owner
    moderator: user | waddle#admin
    member: user | waddle#moderator

  permissions:
    delete: owner
    manage_settings: admin
    manage_roles: admin
    manage_members: admin | moderator
    create_channel: admin
    view: member
```

### Channel Relations

```yaml
type channel:
  relations:
    parent: waddle
    manager: user | waddle#admin
    moderator: user | waddle#moderator | channel#manager
    writer: user | waddle#member | channel#moderator
    viewer: user | channel#writer

  permissions:
    delete: parent->admin
    manage: manager
    moderate: moderator
    send_message: writer
    read: viewer
    view: viewer
```

### Message Relations

```yaml
type message:
  relations:
    channel: channel
    author: user

  permissions:
    delete: author | channel->moderator
    edit: author
    react: channel->viewer
    read: channel->viewer
```

### DM Relations

```yaml
type dm:
  relations:
    participant: user
    owner: user  # for group DMs

  permissions:
    read: participant
    send: participant
    manage: owner
    add_participant: owner
    leave: participant
```

### Role Relations

```yaml
type role:
  relations:
    waddle: waddle
    member: user
    manager: user | waddle#admin

  permissions:
    assign: manager
    delete: manager
    edit: manager
```

## Database Schema

### Tuples Table

```sql
CREATE TABLE permission_tuples (
    id TEXT PRIMARY KEY,
    object_type TEXT NOT NULL,      -- waddle, channel, message, etc.
    object_id TEXT NOT NULL,
    relation TEXT NOT NULL,
    subject_type TEXT NOT NULL,     -- user, waddle, role
    subject_id TEXT NOT NULL,
    subject_relation TEXT,          -- for set-based subjects
    created_at TEXT NOT NULL,

    UNIQUE(object_type, object_id, relation, subject_type, subject_id, subject_relation)
);

CREATE INDEX idx_tuples_object ON permission_tuples(object_type, object_id);
CREATE INDEX idx_tuples_subject ON permission_tuples(subject_type, subject_id);
CREATE INDEX idx_tuples_relation ON permission_tuples(object_type, relation);
```

### Example Tuples

```sql
-- Alice owns penguin-club
INSERT INTO permission_tuples VALUES (
    'tuple_1', 'waddle', 'penguin-club', 'owner',
    'user', 'did:plc:alice', NULL, '2024-01-15T10:00:00Z'
);

-- Bob is a member of penguin-club
INSERT INTO permission_tuples VALUES (
    'tuple_2', 'waddle', 'penguin-club', 'member',
    'user', 'did:plc:bob', NULL, '2024-01-15T10:00:00Z'
);

-- Channel general belongs to penguin-club
INSERT INTO permission_tuples VALUES (
    'tuple_3', 'channel', 'general', 'parent',
    'waddle', 'penguin-club', NULL, '2024-01-15T10:00:00Z'
);

-- All penguin-club members can view general
INSERT INTO permission_tuples VALUES (
    'tuple_4', 'channel', 'general', 'viewer',
    'waddle', 'penguin-club', 'member', '2024-01-15T10:00:00Z'
);
```

## Permission Check Algorithm

### Check Request

```rust
struct CheckRequest {
    subject: Subject,      // Who is asking
    permission: String,    // What permission
    object: Object,        // On what resource
}

struct CheckResponse {
    allowed: bool,
    reason: Option<String>,
}
```

### Algorithm

```rust
fn check(req: CheckRequest) -> CheckResponse {
    // 1. Direct relation check
    if has_tuple(req.object, req.permission, req.subject) {
        return CheckResponse { allowed: true, reason: Some("direct") };
    }

    // 2. Computed permission check (traverse schema)
    for computed in schema.get_computed_permissions(req.object.type, req.permission) {
        match computed {
            // Union: permission if any relation matches
            ComputedPermission::Union(relations) => {
                for relation in relations {
                    if check_relation(req.object, relation, req.subject) {
                        return CheckResponse { allowed: true, reason: Some(relation) };
                    }
                }
            }
            // Intersection: permission if all relations match
            ComputedPermission::Intersection(relations) => {
                if relations.iter().all(|r| check_relation(req.object, r, req.subject)) {
                    return CheckResponse { allowed: true, reason: Some("intersection") };
                }
            }
            // Arrow: follow relation to another object
            ComputedPermission::Arrow(relation, target_permission) => {
                let parents = get_subjects(req.object, relation);
                for parent in parents {
                    if check(CheckRequest {
                        subject: req.subject,
                        permission: target_permission,
                        object: parent,
                    }).allowed {
                        return CheckResponse { allowed: true, reason: Some("inherited") };
                    }
                }
            }
        }
    }

    CheckResponse { allowed: false, reason: None }
}
```

### Optimization: Caching

```rust
struct PermissionCache {
    cache: LruCache<(Subject, Permission, Object), bool>,
    ttl: Duration,
}

impl PermissionCache {
    fn check_with_cache(&mut self, req: CheckRequest) -> CheckResponse {
        let key = (req.subject.clone(), req.permission.clone(), req.object.clone());

        if let Some(cached) = self.cache.get(&key) {
            return CheckResponse { allowed: *cached, reason: Some("cached") };
        }

        let result = check(req);
        self.cache.put(key, result.allowed);
        result
    }
}
```

## API

### Check Permission

```http
POST /v1/permissions/check HTTP/1.1

{
  "subject": "user:did:plc:bob",
  "permission": "send_message",
  "object": "channel:general"
}
```

Response:
```json
{
  "allowed": true,
  "reason": "member of parent waddle"
}
```

### List Permissions

```http
GET /v1/permissions/list?subject=user:did:plc:bob&object=channel:general HTTP/1.1
```

Response:
```json
{
  "permissions": ["read", "send_message", "react"],
  "relations": ["viewer", "writer"]
}
```

### Write Tuple

```http
POST /v1/permissions/tuples HTTP/1.1

{
  "object": "waddle:penguin-club",
  "relation": "member",
  "subject": "user:did:plc:charlie"
}
```

### Delete Tuple

```http
DELETE /v1/permissions/tuples HTTP/1.1

{
  "object": "waddle:penguin-club",
  "relation": "member",
  "subject": "user:did:plc:charlie"
}
```

## Permission Definitions by Resource

### Waddle Permissions

| Permission | Description | Granted By |
|------------|-------------|------------|
| `waddle:delete` | Delete the waddle | owner |
| `waddle:manage_settings` | Edit waddle settings | admin |
| `waddle:manage_roles` | Create/edit/delete roles | admin |
| `waddle:manage_members` | Kick/ban members | admin, moderator |
| `waddle:create_channel` | Create new channels | admin |
| `waddle:view` | See waddle exists | member |

### Channel Permissions

| Permission | Description | Granted By |
|------------|-------------|------------|
| `channel:delete` | Delete channel | waddle admin |
| `channel:manage` | Edit channel settings | manager |
| `channel:moderate` | Delete messages, timeout | moderator |
| `channel:send_message` | Post messages | writer |
| `channel:read` | Read message history | viewer |
| `channel:mention_everyone` | Use @everyone | moderator |

### Message Permissions

| Permission | Description | Granted By |
|------------|-------------|------------|
| `message:delete` | Delete message | author, channel moderator |
| `message:edit` | Edit message | author |
| `message:react` | Add reactions | channel viewer |
| `message:pin` | Pin message | channel moderator |

## Related

- [ADR-0009: Zanzibar Permissions](../adrs/0009-zanzibar-permissions.md)
- [RFC-0001: Waddles](../rfcs/0001-waddles.md)
- [RFC-0013: Moderation](../rfcs/0013-moderation.md)
