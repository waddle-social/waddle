# RFC-0006: Presence & Status

## Summary

The presence system uses native XMPP presence stanzas for online status, with XEP-0085 for typing indicators and custom extensions for per-Waddle visibility.

## Motivation

Users want:

- See who's online in a Waddle
- Set custom status messages
- Control presence visibility per community
- Typing indicators in conversations

## Detailed Design

### XMPP Presence

Uses native XMPP `<presence>` stanzas:

```xml
<presence from='alice@waddle.social/device1'>
  <show>chat</show>
  <status>Working on Waddle!</status>
  <c xmlns='http://jabber.org/protocol/caps' hash='sha-1' node='https://waddle.social' ver='abc123'/>
</presence>
```

### Presence States

XMPP presence maps to user states:

| XMPP | Waddle State | Description |
|------|--------------|-------------|
| (available, no show) | online | Actively connected |
| `<show>away</show>` | idle | Connected but inactive |
| `<show>dnd</show>` | dnd | Do Not Disturb |
| `<show>xa</show>` | away | Extended away |
| type="unavailable" | offline | Not connected |

### Custom Status

Status text and emoji via presence:

```xml
<presence from='alice@waddle.social/device1'>
  <show>chat</show>
  <status>Working on Waddle!</status>
  <waddle-status xmlns='urn:waddle:status:0'>
    <emoji>🐧</emoji>
  </waddle-status>
</presence>
```

### Per-Waddle Presence

Users can appear differently in each Waddle using directed presence:

```xml
<!-- Appear offline in work Waddle -->
<presence from='alice@waddle.social/device1'
          to='work-waddle@muc.waddle.social'
          type='unavailable'/>

<!-- Appear online in gaming Waddle -->
<presence from='alice@waddle.social/device1'
          to='gaming@muc.penguin-club.waddle.social'>
  <show>chat</show>
</presence>
```

Per-Waddle overrides stored in backend:

```
PresenceOverride
├── waddle_id: UUID
├── visible: Boolean
├── state_override: PresenceState (optional)
└── status_override: String (optional)
```

### MUC Presence

When joining a channel, presence is sent to the room:

```xml
<presence to='general@muc.penguin-club.waddle.social/alice'>
  <x xmlns='http://jabber.org/protocol/muc'/>
  <show>chat</show>
</presence>
```

The MUC broadcasts presence to all room occupants.

### Typing Indicators

Uses XEP-0085 (Chat State Notifications):

```xml
<!-- User started typing -->
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <composing xmlns='http://jabber.org/protocol/chatstates'/>
</message>

<!-- User stopped typing -->
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <paused xmlns='http://jabber.org/protocol/chatstates'/>
</message>

<!-- User is active but not typing -->
<message to='general@muc.penguin-club.waddle.social' type='groupchat'>
  <active xmlns='http://jabber.org/protocol/chatstates'/>
</message>
```

Chat states:

- `<active/>`: User is focused on conversation
- `<composing/>`: User is typing
- `<paused/>`: User stopped typing but hasn't sent
- `<inactive/>`: User is not focused
- `<gone/>`: User has left the conversation

### Idle Detection

Client responsibilities:

- Track user activity (input, focus)
- Send `<show>away</show>` after configurable timeout (default: 5 minutes)
- Return to available on activity

```xml
<!-- User went idle -->
<presence from='alice@waddle.social/device1'>
  <show>away</show>
  <idle xmlns='urn:xmpp:idle:1' since='2024-01-15T10:25:00Z'/>
</presence>
```

### Privacy Controls

Configured via backend API, enforced by Prosody:

```
PresenceSettings
├── show_status_to: "everyone" | "friends" | "nobody"
├── show_last_active: Boolean
├── idle_timeout: Duration
└── default_waddle_visibility: Boolean
```

### Last Activity

Uses XEP-0012 (Last Activity):

```xml
<iq type='get' to='bob@waddle.social'>
  <query xmlns='jabber:iq:last'/>
</iq>

<iq type='result'>
  <query xmlns='jabber:iq:last' seconds='3600'>Working on something cool</query>
</iq>
```

### Multi-Device Presence

Uses XEP-0319 (Last User Interaction):

- Each device sends its own presence
- Server aggregates to show "online" if any device is available
- Clients can query for per-resource presence

```xml
<iq type='get' to='bob@waddle.social'>
  <query xmlns='urn:xmpp:last-user-interaction-in-presence:0'/>
</iq>
```

### Roster Integration

Presence is exchanged via XMPP roster subscriptions:

```xml
<!-- Subscribe to see someone's presence -->
<presence type='subscribe' to='bob@waddle.social'/>

<!-- Accept subscription -->
<presence type='subscribed' to='alice@waddle.social'/>
```

For Waddle members, subscriptions are automatically managed when joining/leaving.

## API Endpoints

Backend API for presence settings (not real-time presence):

```
GET    /presence/@me                Get own presence settings
PATCH  /presence/@me                Update presence settings
PATCH  /waddles/:id/presence        Set Waddle-specific overrides
```

Real-time presence comes directly from XMPP.

## Scaling Considerations

- Presence is handled by XMPP server (Prosody/ejabberd)
- Multi-node deployments use server clustering
- Typing indicators scoped to room occupants only
- Consider presence-lite for large rooms (>1000 users)

## Related

- [RFC-0001: Waddles](./0001-waddles.md)
- [RFC-0002: Channels](./0002-channels.md)
- [RFC-0003: Direct Messages](./0003-direct-messages.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [Spec: XMPP Integration](../specs/xmpp-integration.md)
