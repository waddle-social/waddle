# RFC-0002: Channel System

## Summary

Channels are the primary organizational unit for conversations within a Waddle. They are implemented as XMPP Multi-User Chat (MUC) rooms with MIX extensions for modern semantics.

## Motivation

Users need to:

- Organize discussions by topic within a community
- Control who can read and write in specific areas
- Configure message retention and behavior per channel
- Create hierarchical organization via categories

## Detailed Design

### XMPP Implementation

Channels map to XMPP MUC rooms (XEP-0045) with MIX (XEP-0369) for persistent membership:

```
Waddle: penguin-club
  ↓
XMPP Domain: penguin-club.waddle.social
  ↓
Channel: #general
  ↓
MUC Room: general@muc.penguin-club.waddle.social
```

### Channel Types

1. **Text Channel**: Standard MUC room for message-based communication
2. **Announcement Channel**: MUC with restricted posting (moderated room)
3. **Voice Channel**: Real-time voice communication (future phase, Jingle XEP-0166)
4. **Stage Channel**: One-to-many broadcast with raised hands (future phase)

### Channel Structure

```
Channel
├── id: UUID (internal reference)
├── waddle_id: UUID
├── muc_jid: String (room@muc.waddle.waddle.social)
├── category_id: UUID (optional)
├── name: String (1-100 chars, lowercase, hyphens allowed)
├── topic: String (optional, max 1024 chars)
├── type: ChannelType
├── position: Integer (ordering within category)
├── settings: ChannelSettings
└── created_at: Timestamp

ChannelSettings
├── message_ttl: Duration (optional, for ephemeral via XEP-0428)
├── slowmode_interval: Duration (optional)
├── nsfw: Boolean
├── moderated: Boolean (announcement channels)
└── auto_archive_threads: Duration (optional)
```

### MUC Room Configuration

When a channel is created, the backend provisions a MUC room with:

```xml
<x xmlns='jabber:x:data' type='submit'>
  <field var='FORM_TYPE'>
    <value>http://jabber.org/protocol/muc#roomconfig</value>
  </field>
  <field var='muc#roomconfig_persistentroom'><value>1</value></field>
  <field var='muc#roomconfig_membersonly'><value>1</value></field>
  <field var='muc#roomconfig_whois'><value>moderators</value></field>
  <field var='muc#roomconfig_enablelogging'><value>1</value></field>
</x>
```

### Categories

Categories group related channels visually (client-side organization):

```
Category
├── id: UUID
├── waddle_id: UUID
├── name: String
├── position: Integer
└── collapsed: Boolean (client-side hint)
```

Categories are stored in the Waddle backend database, not in XMPP.

### Channel Permissions

Permissions map to MUC affiliations and roles:

| Waddle Permission | MUC Affiliation/Role |
|-------------------|----------------------|
| `view_channel` | member affiliation |
| `send_messages` | participant role |
| `manage_messages` | moderator role |
| `manage_channel` | admin affiliation |
| `mention_everyone` | Custom extension |

See [Spec: Permission Model](../specs/permission-model.md) for Zanzibar integration.

### Message History

Message history uses XEP-0313 (Message Archive Management):

```xml
<iq type='set' id='q1'>
  <query xmlns='urn:xmpp:mam:2'>
    <x xmlns='jabber:x:data' type='submit'>
      <field var='FORM_TYPE'><value>urn:xmpp:mam:2</value></field>
      <field var='with'><value>general@muc.penguin-club.waddle.social</value></field>
    </x>
    <set xmlns='http://jabber.org/protocol/rsm'>
      <max>50</max>
    </set>
  </query>
</iq>
```

### Threads

Threads use XEP-0461 (Message Replies):

```xml
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <body>This is a reply</body>
  <reply xmlns='urn:xmpp:reply:0' to='original-message-id'/>
  <thread>thread-id</thread>
</message>
```

### Limits

| Resource | Limit |
|----------|-------|
| Channels per Waddle | 500 |
| Categories per Waddle | 50 |
| Channel name length | 100 characters |
| Topic length | 1024 characters |

## API Endpoints

The Waddle backend exposes REST endpoints for channel management (provisioning MUC rooms):

```
POST   /waddles/:wid/channels           Create channel (provisions MUC)
GET    /waddles/:wid/channels           List channels
GET    /channels/:id                    Get channel metadata
PATCH  /channels/:id                    Update channel (syncs to MUC config)
DELETE /channels/:id                    Delete channel (destroys MUC room)
```

Message operations happen directly over XMPP, not through REST.

## XMPP Events

Clients receive channel events via XMPP:

- **Room created**: MUC presence with status code 201
- **Room configuration changed**: MUC message with status code 104
- **Room destroyed**: MUC presence with status code 332
- **User typing**: XEP-0085 Chat State Notifications

## Related

- [RFC-0001: Waddles](./0001-waddles.md)
- [RFC-0004: Rich Message Format](./0004-message-format.md)
- [RFC-0005: Ephemeral Content](./0005-ephemeral-content.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [Spec: XMPP Integration](../specs/xmpp-integration.md)
