# RFC-0003: Direct Messages & Groups

## Summary

Direct Messages (DMs) provide private communication outside of Waddles, using native XMPP 1:1 messaging and private MUC rooms for group chats.

## Motivation

Users need to:

- Have private conversations not tied to any Waddle
- Create small group chats for coordination
- Maintain conversations across multiple shared Waddles
- Control who can initiate contact

## Detailed Design

### XMPP Implementation

DMs use native XMPP messaging:

- **1:1 DMs**: Standard XMPP `<message type="chat">` stanzas
- **Group DMs**: Private MUC rooms with invite-only membership

```
1:1 DM:
  alice@waddle.social → bob@waddle.social
  (standard XMPP chat)

Group DM:
  Private MUC: dm-abc123@muc.waddle.social
  (members-only, non-persistent listing)
```

### DM Types

1. **1:1 DM**: Conversation between exactly two users (XMPP chat)
2. **Group DM**: Conversation with 2-10 participants (private MUC)

### 1:1 DM Behavior

Uses standard XMPP messaging:

```xml
<message from='alice@waddle.social/device1'
         to='bob@waddle.social'
         type='chat'>
  <body>Hey, how's it going?</body>
</message>
```

- Created implicitly on first message
- History stored via MAM (XEP-0313)
- Carbons (XEP-0280) sync across devices
- Cannot be "deleted", only conversation cleared locally

### Group DM Behavior

Uses private MUC rooms:

```xml
<!-- Create private MUC for group DM -->
<presence to='dm-abc123@muc.waddle.social/alice'>
  <x xmlns='http://jabber.org/protocol/muc'/>
</presence>
```

Room configuration:

```xml
<x xmlns='jabber:x:data' type='submit'>
  <field var='muc#roomconfig_membersonly'><value>1</value></field>
  <field var='muc#roomconfig_publicroom'><value>0</value></field>
  <field var='muc#roomconfig_persistentroom'><value>1</value></field>
  <field var='muc#roomconfig_allowinvites'><value>1</value></field>
</x>
```

- Created explicitly via backend API (provisions MUC)
- Creator is owner, can add/remove participants
- Any participant can leave
- Dissolved when < 2 participants remain

### Group DM Structure

```
GroupDM
├── id: UUID (internal reference)
├── muc_jid: String (dm-{id}@muc.waddle.social)
├── name: String (optional)
├── icon: URL (optional)
├── owner_did: DID (creator)
├── created_at: Timestamp
└── settings: DMSettings

DMSettings
├── message_ttl: Duration (optional, via XEP-0428)
└── e2e_required: Boolean (require OMEMO)
```

### DM Requests

To prevent spam, DMs can require approval. This is handled in the backend:

```
DMRequest
├── id: UUID
├── from_did: DID
├── to_did: DID
├── message_preview: String (first message)
├── status: "pending" | "accepted" | "declined"
└── created_at: Timestamp
```

**Auto-accept conditions** (configurable per user):

- Shares a Waddle with sender
- Sender is in user's contacts
- User allows all DMs

When accepted, the backend allows XMPP messages to flow; when pending/declined, messages are blocked at the server.

### Privacy Controls

Users configure via backend API:

- `dm_policy`: "everyone" | "friends_and_waddles" | "friends_only" | "nobody"
- `show_read_receipts`: Boolean (XEP-0184)
- `show_typing_indicator`: Boolean (XEP-0085)

These settings are enforced via Prosody modules that check the backend before allowing message delivery.

### Message Features

1:1 and Group DMs support:

- **Read receipts**: XEP-0184 (Message Delivery Receipts)
- **Typing indicators**: XEP-0085 (Chat State Notifications)
- **Reactions**: XEP-0444 (Message Reactions)
- **Replies**: XEP-0461 (Message Replies)
- **E2E encryption**: XEP-0384 (OMEMO)

### Limits

| Resource | Limit |
|----------|-------|
| Group DM participants | 10 |
| Active DMs per user | 1000 |
| Group DMs owned | 100 |

## API Endpoints

Backend REST API for DM management:

```
GET    /dms                        List user's DMs (metadata only)
POST   /dms                        Create group DM (provisions MUC)
GET    /dms/:id                    Get DM details
PATCH  /dms/:id                    Update DM (group settings)
DELETE /dms/:id                    Leave/close DM
POST   /dms/:id/participants       Add participant (group)
DELETE /dms/:id/participants/:did  Remove participant (group)
GET    /dm-requests                List pending DM requests
POST   /dm-requests/:id/accept     Accept request
POST   /dm-requests/:id/decline    Decline request
```

Message operations happen directly over XMPP.

## XMPP Events

- **New message**: `<message type="chat">` or MUC groupchat
- **Typing**: XEP-0085 `<composing/>`
- **Read receipt**: XEP-0184 `<received/>`
- **Delivery receipt**: XEP-0184 `<received/>`

## Related

- [RFC-0004: Rich Message Format](./0004-message-format.md)
- [RFC-0005: Ephemeral Content](./0005-ephemeral-content.md)
- [RFC-0006: Presence System](./0006-presence-system.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [Spec: XMPP Integration](../specs/xmpp-integration.md)
