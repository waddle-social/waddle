# RFC-0011: Bluesky Announcements

## Summary

Waddles can broadcast announcements to Bluesky via ATProto, allowing community updates to reach followers on the broader social network.

## Motivation

Communities want to:
- Announce events, updates, and news publicly
- Reach members who follow on Bluesky
- Cross-post important content
- Maintain presence on ATProto ecosystem

## Detailed Design

### Bluesky Integration

Since users authenticate via ATProto (see [ADR-0005](../adrs/0005-atproto-identity.md)), Waddle can:
- Post to the user's PDS on their behalf
- Require explicit authorization per action
- Use the Waddle's linked Bluesky account

### Waddle Bluesky Account

Waddles can link a Bluesky account:

```
WaddleBlueskyConfig
├── linked_did: DID
├── handle: String
├── authorized_by: DID (Waddle admin who linked)
├── post_permissions: PostPermission
├── default_labels: String[] (content labels)
└── footer_text: String (optional, "via Waddle")
```

### Announcement Channel

Designate a channel as the announcement source:

```
ChannelSettings
├── ...
├── bluesky_broadcast: Boolean
├── broadcast_filter: "all" | "pinned_only" | "tagged"
└── broadcast_tag: String (e.g., "#announce")
```

### Broadcast Flow

1. **Message posted** to announcement channel
2. **Filter applied** (pinned, tagged, or all)
3. **Authorization check** (poster has broadcast permission)
4. **Content transformed** for Bluesky:
   - Markdown converted to Bluesky rich text
   - Attachments uploaded to Bluesky's blob store
   - Mentions mapped to Bluesky handles
   - Links converted to facets
5. **Posted to PDS** via ATProto API
6. **Reference stored** for tracking

### Content Transformation

**Waddle → Bluesky mapping**:

| Waddle | Bluesky |
|--------|---------|
| Markdown bold | No equivalent (stripped) |
| Links | Link facets |
| @mentions | Mention facets (if Bluesky user) |
| Images | Blob uploads |
| Videos | External embed link |

**Length handling**:
- Bluesky limit: 300 graphemes
- Long messages truncated with "... [Read more on Waddle]"
- Link to full message in Waddle (if public)

### Broadcast Record

```
BroadcastRecord
├── id: UUID
├── waddle_id: UUID
├── message_id: UUID
├── bluesky_uri: ATProto URI
├── bluesky_cid: CID
├── status: "pending" | "posted" | "failed" | "deleted"
├── error: String (if failed)
└── posted_at: Timestamp
```

### Permissions

New Waddle permissions:
- `broadcast_to_bluesky`: Can trigger broadcast
- `manage_bluesky_config`: Can link/unlink account

### Privacy Considerations

- Only designated channels broadcast
- Users opt-in by posting to announcement channel
- Waddle admins control broadcast settings
- Members can see broadcast status

### Moderation

Bluesky's moderation applies:
- Posts must comply with Bluesky ToS
- Community labels can be applied
- Reports handled via Bluesky's system

### Two-Way Sync (Future)

Potential future feature:
- Import Bluesky replies into Waddle
- Show engagement metrics
- Cross-platform threads

## API Endpoints

```
GET    /waddles/:id/bluesky           Get Bluesky config
PUT    /waddles/:id/bluesky           Link Bluesky account
DELETE /waddles/:id/bluesky           Unlink account
POST   /messages/:id/broadcast        Manual broadcast trigger
GET    /messages/:id/broadcast        Get broadcast status
DELETE /messages/:id/broadcast        Delete Bluesky post
```

## Configuration

```json
{
  "waddle_id": "...",
  "bluesky": {
    "linked_did": "did:plc:abc123",
    "handle": "mywaddle.bsky.social",
    "default_labels": [],
    "footer_text": "\n\n📢 via Waddle",
    "channels": {
      "announcements": {
        "broadcast": true,
        "filter": "pinned_only"
      }
    }
  }
}
```

## Error Handling

- **Rate limiting**: Queue and retry with backoff
- **Auth failure**: Notify admin, pause broadcasts
- **Content rejection**: Log error, notify poster
- **PDS unavailable**: Queue for retry

## Related

- [ADR-0005: ATProto Identity](../adrs/0005-atproto-identity.md)
- [RFC-0001: Waddles](./0001-waddles.md)
- [RFC-0002: Channels](./0002-channels.md)
- [Spec: ATProto Integration](../specs/atproto-integration.md)
