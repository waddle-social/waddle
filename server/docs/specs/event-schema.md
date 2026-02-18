# Event Types Specification

## Overview

This document defines the event types used in Waddle Social's CQRS architecture, including payloads and versioning.

## Event Structure

### Base Event

```rust
struct Event {
    id: Uuid,                    // Unique event ID
    event_type: String,          // Event type name
    version: u32,                // Schema version
    aggregate_id: String,        // Entity this event belongs to
    aggregate_type: String,      // Entity type
    payload: serde_json::Value,  // Event-specific data
    metadata: EventMetadata,
    created_at: DateTime<Utc>,
}

struct EventMetadata {
    actor_did: Option<String>,   // User who triggered event
    correlation_id: Option<Uuid>, // Request correlation
    causation_id: Option<Uuid>,   // Parent event ID
    trace_id: Option<String>,     // Distributed tracing
}
```

### JSON Format

```json
{
  "id": "evt_01912345-6789-7abc-def0-123456789abc",
  "event_type": "MessageCreated",
  "version": 1,
  "aggregate_id": "ch_general",
  "aggregate_type": "channel",
  "payload": { ... },
  "metadata": {
    "actor_did": "did:plc:alice",
    "correlation_id": "req_xyz",
    "trace_id": "trace_abc"
  },
  "created_at": "2024-01-15T10:30:00.000Z"
}
```

## Event Categories

### Message Events

#### MessageCreated (v1)

```json
{
  "event_type": "MessageCreated",
  "version": 1,
  "aggregate_type": "channel",
  "payload": {
    "message_id": "msg_123",
    "channel_id": "ch_general",
    "author_did": "did:plc:alice",
    "content": "Hello world!",
    "attachments": [],
    "reply_to_id": null,
    "thread_id": null,
    "flags": 0,
    "mentions": []
  }
}
```

#### MessageUpdated (v1)

```json
{
  "event_type": "MessageUpdated",
  "version": 1,
  "aggregate_type": "channel",
  "payload": {
    "message_id": "msg_123",
    "channel_id": "ch_general",
    "content": "Hello world! (edited)",
    "edited_at": "2024-01-15T10:35:00.000Z"
  }
}
```

#### MessageDeleted (v1)

```json
{
  "event_type": "MessageDeleted",
  "version": 1,
  "aggregate_type": "channel",
  "payload": {
    "message_id": "msg_123",
    "channel_id": "ch_general",
    "deleted_by": "did:plc:alice",
    "reason": "user_deleted"  // user_deleted, ttl_expired, moderation
  }
}
```

#### ReactionAdded (v1)

```json
{
  "event_type": "ReactionAdded",
  "version": 1,
  "aggregate_type": "message",
  "payload": {
    "message_id": "msg_123",
    "user_did": "did:plc:bob",
    "emoji": "🎉"
  }
}
```

#### ReactionRemoved (v1)

```json
{
  "event_type": "ReactionRemoved",
  "version": 1,
  "aggregate_type": "message",
  "payload": {
    "message_id": "msg_123",
    "user_did": "did:plc:bob",
    "emoji": "🎉"
  }
}
```

### Channel Events

#### ChannelCreated (v1)

```json
{
  "event_type": "ChannelCreated",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "channel_id": "ch_announcements",
    "waddle_id": "waddle_123",
    "name": "announcements",
    "type": "text",
    "topic": "",
    "position": 1,
    "category_id": null,
    "settings": {
      "message_ttl": null,
      "slowmode_interval": null
    }
  }
}
```

#### ChannelUpdated (v1)

```json
{
  "event_type": "ChannelUpdated",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "channel_id": "ch_announcements",
    "changes": {
      "topic": "Important updates only",
      "settings": {
        "message_ttl": "P7D"
      }
    }
  }
}
```

#### ChannelDeleted (v1)

```json
{
  "event_type": "ChannelDeleted",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "channel_id": "ch_old",
    "waddle_id": "waddle_123"
  }
}
```

### Waddle Events

#### WaddleCreated (v1)

```json
{
  "event_type": "WaddleCreated",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "waddle_id": "waddle_123",
    "name": "Penguin Club",
    "description": "For penguin enthusiasts",
    "owner_did": "did:plc:alice",
    "visibility": "public"
  }
}
```

#### WaddleUpdated (v1)

```json
{
  "event_type": "WaddleUpdated",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "waddle_id": "waddle_123",
    "changes": {
      "description": "Updated description",
      "icon_url": "https://..."
    }
  }
}
```

