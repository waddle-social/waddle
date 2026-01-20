# Message Data Schema Specification

## Overview

This document defines the database schema and JSON format for messages in Waddle Social.

## Database Schema

### Messages Table

```sql
CREATE TABLE messages (
    id TEXT PRIMARY KEY,                    -- UUID v7 (time-sortable)
    channel_id TEXT NOT NULL,               -- FK to channels or dm_channels
    author_did TEXT NOT NULL,               -- ATProto DID
    content TEXT,                           -- Message text (max 4000 chars)
    reply_to_id TEXT,                       -- FK to parent message
    thread_id TEXT,                         -- FK to thread root message
    flags INTEGER DEFAULT 0,                -- Bitfield for message flags
    edited_at TEXT,                         -- ISO 8601 timestamp
    created_at TEXT NOT NULL,               -- ISO 8601 timestamp
    expires_at TEXT,                        -- TTL expiration timestamp

    FOREIGN KEY (channel_id) REFERENCES channels(id) ON DELETE CASCADE,
    FOREIGN KEY (reply_to_id) REFERENCES messages(id) ON DELETE SET NULL,
    FOREIGN KEY (thread_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_messages_channel_created ON messages(channel_id, created_at DESC);
CREATE INDEX idx_messages_thread ON messages(thread_id, created_at);
CREATE INDEX idx_messages_author ON messages(author_did, created_at DESC);
CREATE INDEX idx_messages_expires ON messages(expires_at) WHERE expires_at IS NOT NULL;
```

### Message Flags

```rust
bitflags! {
    pub struct MessageFlags: u32 {
        const PINNED           = 1 << 0;
        const SUPPRESS_EMBEDS  = 1 << 1;
        const EPHEMERAL        = 1 << 2;
        const URGENT           = 1 << 3;
        const SILENT           = 1 << 4;  // No notification
        const SYSTEM           = 1 << 5;  // System-generated
        const CROSSPOST        = 1 << 6;  // Announcement crosspost
    }
}
```

### Attachments Table

```sql
CREATE TABLE attachments (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    filename TEXT NOT NULL,
    content_type TEXT NOT NULL,
    size INTEGER NOT NULL,              -- Bytes
    url TEXT NOT NULL,                  -- S3 URL
    thumbnail_url TEXT,
    width INTEGER,                      -- For images/video
    height INTEGER,
    duration INTEGER,                   -- For audio/video (seconds)
    created_at TEXT NOT NULL,

    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_attachments_message ON attachments(message_id);
```

### Embeds Table

```sql
CREATE TABLE embeds (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    type TEXT NOT NULL,                 -- link, image, video, rich
    url TEXT NOT NULL,
    title TEXT,
    description TEXT,
    thumbnail_url TEXT,
    provider TEXT,
    author TEXT,
    color INTEGER,                      -- Hex color as integer

    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_embeds_message ON embeds(message_id);
```

### Mentions Table

```sql
CREATE TABLE mentions (
    id TEXT PRIMARY KEY,
    message_id TEXT NOT NULL,
    type TEXT NOT NULL,                 -- user, role, channel, everyone
    target_id TEXT NOT NULL,            -- DID, role_id, or channel_id

    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_mentions_message ON mentions(message_id);
CREATE INDEX idx_mentions_target ON mentions(target_id, type);
```

### Reactions Table

```sql
CREATE TABLE reactions (
    message_id TEXT NOT NULL,
    emoji TEXT NOT NULL,                -- Unicode or :custom_name:
    user_did TEXT NOT NULL,
    created_at TEXT NOT NULL,

    PRIMARY KEY (message_id, emoji, user_did),
    FOREIGN KEY (message_id) REFERENCES messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_reactions_message ON reactions(message_id);
```

## JSON Schema

### Message Object

