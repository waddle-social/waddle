# XMPP Integration Specification

## Overview

This document specifies how Waddle Social implements XMPP for real-time messaging, using a native Rust XMPP server (`waddle-xmpp` crate) embedded in `waddle-server`.

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                       waddle-server binary                       │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌──────────────────┐     ┌──────────────────────────────────┐  │
│  │   Axum HTTP      │     │      waddle-xmpp                 │  │
│  │   (REST API)     │     │      (XMPP Server)               │  │
│  │                  │     │                                  │  │
│  │  /auth/*         │     │  TCP 5222 (C2S)                  │  │
│  │  /waddles/*      │     │  TCP 5269 (S2S, Phase 5)         │  │
│  │  /channels/*     │     │                                  │  │
│  └────────┬─────────┘     │  Connection Actors (Kameo)       │  │
│           │               │  MUC Room Actors (Kameo)         │  │
│           │               └────────────┬─────────────────────┘  │
│           │                            │                        │
│           └──────────┬─────────────────┘                        │
│                      ▼                                          │
│           ┌──────────────────────┐                              │
│           │     Shared AppState  │                              │
│           ├──────────────────────┤                              │
│           │  Sessions            │                              │
│           │  Permissions (Zanzibar)                             │
│           │  Databases (per-Waddle libSQL)                      │
│           └──────────────────────┘                              │
└─────────────────────────────────────────────────────────────────┘
                              │
                              ▼
                  ┌──────────────────────┐
                  │       Clients        │
                  │  - CLI TUI (Ratatui) │
                  │  - Web (future)      │
                  │  - Mobile (future)   │
                  └──────────────────────┘
```

## Identity Mapping

### DID to JID Conversion

ATProto DIDs are mapped to XMPP JIDs:

```
ATProto DID: did:plc:abc123xyz789def
                        ↓
XMPP JID: abc123xyz789def@waddle.social
```

The DID method prefix (`did:plc:`) is stripped, and the identifier becomes the localpart.

For `did:web` identifiers:

```
ATProto DID: did:web:example.com
                   ↓
XMPP JID: web-example-com@waddle.social
```

### JID Structure

```
localpart@domain/resource

localpart: DID identifier (sanitized)
domain: waddle.social (or self-hosted domain)
resource: device identifier (e.g., "mobile", "desktop", "cli-abc123")
```

## Authentication Flow

### 1. ATProto OAuth

User authenticates via ATProto OAuth (see [atproto-integration.md](./atproto-integration.md)):

1. User initiates login with Bluesky handle
2. OAuth flow with user's PDS
3. Backend receives access token + DID

### 2. Session Creation

On successful ATProto auth, backend creates unified session:

```rust
// In waddle-server
struct Session {
    did: String,
    jid: Jid,
    atproto_token: String,
    xmpp_token: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}
```

### 3. XMPP Token Endpoint

Backend issues XMPP session token (same session, different view):

```json
POST /auth/xmpp-token
Authorization: Bearer <atproto-access-token>

Response:
{
  "jid": "abc123xyz@waddle.social",
  "token": "xmpp-session-token",
  "expires_at": "2024-01-15T11:30:00Z",
  "xmpp_host": "waddle.social",
  "xmpp_port": 5222,
  "websocket_url": "wss://waddle.social/xmpp-websocket"
}
```

### 4. Client XMPP Connection

Client connects to the embedded XMPP server with token:

```xml
<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>
  base64(jid + \0 + token)
</auth>
```

### 5. Token Validation (In-Process)

Unlike external Prosody, token validation is a direct function call:

```rust
// In waddle-xmpp auth handler
async fn validate_sasl_plain(
    &self,
    jid: &Jid,
    token: &str,
    app_state: &AppState,
) -> Result<Session, AuthError> {
    // Direct lookup in shared session store
    app_state.sessions.validate(jid, token).await
}
```

## Waddle to XMPP Mapping

### Waddle Creation

When a Waddle is created:

1. Create Waddle record in database
2. Spawn MUC room actors for default channels (`general`, `announcements`)
3. Add creator as owner via Zanzibar permissions

```
Waddle: penguin-club
  ↓
MUC Rooms:
  - general@waddle.social (room_id: penguin-club/general)
  - announcements@waddle.social (room_id: penguin-club/announcements)
```

### Channel Creation

```json
POST /waddles/:wid/channels
{
  "name": "random",
  "type": "text"
}
```

Backend:

1. Creates channel record in per-Waddle database
2. Spawns MUC room actor via Kameo
3. Configures room settings (persistent, members-only)
4. Updates Zanzibar permissions
5. Returns channel metadata

### Member Management

When user joins Waddle:

1. Backend adds Zanzibar tuple: `waddle:{wid}#member@user:{did}`
2. Permission propagates to all channels via Zanzibar relations
3. MUC room actors check permissions on join

```rust
// In MUC room actor
async fn handle_join(&mut self, jid: &Jid, app_state: &AppState) -> Result<(), MucError> {
    // Direct Zanzibar check
    let allowed = app_state.permissions.check(
        &format!("channel:{}", self.room_id),
        "join",
        &format!("user:{}", jid.to_did()),
    ).await?;

    if !allowed {
        return Err(MucError::Forbidden);
    }

    // Add occupant and broadcast presence
    self.add_occupant(jid).await
}
```

## Transport Options

### Native TCP

For desktop/mobile/CLI clients:

```
Host: waddle.social
Port: 5222 (STARTTLS)
```

### WebSocket

For web clients (future):

```
Endpoint: wss://waddle.social/xmpp-websocket
```

The WebSocket transport is handled by Axum's WebSocket support, bridging to the same connection actor infrastructure.

## Client Library Recommendations

| Platform | Library | Notes |
|----------|---------|-------|
| CLI (Rust) | `xmpp` crate | Native Rust, used by waddle-cli |
| Web | Strophe.js | Mature, WebSocket support |
| Web | XMPP.js | Modern, Promise-based |
| iOS | XMPPFramework | Objective-C, Swift wrapper |
| Android | Smack | Java/Kotlin |
| Desktop | libstrophe | C library, bindings available |

## XEP Implementation Status

### Phase 0: Foundation

| XEP/RFC | Name | Status |
|---------|------|--------|
| RFC 6120 | XMPP Core | Required |
| RFC 6121 | XMPP IM | Required |
| XEP-0030 | Service Discovery | Required |

### Phase 1: Core Messaging (MVP)

| XEP | Name | Status |
|-----|------|--------|
| XEP-0045 | Multi-User Chat (MUC) | Required |
| XEP-0313 | Message Archive Management | Required |
| XEP-0280 | Message Carbons | Required |
| XEP-0198 | Stream Management | Required |
| XEP-0085 | Chat State Notifications | Required |

### Phase 2: Rich Features

| XEP | Name | Status |
|-----|------|--------|
| XEP-0363 | HTTP File Upload | Required |
| XEP-0372 | References (Mentions) | Required |
| XEP-0444 | Message Reactions | Required |
| XEP-0461 | Message Replies | Required |
| XEP-0308 | Last Message Correction | Required |
| XEP-0424 | Message Retraction | Required |

### Phase 3: Security/Admin

| XEP | Name | Status |
|-----|------|--------|
| XEP-0384 | OMEMO | Optional |
| XEP-0077 | In-Band Registration | Required |
| XEP-0133 | Service Administration | Required |

### Phase 5: Federation

| XEP/RFC | Name | Status |
|---------|------|--------|
| RFC 6120 | Server Dialback | Required |
| XEP-0220 | Server Dialback | Required |

## Error Handling

### Authentication Errors

| XMPP Error | Meaning | Client Action |
|------------|---------|---------------|
| `<not-authorized/>` | Invalid token | Re-authenticate via ATProto |
| `<conflict/>` | Resource conflict | Use different resource |
| `<policy-violation/>` | Rate limited | Back off and retry |

### MUC Errors

| Error | Meaning | Client Action |
|-------|---------|---------------|
| `403 forbidden` | Not a member | Request Waddle invite |
| `404 not-found` | Room doesn't exist | Refresh channel list |
| `405 not-allowed` | Action forbidden | Check permissions |

## Rate Limits

Enforced in-process by connection actors:

| Action | Limit | Window |
|--------|-------|--------|
| Messages (per room) | 10 | 10 seconds |
| Presence updates | 5 | 60 seconds |
| Room joins | 10 | 60 seconds |
| IQ requests | 20 | 10 seconds |

## Observability

All XMPP operations are instrumented with OpenTelemetry (see [ADR-0014](../adrs/0014-opentelemetry.md)).

### Key Spans

| Span | Attributes |
|------|------------|
| `xmpp.connection.lifecycle` | `jid`, `client_ip`, `transport` |
| `xmpp.stanza.process` | `stanza_type`, `from`, `to` |
| `xmpp.muc.message` | `room_jid`, `from`, `message_id` |

### Key Metrics

| Metric | Type | Labels |
|--------|------|--------|
| `xmpp.connections.active` | Gauge | `transport` |
| `xmpp.stanzas.processed` | Counter | `type`, `direction` |
| `xmpp.stanza.latency` | Histogram | `type` |

## Connection Actor Model

Each XMPP connection is managed by a Kameo actor:

```rust
struct ConnectionActor {
    jid: Jid,
    stream: XmppStream,
    session: Session,
    state: ConnectionState,
}

impl Actor for ConnectionActor {
    type Mailbox = UnboundedMailbox<Self>;
}

#[derive(Clone)]
enum ConnectionMessage {
    Stanza(Stanza),
    Ping,
    Close,
}
```

MUC rooms are also actors, enabling concurrent message handling:

```rust
struct MucRoomActor {
    room_jid: Jid,
    waddle_id: String,
    channel_id: String,
    occupants: HashMap<Jid, Occupant>,
    config: RoomConfig,
}
```

## Related

- [ADR-0006: Native Rust XMPP Server](../adrs/0006-xmpp-protocol.md)
- [ADR-0008: Kameo Actors](../adrs/0008-kameo-actors.md)
- [ADR-0014: OpenTelemetry](../adrs/0014-opentelemetry.md)
- [Spec: ATProto Integration](./atproto-integration.md)
- [Spec: File Upload](./file-upload.md)
- [RFC-0002: Channels](../rfcs/0002-channels.md)
- [RFC-0003: Direct Messages](../rfcs/0003-direct-messages.md)
