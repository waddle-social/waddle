# RFC-0001: Waddles (Communities)

## Summary

Waddles are the primary community structure in Waddle Social, analogous to Discord servers or Slack workspaces. A Waddle is a collection of channels, members, and roles that form a community around shared interests.

## Motivation

Users need a way to:
- Create and manage communities around shared interests
- Organize conversations into topical channels
- Control membership and permissions
- Build identity within communities

## Detailed Design

### Waddle Structure

```
Waddle
├── Metadata
│   ├── id: UUID
│   ├── name: String (2-100 chars)
│   ├── description: String (optional, max 1000 chars)
│   ├── icon: URL (optional)
│   ├── banner: URL (optional)
│   ├── created_at: Timestamp
│   └── owner_did: DID
├── Channels[]
├── Roles[]
└── Members[]
```

### Creating a Waddle

1. Authenticated user submits creation request
2. System validates name uniqueness (case-insensitive)
3. Waddle created with user as owner
4. Default channels created: `#general`, `#announcements`
5. Default roles created: `@everyone`, `@admin`, `@moderator`

### Membership

- **Join Methods**:
  - Invite link (time-limited or permanent)
  - Direct invite from existing member
  - Public discovery (if enabled)
  - Request to join (approval required)

- **Member States**:
  - `active`: Full access per role permissions
  - `pending`: Awaiting approval
  - `banned`: Cannot rejoin without unban

### Roles and Permissions

See [ADR-0009: Zanzibar Permissions](../adrs/0009-zanzibar-permissions.md) for the authorization model.

Default roles:
- `@everyone`: Base permissions for all members
- `@moderator`: Can manage messages, mute users
- `@admin`: Can manage channels, roles, members

Custom roles can be created with granular permissions.

### Discovery

Waddles can be:
- **Private**: Invite-only, not listed
- **Public**: Listed in discovery, anyone can join
- **Restricted**: Listed, but requires approval

### Limits

| Resource | Limit |
|----------|-------|
| Members per Waddle | 100,000 |
| Channels per Waddle | 500 |
| Roles per Waddle | 250 |
| Waddles per User | 100 |

## API Endpoints

```
POST   /waddles                    Create Waddle
GET    /waddles/:id                Get Waddle details
PATCH  /waddles/:id                Update Waddle
DELETE /waddles/:id                Delete Waddle
GET    /waddles/:id/members        List members
POST   /waddles/:id/members        Add member
DELETE /waddles/:id/members/:did   Remove member
GET    /waddles/:id/invites        List invites
POST   /waddles/:id/invites        Create invite
GET    /waddles/discover           Public Waddle discovery
```

## WebSocket Events

- `waddle.updated`: Waddle metadata changed
- `waddle.deleted`: Waddle was deleted
- `waddle.member.joined`: New member joined
- `waddle.member.left`: Member left
- `waddle.member.banned`: Member was banned

## Related

- [RFC-0002: Channels](./0002-channels.md)
- [RFC-0013: Moderation](./0013-moderation.md)
- [ADR-0009: Zanzibar Permissions](../adrs/0009-zanzibar-permissions.md)
- [Spec: Permission Model](../specs/permission-model.md)
