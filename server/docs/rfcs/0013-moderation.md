# RFC-0013: Moderation System

## Summary

The moderation system provides tools for community management, including roles, permissions, member actions, and AI-assisted content detection.

## Motivation

Healthy communities require:
- Clear roles and responsibilities
- Tools to address harmful content
- Transparent enforcement actions
- Scalable moderation as communities grow

## Detailed Design

### Moderation Roles

Default hierarchy (customizable):

```
Owner
└── Admin
    └── Moderator
        └── Member
```

**Permissions per role**:

| Action | Owner | Admin | Moderator | Member |
|--------|-------|-------|-----------|--------|
| Delete Waddle | ✓ | | | |
| Manage roles | ✓ | ✓ | | |
| Ban members | ✓ | ✓ | | |
| Kick members | ✓ | ✓ | ✓ | |
| Timeout members | ✓ | ✓ | ✓ | |
| Delete messages | ✓ | ✓ | ✓ | |
| Pin messages | ✓ | ✓ | ✓ | |
| Manage channels | ✓ | ✓ | | |
| View audit log | ✓ | ✓ | ✓ | |

### Member Actions

**Timeout** (temporary restriction):
```
Timeout
├── user_did: DID
├── waddle_id: UUID
├── channel_id: UUID (optional, channel-specific)
├── duration: Duration
├── reason: String
├── issued_by: DID
├── issued_at: Timestamp
└── expires_at: Timestamp
```

During timeout, user cannot:
- Send messages
- Add reactions
- Join voice (future)

**Kick** (removal with rejoin allowed):
```
Kick
├── user_did: DID
├── waddle_id: UUID
├── reason: String
├── issued_by: DID
└── issued_at: Timestamp
```

**Ban** (permanent removal):
```
Ban
├── user_did: DID
├── waddle_id: UUID
├── reason: String
├── issued_by: DID
├── issued_at: Timestamp
├── expires_at: Timestamp (optional, for temp bans)
└── delete_messages: Boolean (optional)
```

### Content Moderation

**Manual actions**:
- Delete message
- Delete and warn user
- Delete with timeout
- Delete with ban

**Bulk moderation**:
- Delete last N messages from user
- Purge messages matching pattern

### AI-Assisted Moderation

See [RFC-0007: AI Features](./0007-ai-integrations.md) for AI moderation details.

Integration:
1. Message flagged by AI
2. Added to moderation queue
3. Human moderator reviews
4. Action taken (or dismissed)

**Moderation Queue**:
```
ModerationItem
├── id: UUID
├── type: "ai_flag" | "user_report"
├── content_type: "message" | "user" | "media"
├── content_id: UUID
├── reason: String
├── confidence: Float (for AI)
├── reporter_did: DID (for reports)
├── status: "pending" | "actioned" | "dismissed"
├── reviewed_by: DID (optional)
└── created_at: Timestamp
```

### User Reports

Members can report content:

```
Report
├── id: UUID
├── reporter_did: DID
├── reported_did: DID
├── content_type: "message" | "user" | "waddle"
├── content_id: UUID
├── reason: ReportReason
├── details: String (optional)
├── status: "pending" | "reviewed"
└── created_at: Timestamp

ReportReason
├── harassment
├── spam
├── hate_speech
├── violence
├── sexual_content
├── misinformation
├── other
```

### Audit Log

All moderation actions logged:

```
AuditEntry
├── id: UUID
├── waddle_id: UUID
├── actor_did: DID
├── action: AuditAction
├── target_type: String
├── target_id: String
├── details: JSON
├── reason: String (optional)
└── timestamp: Timestamp
```

Retention: 90 days (configurable)

### Automod Rules

Configurable automatic actions:

```
AutomodRule
├── id: UUID
├── waddle_id: UUID
├── name: String
├── trigger: AutomodTrigger
├── action: AutomodAction
├── enabled: Boolean
└── exemptions: Exemption[]

AutomodTrigger
├── type: "keyword" | "regex" | "link" | "spam" | "caps"
├── pattern: String
└── threshold: Integer (for spam/caps)

AutomodAction
├── action: "delete" | "flag" | "timeout" | "warn"
├── duration: Duration (for timeout)
└── notify_mods: Boolean
```

### Appeals

Banned users can appeal:

```
Appeal
├── id: UUID
├── ban_id: UUID
├── user_did: DID
├── message: String
├── status: "pending" | "accepted" | "denied"
├── reviewed_by: DID (optional)
├── response: String (optional)
└── created_at: Timestamp
```

## API Endpoints

```
# Member actions
POST   /waddles/:id/members/:did/timeout   Timeout user
DELETE /waddles/:id/members/:did/timeout   Remove timeout
POST   /waddles/:id/members/:did/kick      Kick user
POST   /waddles/:id/members/:did/ban       Ban user
DELETE /waddles/:id/members/:did/ban       Unban user

# Message moderation
DELETE /messages/:id                       Delete message
POST   /messages/:id/report                Report message

# Moderation queue
GET    /waddles/:id/moderation/queue       Get pending items
POST   /moderation/:id/action              Take action
POST   /moderation/:id/dismiss             Dismiss item

# Automod
GET    /waddles/:id/automod                List rules
POST   /waddles/:id/automod                Create rule
PATCH  /automod/:id                        Update rule
DELETE /automod/:id                        Delete rule

# Audit
GET    /waddles/:id/audit                  Get audit log

# Appeals
POST   /bans/:id/appeal                    Submit appeal
GET    /waddles/:id/appeals                List appeals
POST   /appeals/:id/review                 Review appeal
```

## Related

- [RFC-0001: Waddles](./0001-waddles.md)
- [RFC-0007: AI Features](./0007-ai-integrations.md)
- [ADR-0009: Zanzibar Permissions](../adrs/0009-zanzibar-permissions.md)
- [ADR-0012: Transport Encryption](../adrs/0012-transport-encryption.md)
