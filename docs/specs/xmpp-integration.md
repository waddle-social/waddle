# XMPP Integration Specification

## Overview

This document specifies how Waddle Social integrates with XMPP for real-time messaging, using Prosody as the server implementation.

## Architecture

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│   ATProto       │────▶│  Waddle Backend  │────▶│  Prosody        │
│   (Identity)    │     │  (Rust/Axum)     │     │  (XMPP Server)  │
└─────────────────┘     └──────────────────┘     └────────┬────────┘
                                │                         │
                                │ REST API                │ XMPP
                                │ (management)            │ (messaging)
                                ▼                         ▼
                        ┌──────────────────────────────────┐
                        │            Clients               │
                        └──────────────────────────────────┘
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
resource: device identifier (e.g., "mobile", "desktop", "web-abc123")
```

## Authentication Flow

### 1. ATProto OAuth

User authenticates via ATProto OAuth (see [atproto-integration.md](./atproto-integration.md)):

1. User initiates login with Bluesky handle
2. OAuth flow with user's PDS
3. Backend receives access token + DID

### 2. XMPP Account Provisioning

On first login, backend creates XMPP account:

```lua
-- Prosody mod_auth_waddle
function provider.create_user(username, password)
    -- Called by backend via admin API
    -- username = DID identifier
    -- password = generated token
end
```

### 3. Session Token Issuance

Backend issues short-lived XMPP session token:

```json
POST /auth/xmpp-token
Authorization: Bearer <atproto-access-token>

Response:
{
  "jid": "abc123xyz@waddle.social",
  "token": "xmpp-session-token",
  "expires_at": "2024-01-15T11:30:00Z",
  "xmpp_host": "xmpp.waddle.social",
  "xmpp_port": 5222,
  "bosh_url": "https://xmpp.waddle.social/http-bind",
  "websocket_url": "wss://xmpp.waddle.social/xmpp-websocket"
}
```

### 4. Client XMPP Connection

Client connects to XMPP server with token:

```xml
<auth xmlns='urn:ietf:params:xml:ns:xmpp-sasl' mechanism='PLAIN'>
  base64(jid + \0 + token)
</auth>
```

## Prosody Configuration

### Required Modules

```lua
-- prosody.cfg.lua
modules_enabled = {
    -- Core
    "roster";
    "saslauth";
    "tls";
    "dialback";
    "disco";
    "posix";

    -- MUC
    "muc";
    "muc_mam";

    -- Modern messaging
    "carbons";       -- XEP-0280: Message Carbons
    "mam";           -- XEP-0313: Message Archive Management
    "csi";           -- XEP-0352: Client State Indication
    "smacks";        -- XEP-0198: Stream Management

    -- Rich features
    "http_upload";   -- XEP-0363: HTTP File Upload
    "bookmarks";     -- XEP-0402: PEP Bookmarks
    "vcard4";        -- XEP-0292: vCard4

    -- Waddle custom
    "auth_waddle";   -- Custom auth against Waddle backend
    "waddle_expiry"; -- Message TTL enforcement
    "waddle_dm_filter"; -- DM request filtering
}
```

### Virtual Hosts

```lua
-- Main domain
VirtualHost "waddle.social"
    authentication = "waddle"  -- Custom auth module

-- MUC component for channels
Component "muc.waddle.social" "muc"
    modules_enabled = { "muc_mam" }
    muc_room_default_persistent = true
    muc_room_default_members_only = true

-- HTTP upload component
Component "upload.waddle.social" "http_upload"
    http_upload_path = "/uploads"
    http_upload_file_size_limit = 104857600  -- 100MB
```

### Custom Auth Module

```lua
-- mod_auth_waddle.lua
local http = require "net.http";
local json = require "util.json";

local provider = {};

function provider.test_password(username, password)
    -- Verify token with Waddle backend
    local response = http.request(
        "POST",
        "http://localhost:3000/internal/verify-xmpp-token",
        { ["Content-Type"] = "application/json" },
        json.encode({ jid = username, token = password })
    );
    return response.code == 200;
end

function provider.user_exists(username)
    -- Check with Waddle backend
    local response = http.request(
        "GET",
        "http://localhost:3000/internal/user-exists/" .. username
    );
    return response.code == 200;
