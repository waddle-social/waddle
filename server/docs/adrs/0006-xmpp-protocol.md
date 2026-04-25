# ADR-0006: Native Rust XMPP Server

## Status

Accepted (Updated)

## Context

Real-time messaging requires a persistent, bidirectional communication channel. Options include:

- **Custom WebSocket Protocol**: Full control, but requires building everything from scratch
- **XMPP**: Mature, extensible protocol with 25+ years of battle-testing at scale
- **Matrix**: Federated protocol, but heavier and more complex
- **IRC**: Simple but lacks modern features (reactions, typing, presence)

### Original Decision (Superseded)

Initially, we planned to use Prosody as an external XMPP server. After further analysis, we've decided to build a native Rust XMPP server instead.

### Why Native Rust Instead of Prosody?

| Factor | Prosody | Native Rust |
|--------|---------|-------------|
| **Deployment** | Separate process, IPC needed | Single binary, direct calls |
| **Observability** | Separate metrics/traces | Unified OpenTelemetry traces |
| **State Sharing** | HTTP/socket coordination | Shared `AppState` in-process |
| **Language** | Lua customization | Rust (our core competency) |
| **Permissions** | Sync via HTTP | Direct Zanzibar calls |
| **License** | MIT (Prosody) | AGPL (source available) |

## Decision

We will use **XMPP** as the chat protocol, implemented as a native Rust library (`waddle-xmpp` crate) embedded in `waddle-server`.

### Architecture

```
crates/
├── waddle-xmpp/           # XMPP server library
│   ├── src/
│   │   ├── lib.rs
│   │   ├── auth/          # SASL mechanisms
│   │   ├── protocol/      # Sans-I/O C2S protocol
│   │   ├── muc/           # XEP-0045 MUC
│   │   ├── mam/           # XEP-0313 Archive
│   │   └── presence/      # Presence management
│   └── Cargo.toml
└── waddle-server/
    └── (depends on waddle-xmpp)
```

```
waddle-server binary
├── Axum HTTP (REST API)
├── Axum WebSocket (RFC 7395 C2S)
├── waddle-xmpp (XMPP Server)
│   ├── Sans-I/O protocol + handlers
│   └── MUC Room Actors (Kameo)
└── Shared AppState
    ├── Sessions
    ├── Permissions (Zanzibar)
    └── Databases (per-Waddle libSQL)
```

### Identity Mapping

```
ATProto DID: did:plc:abc123xyz
     ↓
XMPP JID: abc123xyz@waddle.social
```

### XEP Prioritization

| Phase | XEPs | Purpose |
|-------|------|---------|
| MVP | RFC 6120/6121, XEP-0045 (MUC), XEP-0313 (MAM), XEP-0198 (Stream Mgmt), XEP-0280 (Carbons), XEP-0030 (Disco) | Core messaging |
| Phase 2 | XEP-0363 (HTTP Upload), XEP-0372 (Mentions), XEP-0444 (Reactions), XEP-0461 (Replies), XEP-0308 (Edit), XEP-0424 (Delete) | Rich messaging |
| Phase 3 | XEP-0384 (OMEMO), XEP-0077 (In-Band Reg), XEP-0133 (Admin) | Security/Admin |

### Key Dependencies

```toml
# XMPP Server
jid = "0.10"                # JID handling
xmpp-parsers = "0.21"       # Stanza parsing
dashmap = "6.0"             # Concurrent session storage
rustls = "0.23"             # TLS
```

## Consequences

### Positive

- **Single Deployment**: One binary, simpler ops, no IPC
- **Unified Observability**: OpenTelemetry traces span HTTP → XMPP → DB
- **Direct State Access**: Permissions checked in-process via Zanzibar
- **Type Safety**: Rust's type system catches protocol errors at compile time
- **Battle-tested Protocol**: XMPP powers WhatsApp, Zoom chat, and others
- **Rich Extension Ecosystem**: 400+ XEPs cover nearly every messaging feature
- **Client Libraries**: Mature libraries for every platform (Strophe.js, Smack, etc.)
- **AGPL Source**: All components are open source

### Negative

- **Development Effort**: Building XMPP server from scratch vs. deploying Prosody
- **Protocol Complexity**: XMPP/XML has more edge cases than custom protocols
- **Testing Burden**: Must pass XMPP interop tests for compliance

### Mitigations

- **Incremental Delivery**: XEPs implemented in phases, MVP → Rich
- **Interop Testing**: XMPP compliance tests in CI catch regressions
- **Library Reuse**: `xmpp-parsers` handles typed stanza parsing

## Implementation Phases

### Phase 0: Foundation
- Create `crates/waddle-xmpp/` crate
- WebSocket C2S endpoint at `/ws`
- XML stream parsing via xmpp-parsers
- OpenTelemetry setup (traces, metrics)
- Sans-I/O protocol lifecycle

### Phase 1: Authentication & Presence
- SASL PLAIN authentication
- Custom ATProto token auth mechanism
- Session management in AppState
- Basic presence (online/offline)
- XEP-0030 Service Discovery

### Phase 2: MUC (Channels)
- XEP-0045 MUC core
- MUC room actors (Kameo)
- Room creation/configuration
- Affiliation management via Zanzibar
- MUC message routing

### Phase 3: Message Archive & Sync
- XEP-0313 MAM
- XEP-0280 Message Carbons
- XEP-0198 Stream Management
- Per-Waddle message storage (libSQL)

### Phase 4: Rich Features
- XEP-0085 Typing indicators
- XEP-0308 Edit, XEP-0424 Delete
- XEP-0444 Reactions, XEP-0461 Replies
- XEP-0372 Mentions
- XEP-0363 HTTP File Upload

## Related

- [ADR-0002: Axum Web Framework](./0002-axum-web-framework.md)
- [ADR-0005: ATProto Identity](./0005-atproto-identity.md)
- [ADR-0008: Kameo Actors](./0008-kameo-actors.md)
- [ADR-0014: OpenTelemetry Instrumentation](./0014-opentelemetry.md)
- [Spec: XMPP Integration](../specs/xmpp-integration.md)
