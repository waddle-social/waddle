# ADR-0006: XMPP for Real-time Communication

## Status

Accepted

## Context

Real-time messaging requires a persistent, bidirectional communication channel. Options include:

- **Custom WebSocket Protocol**: Full control, but requires building everything from scratch
- **XMPP**: Mature, extensible protocol with 25+ years of battle-testing at scale
- **Matrix**: Federated protocol, but heavier and more complex
- **IRC**: Simple but lacks modern features (reactions, typing, presence)

## Decision

We will use **XMPP** as the chat protocol, with **Prosody** as the initial server implementation.

### Architecture

```
┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
│   ATProto       │────▶│  Waddle Backend  │────▶│  XMPP Server    │
│   (Identity)    │     │  (Rust/Axum)     │     │  (Prosody)      │
└─────────────────┘     │  - Auth/sessions │     └────────┬────────┘
                        │  - Waddle mgmt   │              │
                        │  - Bluesky sync  │              │
                        └──────────────────┘              │
                                                         │
                        ┌────────────────────────────────┘
                        ▼
                  ┌──────────────┐
                  │   Clients    │  (Direct XMPP connection)
                  │  - Web       │
                  │  - Mobile    │
                  │  - Desktop   │
                  └──────────────┘
```

Clients connect directly to the XMPP server using standard XMPP libraries. The Rust backend handles ATProto authentication and Waddle management only.

### Identity Mapping

```
ATProto DID: did:plc:abc123xyz
     ↓
XMPP JID: abc123xyz@waddle.social
```

### Required XEPs

| XEP | Name | Purpose |
|-----|------|---------|
| XEP-0045 | Multi-User Chat (MUC) | Channels within Waddles |
| XEP-0369 | MIX | Modern channel semantics (users stay joined offline) |
| XEP-0313 | Message Archive Management | Message history sync |
| XEP-0384 | OMEMO | End-to-end encryption |
| XEP-0363 | HTTP File Upload | Attachments |
| XEP-0372 | References | Mentions |
| XEP-0444 | Message Reactions | Emoji reactions |
| XEP-0461 | Message Replies | Threading |
| XEP-0428 | Fallback Indication | Message expiry/ephemeral |

### Server Selection

| Server | Language | Pros | Cons |
|--------|----------|------|------|
| **Prosody** | Lua | Lightweight, easy to extend, good community | Less scalable than ejabberd |
| ejabberd | Erlang | Highly scalable, clustering built-in | Erlang ecosystem complexity |
| MongooseIM | Erlang | Enterprise features, fork of ejabberd | More complex |

**Choice**: Start with Prosody for simplicity. Migrate to ejabberd if scale demands.

## Consequences

### Positive

- **Battle-tested**: XMPP powers WhatsApp, Google Chat (historically), Zoom chat
- **Rich Extension Ecosystem**: 400+ XEPs cover nearly every messaging feature
- **Client Libraries**: Mature libraries for every platform (Strophe.js, Smack, etc.)
- **Federation Ready**: If we ever want cross-Waddle federation
- **Presence Built-in**: Native support for online/offline/typing status
- **Message Archive**: MAM handles history sync across devices
- **E2E Encryption**: OMEMO provides Signal-protocol-based encryption

### Negative

- **XML Verbosity**: More bandwidth than binary protocols (mitigated by compression)
- **Learning Curve**: XEP ecosystem is large and complex
- **Server Dependency**: Adds Prosody/ejabberd as infrastructure component

### Neutral

- **Different Skillset**: Lua (Prosody) or Erlang (ejabberd) for server customization

## Implementation Notes

1. Deploy Prosody with required modules (MUC, MAM, HTTP Upload)
2. Implement ATProto OAuth → XMPP account provisioning in Rust backend
3. Map Waddle creation to XMPP MUC room provisioning
4. Clients use standard XMPP libraries with BOSH or WebSocket transport

## Related

- [ADR-0002: Axum Web Framework](./0002-axum-web-framework.md)
- [ADR-0005: ATProto Identity](./0005-atproto-identity.md)
- [ADR-0008: Kameo Actors](./0008-kameo-actors.md)
- [Spec: XMPP Integration](../specs/xmpp-integration.md)
