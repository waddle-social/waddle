# ADR-0009: Zanzibar-Inspired Authorization

## Status

Accepted

## Context

Waddle Social requires fine-grained permissions for:
- Waddle (community) membership and roles
- Channel read/write/manage permissions
- Direct message access
- Administrative actions (ban, kick, mute)

We evaluated authorization models:
- **RBAC (Role-Based)**: Roles grant permissions; simple but inflexible
- **ABAC (Attribute-Based)**: Policy rules on attributes; complex to reason about
- **ACL (Access Control Lists)**: Per-resource lists; doesn't scale well
- **ReBAC (Relationship-Based)**: Permissions derived from relationships; flexible, scalable

Google's Zanzibar paper describes a ReBAC system used for Google Drive, YouTube, etc.

## Decision

We will implement **Zanzibar-inspired relationship-based access control (ReBAC)**.

## Consequences

### Positive

- **Intuitive Model**: "User X is member of Waddle Y" → permissions derived naturally
- **Hierarchical**: Permissions can inherit (Waddle admin → channel admin)
- **Scalable**: Relationship tuples stored efficiently, check operations are fast
- **Flexible**: New permission types added by defining new relations
- **Consistent**: Single authorization model across all resources

### Negative

- **Implementation Complexity**: Building a Zanzibar-like system is non-trivial
- **Query Performance**: Deep relationship traversal can be slow without optimization
- **Learning Curve**: ReBAC is less familiar than RBAC to most developers

### Neutral

- **No External Service**: Implementing in-process (not using SpiceDB/OpenFGA) for simplicity

## Implementation Notes

Core concepts:
- **Tuple**: `(user:alice, member, waddle:penguin-club)`
- **Relation**: `member`, `admin`, `moderator`, `viewer`
- **Check**: "Can alice read channel:general?" → traverse relationships

Example relations:
```
waddle:penguin-club#admin@user:alice
waddle:penguin-club#member@user:bob
channel:general#parent@waddle:penguin-club
channel:general#viewer@waddle:penguin-club#member
```

## Related

- [RFC-0001: Waddles](../rfcs/0001-waddles.md)
- [RFC-0002: Channels](../rfcs/0002-channels.md)
- [RFC-0013: Moderation](../rfcs/0013-moderation.md)
- [Spec: Permission Model](../specs/permission-model.md)