#### MemberJoined (v1)

```json
{
  "event_type": "MemberJoined",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "waddle_id": "waddle_123",
    "user_did": "did:plc:bob",
    "invited_by": "did:plc:alice",
    "invite_code": "abc123"
  }
}
```

#### MemberLeft (v1)

```json
{
  "event_type": "MemberLeft",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "waddle_id": "waddle_123",
    "user_did": "did:plc:bob",
    "reason": "left"  // left, kicked, banned
  }
}
```

#### MemberRoleChanged (v1)

```json
{
  "event_type": "MemberRoleChanged",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "waddle_id": "waddle_123",
    "user_did": "did:plc:bob",
    "added_roles": ["role_moderator"],
    "removed_roles": []
  }
}
```

### Presence Events

#### PresenceUpdated (v1)

```json
{
  "event_type": "PresenceUpdated",
  "version": 1,
  "aggregate_type": "user",
  "payload": {
    "user_did": "did:plc:alice",
    "status": "online",
    "status_text": "Working on Waddle!",
    "status_emoji": "🐧"
  }
}
```

#### TypingStarted (v1)

```json
{
  "event_type": "TypingStarted",
  "version": 1,
  "aggregate_type": "channel",
  "payload": {
    "channel_id": "ch_general",
    "user_did": "did:plc:alice",
    "expires_at": "2024-01-15T10:30:10.000Z"
  }
}
```

### Moderation Events

#### MemberTimedOut (v1)

```json
{
  "event_type": "MemberTimedOut",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "waddle_id": "waddle_123",
    "user_did": "did:plc:bob",
    "moderator_did": "did:plc:alice",
    "duration_seconds": 3600,
    "reason": "Spam"
  }
}
```

#### MemberBanned (v1)

```json
{
  "event_type": "MemberBanned",
  "version": 1,
  "aggregate_type": "waddle",
  "payload": {
    "waddle_id": "waddle_123",
    "user_did": "did:plc:bob",
    "moderator_did": "did:plc:alice",
    "reason": "Repeated violations",
    "delete_messages": true
  }
}
```

## Event Versioning

### Version Strategy

- Events are immutable; never modify existing versions
- New version for structural changes
- Upcasters convert old versions to new

### Upcaster Example

```rust
fn upcast_message_created_v1_to_v2(v1: MessageCreatedV1) -> MessageCreatedV2 {
    MessageCreatedV2 {
        message_id: v1.message_id,
        channel_id: v1.channel_id,
        author_did: v1.author_did,
        content: v1.content,
        attachments: v1.attachments,
        reply_to_id: v1.reply_to_id,
        thread_id: v1.thread_id,
        flags: v1.flags,
        mentions: v1.mentions,
        // New field with default
        nonce: None,
    }
}
```

## Event Storage

### Database Schema

```sql
CREATE TABLE events (
    id TEXT PRIMARY KEY,
    event_type TEXT NOT NULL,
    version INTEGER NOT NULL,
    aggregate_id TEXT NOT NULL,
    aggregate_type TEXT NOT NULL,
    payload TEXT NOT NULL,              -- JSON
    metadata TEXT NOT NULL,             -- JSON
    created_at TEXT NOT NULL,
    sequence_number INTEGER NOT NULL    -- Global ordering
);

CREATE INDEX idx_events_aggregate ON events(aggregate_type, aggregate_id, sequence_number);
CREATE INDEX idx_events_type ON events(event_type, created_at);
CREATE INDEX idx_events_created ON events(created_at);
```

### Retention

- Events retained for 90 days by default
- Compaction: Summarize old events into snapshots
- Configurable per deployment

## Event Publishing

### In-Process

```rust
// Event bus for in-process subscribers
let bus = EventBus::new();

bus.subscribe("MessageCreated", |event| async {
    // Update read model
    update_channel_messages(event).await;
});

bus.subscribe("MessageCreated", |event| async {
    // Update search index
    index_message(event).await;
});

bus.publish(event).await;
```

### External (Future)

For distributed deployments:
- NATS JetStream
- Apache Kafka
- Redis Streams

## Related

- [ADR-0007: CQRS Architecture](../adrs/0007-cqrs-architecture.md)
- [ADR-0008: Kameo Actors](../adrs/0008-kameo-actors.md)
- [Spec: Message Schema](./message-schema.md)
- [Spec: XMPP Integration](./xmpp-integration.md)