```json
{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "type": "object",
  "properties": {
    "id": {
      "type": "string",
      "format": "uuid",
      "description": "Unique message identifier (UUID v7)"
    },
    "channel_id": {
      "type": "string",
      "format": "uuid",
      "description": "Channel or DM channel ID"
    },
    "author": {
      "$ref": "#/definitions/User",
      "description": "Message author"
    },
    "content": {
      "type": "string",
      "maxLength": 4000,
      "description": "Message text content"
    },
    "attachments": {
      "type": "array",
      "items": { "$ref": "#/definitions/Attachment" },
      "maxItems": 10
    },
    "embeds": {
      "type": "array",
      "items": { "$ref": "#/definitions/Embed" },
      "maxItems": 5
    },
    "mentions": {
      "type": "array",
      "items": { "$ref": "#/definitions/Mention" }
    },
    "reactions": {
      "type": "array",
      "items": { "$ref": "#/definitions/ReactionCount" }
    },
    "reply_to": {
      "oneOf": [
        { "$ref": "#/definitions/MessageReference" },
        { "type": "null" }
      ]
    },
    "thread_id": {
      "type": ["string", "null"],
      "format": "uuid"
    },
    "flags": {
      "type": "integer",
      "description": "Bitfield of message flags"
    },
    "pinned": {
      "type": "boolean"
    },
    "edited_at": {
      "type": ["string", "null"],
      "format": "date-time"
    },
    "created_at": {
      "type": "string",
      "format": "date-time"
    }
  },
  "required": ["id", "channel_id", "author", "created_at"],
  "definitions": {
    "User": {
      "type": "object",
      "properties": {
        "did": { "type": "string" },
        "handle": { "type": "string" },
        "display_name": { "type": "string" },
        "avatar": { "type": "string", "format": "uri" }
      },
      "required": ["did", "handle"]
    },
    "Attachment": {
      "type": "object",
      "properties": {
        "id": { "type": "string", "format": "uuid" },
        "filename": { "type": "string" },
        "content_type": { "type": "string" },
        "size": { "type": "integer" },
        "url": { "type": "string", "format": "uri" },
        "thumbnail_url": { "type": "string", "format": "uri" },
        "width": { "type": "integer" },
        "height": { "type": "integer" },
        "duration": { "type": "integer" }
      },
      "required": ["id", "filename", "content_type", "size", "url"]
    },
    "Embed": {
      "type": "object",
      "properties": {
        "type": { "enum": ["link", "image", "video", "rich"] },
        "url": { "type": "string", "format": "uri" },
        "title": { "type": "string" },
        "description": { "type": "string" },
        "thumbnail": { "type": "string", "format": "uri" },
        "provider": { "type": "string" },
        "author": { "type": "string" },
        "color": { "type": "integer" }
      },
      "required": ["type", "url"]
    },
    "Mention": {
      "type": "object",
      "properties": {
        "type": { "enum": ["user", "role", "channel", "everyone"] },
        "id": { "type": "string" },
        "display": { "type": "string" }
      },
      "required": ["type", "id"]
    },
    "ReactionCount": {
      "type": "object",
      "properties": {
        "emoji": { "type": "string" },
        "count": { "type": "integer" },
        "me": { "type": "boolean" }
      },
      "required": ["emoji", "count", "me"]
    },
    "MessageReference": {
      "type": "object",
      "properties": {
        "message_id": { "type": "string", "format": "uuid" },
        "channel_id": { "type": "string", "format": "uuid" },
        "author": { "$ref": "#/definitions/User" }
      },
      "required": ["message_id"]
    }
  }
}
```

### Example Message

```json
{
  "id": "01912345-6789-7abc-def0-123456789abc",
  "channel_id": "ch_general_123",
  "author": {
    "did": "did:plc:abcdef123456",
    "handle": "alice.bsky.social",
    "display_name": "Alice",
    "avatar": "https://cdn.waddle.social/avatars/abc.png"
  },
  "content": "Hey @bob, check out this **awesome** feature!\n\nHere's a code example:\n```rust\nfn main() {\n    println!(\"Hello, Waddle!\");\n}\n```",
  "attachments": [
    {
      "id": "att_123",
      "filename": "screenshot.png",
      "content_type": "image/png",
      "size": 245678,
      "url": "https://cdn.waddle.social/attachments/abc/screenshot.png",
      "thumbnail_url": "https://cdn.waddle.social/attachments/abc/screenshot_thumb.png",
      "width": 1920,
      "height": 1080
    }
  ],
  "embeds": [
    {
      "type": "link",
      "url": "https://github.com/waddle-social/wa",
      "title": "waddle-social/wa",
      "description": "Open source communication platform",
      "thumbnail": "https://opengraph.githubassets.com/...",
      "provider": "GitHub"
    }
  ],
  "mentions": [
    {
      "type": "user",
      "id": "did:plc:bob123",
      "display": "@bob.bsky.social"
    }
  ],
  "reactions": [
    { "emoji": "🎉", "count": 5, "me": true },
    { "emoji": "❤️", "count": 3, "me": false }
  ],
  "reply_to": null,
  "thread_id": null,
  "flags": 0,
  "pinned": false,
  "edited_at": null,
  "created_at": "2024-01-15T10:30:00.000Z"
}
```

## Related

- [RFC-0004: Rich Message Format](../rfcs/0004-message-format.md)
- [ADR-0004: Turso/libSQL](../adrs/0004-turso-libsql-database.md)
- [Spec: Event Schema](./event-schema.md)