end

module:provides("auth", provider);
```

## Waddle to XMPP Mapping

### Waddle Creation

When a Waddle is created, backend provisions MUC infrastructure:

1. Create MUC subdomain (optional, for isolation)
2. Create default rooms (`general`, `announcements`)
3. Set room configurations
4. Add creator as owner

```
Waddle: penguin-club
  ↓
MUC Domain: muc.waddle.social (shared) or muc.penguin-club.waddle.social (isolated)
  ↓
Rooms: general@muc..., announcements@muc...
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

1. Creates channel record in database
2. Provisions MUC room via Prosody admin API
3. Configures room settings
4. Returns channel metadata

### Member Management

When user joins Waddle:

1. Backend adds user to Waddle membership
2. Backend grants MUC affiliation to all Waddle rooms
3. Client receives room list and joins

```xml
<!-- Backend grants affiliation via admin -->
<iq to='general@muc.waddle.social' type='set'>
  <query xmlns='http://jabber.org/protocol/muc#admin'>
    <item affiliation='member' jid='newuser@waddle.social'/>
  </query>
</iq>
```

## Transport Options

### BOSH (HTTP Binding)

For web clients without WebSocket support:

```
Endpoint: https://xmpp.waddle.social/http-bind
```

### WebSocket

Preferred for web clients:

```
Endpoint: wss://xmpp.waddle.social/xmpp-websocket
```

### Native TCP

For desktop/mobile clients:

```
Host: xmpp.waddle.social
Port: 5222 (STARTTLS) or 5223 (Direct TLS)
```

## Client Library Recommendations

| Platform | Library | Notes |
|----------|---------|-------|
| Web | Strophe.js | Mature, BOSH + WebSocket |
| Web | XMPP.js | Modern, Promise-based |
| iOS | XMPPFramework | Objective-C, Swift wrapper |
| Android | Smack | Java/Kotlin |
| Rust | xmpp-rs | Native Rust |
| Desktop | libstrophe | C library, bindings available |

## XEP Implementation Status

### Required (MVP)

| XEP | Name | Status |
|-----|------|--------|
| XEP-0045 | Multi-User Chat | Required |
| XEP-0313 | Message Archive Management | Required |
| XEP-0280 | Message Carbons | Required |
| XEP-0198 | Stream Management | Required |
| XEP-0363 | HTTP File Upload | Required |
| XEP-0085 | Chat State Notifications | Required |

### Required (Rich Features)

| XEP | Name | Status |
|-----|------|--------|
| XEP-0369 | MIX | Recommended |
| XEP-0372 | References | Required |
| XEP-0444 | Message Reactions | Required |
| XEP-0461 | Message Replies | Required |
| XEP-0308 | Last Message Correction | Required |
| XEP-0424 | Message Retraction | Required |
| XEP-0384 | OMEMO | Required |

### Optional (Future)

| XEP | Name | Status |
|-----|------|--------|
| XEP-0166 | Jingle | Voice/video |
| XEP-0167 | Jingle RTP | Voice/video |
| XEP-0234 | Jingle File Transfer | P2P files |

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

Enforced by Prosody:

| Action | Limit | Window |
|--------|-------|--------|
| Messages (per room) | 10 | 10 seconds |
| Presence updates | 5 | 60 seconds |
| Room joins | 10 | 60 seconds |
| File uploads | 5 | 60 seconds |

## Monitoring

### Prosody Metrics

```lua
-- mod_prometheus.lua
modules_enabled = { "prometheus" }
```

Metrics endpoint: `http://localhost:5280/metrics`

Key metrics:

- `prosody_c2s_connections`: Connected clients
- `prosody_muc_occupants`: Room occupants
- `prosody_messages_sent`: Message throughput
- `prosody_stanza_latency`: Processing latency

## Related

- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [Spec: ATProto Integration](./atproto-integration.md)
- [Spec: File Upload](./file-upload.md)
- [RFC-0002: Channels](../rfcs/0002-channels.md)
- [RFC-0003: Direct Messages](../rfcs/0003-direct-messages.md)
