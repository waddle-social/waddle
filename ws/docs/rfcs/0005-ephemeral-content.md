# RFC-0005: Ephemeral Content

## Summary

Ephemeral content allows messages to automatically delete after a configurable time-to-live (TTL), using XEP-0428 (Fallback Indication) with message expiry hints.

## Motivation

Users want:

- Privacy for sensitive conversations
- Reduced data retention
- "Disappearing messages" like Signal/WhatsApp
- Channel-level defaults for consistent behavior

## Detailed Design

### XMPP Implementation

Ephemeral messages use a combination of:

- **XEP-0334 (Message Processing Hints)**: `<no-store/>` to prevent archiving
- **Custom expiry extension**: Based on proposed XEP for message expiry
- **Server-side enforcement**: Prosody module tracks and deletes expired messages

### Message with TTL

```xml
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <body>This will disappear in 1 hour</body>
  <expiry xmlns='urn:waddle:expiry:0' seconds='3600'/>
  <store xmlns='urn:xmpp:hints'/>
</message>
```

The `<expiry>` element specifies when the message should be deleted (relative to send time).

### TTL Configuration

TTL can be set at multiple levels (in order of precedence):

1. **Per-message**: Sender specifies TTL at send time
2. **Per-channel**: Channel default, enforced by MUC configuration
3. **Per-DM**: Conversation-level setting

### TTL Options

Predefined durations:

- 1 hour (3600s)
- 24 hours (86400s)
- 7 days (604800s)
- 30 days (2592000s)
- 90 days (7776000s)
- Custom (any duration, minimum 60 seconds)
- Permanent (no expiry)

### Message Lifecycle

```
[Sent] → [Archived w/ expiry] → [TTL Countdown] → [Retracted]
```

When TTL expires:

1. Server sends message retraction to all participants
2. Message removed from MAM archive
3. Attachments deleted from storage
4. Clients notified via retraction stanza

### Retraction on Expiry

Server automatically sends:

```xml
<message from='general@muc.penguin-club.waddle.social' type='groupchat'>
  <retract id='expired-message-id' xmlns='urn:xmpp:message-retract:1'/>
  <reason xmlns='urn:waddle:expiry:0'>ttl_expired</reason>
  <fallback xmlns='urn:xmpp:fallback:0'/>
  <body>This message has expired</body>
</message>
```

### Channel Ephemeral Mode

MUC rooms can enforce ephemeral messaging via room configuration:

```xml
<x xmlns='jabber:x:data' type='submit'>
  <field var='FORM_TYPE'>
    <value>http://jabber.org/protocol/muc#roomconfig</value>
  </field>
  <field var='muc#roomconfig_waddle_message_ttl'>
    <value>86400</value>  <!-- 24 hours in seconds -->
  </field>
  <field var='muc#roomconfig_waddle_ttl_locked'>
    <value>1</value>  <!-- Members cannot override -->
  </field>
</x>
```

When `ttl_locked` is set, users cannot send permanent messages.

### DM Ephemeral Mode

For DMs, both parties can configure TTL:

- If both set TTL, shorter duration wins
- Either party can enable ephemeral mode
- Changes apply to future messages only

Configuration stored in backend, enforced by Prosody module.

### Attachments

Attachments follow the message's TTL:

- Deleted from S3-compatible storage on expiry
- Thumbnail/preview also deleted
- CDN cache invalidated (best effort)

### MAM Considerations

Ephemeral messages are still archived (for multi-device sync) but with metadata:

```xml
<result xmlns='urn:xmpp:mam:2' id='archive-id'>
  <forwarded xmlns='urn:xmpp:forward:0'>
    <message>
      <body>Secret message</body>
      <expiry xmlns='urn:waddle:expiry:0' seconds='3600' expires-at='2024-01-15T11:30:00Z'/>
    </message>
  </forwarded>
</result>
```

The `expires-at` timestamp is absolute, allowing clients to hide expired messages immediately.

### Cleanup Process

Prosody module (`mod_waddle_expiry`):

1. Runs periodically (every minute)
2. Queries messages past expiry time
3. Sends retraction stanzas
4. Removes from MAM archive
5. Notifies backend to delete attachments

### Audit Considerations

For compliance-required deployments:

- Optional audit log of deletions (metadata only)
- Admin override to retain messages (disables ephemeral)
- Export before deletion hooks

## API

### Set Channel TTL (Backend API)

```
PATCH /channels/:id
{
  "settings": {
    "message_ttl": 86400,
    "ttl_locked": false
  }
}
```

Backend syncs to MUC room configuration.

### Set DM TTL (Backend API)

```
PATCH /dms/:id
{
  "settings": {
    "message_ttl": 86400
  }
}
```

## Security Considerations

- TTL is best-effort; cached/screenshot content not recoverable
- Clients should respect TTL for local storage
- Server deletion is authoritative
- No guarantee against malicious clients
- OMEMO-encrypted messages still expire (ciphertext deleted)

## Related

- [RFC-0002: Channels](./0002-channels.md)
- [RFC-0003: Direct Messages](./0003-direct-messages.md)
- [RFC-0004: Rich Message Format](./0004-message-format.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [ADR-0011: S3 Storage](../adrs/0011-self-hosted-storage.md)
