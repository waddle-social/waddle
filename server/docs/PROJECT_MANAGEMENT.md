# Waddle Social - Project Management

## Overview

This document tracks implementation progress for Waddle Social, an open-source consumer chat/communication platform with ATProto integration.

**License**: AGPL-3.0
**MVP Target**: Federated XMPP ecosystem with optional ATProto identity

---

## Current Priority: Federation Architecture

The immediate focus is building a **federated XMPP ecosystem** where:

1. **waddle.social** acts as the identity home-server for ATProto users
2. Anyone can run self-hosted waddles (independent XMPP servers)
3. Users can federate across waddles using standard XMPP S2S
4. Traditional JID users can participate without ATProto

See [RFC-0015: Federation Architecture](rfcs/0015-federation-architecture.md) for full details.

---

## Implementation Phases

### Phase F1: Native JID Authentication (P0 - CRITICAL)

**Goal:** Allow users to register and authenticate without ATProto

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| SCRAM-SHA-256 SASL mechanism | ✅ Complete | P0 | [ADR-0015](adrs/0015-dual-authentication.md) |
| XEP-0077 In-Band Registration | ✅ Complete | P0 | [ADR-0015](adrs/0015-dual-authentication.md) |
| Native JID credential storage | ✅ Complete | P0 | [ADR-0015](adrs/0015-dual-authentication.md) |
| `native_users` database table | ✅ Complete | P0 | [ADR-0015](adrs/0015-dual-authentication.md) |
| Argon2id password hashing | ✅ Complete | P0 | [ADR-0015](adrs/0015-dual-authentication.md) |
| Config: `native_auth_enabled` | ✅ Complete | P0 | [ADR-0015](adrs/0015-dual-authentication.md) |

**Verification:**
- [ ] Register native JID via XMPP client (Gajim/Conversations)
- [ ] Login with SCRAM-SHA-256
- [ ] Join local MUC channel

**Files to create/modify:**
```
crates/waddle-xmpp/src/auth/scram.rs          (new)
crates/waddle-xmpp/src/xep/xep0077.rs         (new)
crates/waddle-server/src/auth/native.rs       (new)
crates/waddle-server/src/db/global.rs         (modify)
crates/waddle-server/src/config.rs            (modify)
```

### Phase F2: Server Mode Configuration (P0 - CRITICAL) ✅ COMPLETE

**Goal:** Support running as either home-server or standalone waddle

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| `ServerMode` enum (HomeServer/Standalone) | ✅ Complete | P0 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| Conditional ATProto initialization | ✅ Complete | P0 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| `WADDLE_MODE` environment variable | ✅ Complete | P0 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| Mode-specific route registration | ✅ Complete | P0 | [RFC-0015](rfcs/0015-federation-architecture.md) |

**Verification:**
- [x] Start server with `WADDLE_MODE=standalone`
- [x] Confirm ATProto routes are disabled
- [x] Confirm native registration works

**Files created/modified:**
```
crates/waddle-server/src/config.rs            (complete)
crates/waddle-server/src/main.rs              (complete)
```

### Phase F3: S2S Federation Core (P0 - CRITICAL) 🔄 IN PROGRESS

**Goal:** Enable XMPP server-to-server communication

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| S2S listener on port 5269 | ✅ Complete | P0 | [Spec: S2S](specs/s2s-federation.md) |
| TLS 1.3 for S2S connections | ✅ Complete | P0 | [Spec: S2S](specs/s2s-federation.md) |
| Stream negotiation (S2S) | ✅ Complete | P0 | [Spec: S2S](specs/s2s-federation.md) |
| XEP-0220 Server Dialback | ✅ Complete | P0 | [Spec: S2S](specs/s2s-federation.md) |
| DNS SRV record resolution | ✅ Complete | P0 | [Spec: S2S](specs/s2s-federation.md) |
| S2S connection pool | ✅ Complete | P0 | [Spec: S2S](specs/s2s-federation.md) |
| Remote JID routing | ✅ Complete | P0 | [Spec: S2S](specs/s2s-federation.md) |

**Verification:**
- [ ] Two waddle instances communicate (waddle.social:5269, test.local:5269)
- [ ] User on test.local sends message to user@waddle.social
- [ ] Message delivered via S2S

**Files to create:**
```
crates/waddle-xmpp/src/s2s/mod.rs             (new)
crates/waddle-xmpp/src/s2s/connection.rs      (new)
crates/waddle-xmpp/src/s2s/dialback.rs        (new)
crates/waddle-xmpp/src/s2s/pool.rs            (new)
crates/waddle-xmpp/src/s2s/dns.rs             (new)
crates/waddle-xmpp/src/routing.rs             (modify)
```

### Phase F4: Federated MUC Participation (P0 - CRITICAL) 🔄 IN PROGRESS

**Goal:** Users from remote servers can join local MUC rooms

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| Accept remote JIDs as MUC occupants | ✅ Complete | P0 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| Route presence to remote occupants | ✅ Complete | P0 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| Route messages to remote occupants | ✅ Complete | P0 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| Permission model for federated users | ✅ Complete | P0 | [RFC-0015](rfcs/0015-federation-architecture.md) |

**Verification:**
- [ ] Native JID user on alice.dev joins channel on waddle.social
- [ ] ATProto user on waddle.social joins channel on alice.dev
- [ ] Both see each other's messages in real-time

**Files to create/modify:**
```
crates/waddle-xmpp/src/muc/mod.rs             (modify)
crates/waddle-xmpp/src/muc/federation.rs      (new)
```

### Phase F5: Hosted Waddle Subdomains (P1)

**Goal:** MUC namespacing for hosted waddles on waddle.social

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| Subdomain provisioning API | ⬜ Not Started | P1 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| Subdomain-aware MUC routing | ⬜ Not Started | P1 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| Per-waddle SQLite selection | ⬜ Not Started | P1 | [RFC-0015](rfcs/0015-federation-architecture.md) |
| DNS wildcard setup docs | ⬜ Not Started | P1 | [RFC-0015](rfcs/0015-federation-architecture.md) |

**Verification:**
- [ ] Create hosted waddle "penguin"
- [ ] Room `general@penguin.waddle.social` is accessible
- [ ] Messages stored in penguin's SQLite database

### Phase XC1: XEP-0479 Core Compliance (P0) ✅ COMPLETE

**Goal:** Meet XEP-0479 (XMPP Compliance Suites 2023) Core requirements

| Task | Status | Priority | Notes |
|------|--------|----------|-------|
| XEP-0115 Entity Capabilities | ✅ Complete | P0 | waddle-xmpp/src/xep/xep0115.rs |

**Currently Passing (from internal interop tests):**
- RFC 6120 (XMPP Core) - stream.rs, connection.rs
- RFC 7590 (TLS) - STARTTLS in stream.rs
- XEP-0030 (Service Discovery) - disco/

**Verification:**
- [ ] Entity capabilities hash advertised in presence
- [ ] Capabilities cached correctly
- [ ] disco#info responds with capabilities

### Phase XC2: XEP-0479 IM Basic Compliance (P0)

**Goal:** Meet XEP-0479 Instant Messaging basic requirements

| Task | Status | Priority | Notes |
|------|--------|----------|-------|
| RFC 6121 XMPP IM (roster, presence) | ✅ Complete | P0 | Roster management ✅, presence subscription ✅ |
| XEP-0054 vcard-temp | ✅ Complete | P0 | User profile information (get/set handlers, storage) |
| XEP-0249 Direct MUC Invitations | ✅ Complete | P0 | Direct channel invites |
| Complete XEP-0045 MUC | ✅ Complete | P0 | MUC implementation complete (muc/) |
| Complete XEP-0280 Message Carbons | ✅ Complete | P0 | Integrated into chat message routing |
| XEP-0363 HTTP File Upload | ✅ Complete | P0 | File sharing capability |

**Verification:**
- [ ] Roster operations work with standard clients
- [ ] Presence subscription flow complete
- [x] vCard retrieval and update working
- [x] MUC invitations delivered
- [x] Message carbons syncing across devices
- [x] File upload slot allocation working

### Phase XC3: XEP-0479 IM Advanced Compliance (P1)

**Goal:** Meet XEP-0479 Instant Messaging advanced requirements

| Task | Status | Priority | Notes |
|------|--------|----------|-------|
| Complete XEP-0313 MAM | ✅ Complete | P1 | Message archive with RSM pagination (mam/) |
| Complete XEP-0198 Stream Management | ✅ Complete | P1 | stream_management/ with session registry |
| XEP-0048 Bookmark Storage | ✅ Complete | P1 | Legacy compat layer over XEP-0402 (xep/xep0048.rs) |
| XEP-0049 Private XML Storage | ✅ Complete | P1 | Per-user namespace-keyed storage (xep/xep0049.rs) |
| XEP-0060 PubSub | ✅ Complete | P1 | PubSub service with disco (pubsub/) |
| XEP-0084 User Avatar | ✅ Complete | P1 | PEP-based avatar data/metadata (xep/xep0084.rs) |
| XEP-0153 vCard-Based Avatars | ✅ Complete | P1 | Presence avatar hash broadcasting (xep/xep0153.rs) |
| XEP-0163 PEP | ✅ Complete | P1 | Personal Eventing Protocol (pubsub/pep.rs) |
| XEP-0191 Blocking Command | ✅ Complete | P1 | User blocking capability |
| XEP-0223 Persistent Storage | ✅ Complete | P1 | PubSub best practices profile (xep/xep0223.rs) |
| XEP-0398 Avatar Conversion | ✅ Complete | P1 | PEP↔vCard avatar bridge (xep/xep0398.rs) |
| XEP-0402 PEP Native Bookmarks | ✅ Complete | P1 | Modern bookmark storage (xep/xep0402.rs) |
| XEP-0410 MUC Self-Ping | ✅ Complete | P1 | Connection state verification |

**CI Currently Disabled:**
- XEP-0220 (Server Dialback) - S2S federation (separate concern)

**Verification:**
- [x] MAM queries return correct history
- [x] Stream management resumes sessions
- [ ] Bookmarks persist across sessions
- [x] Blocked users cannot send messages (direct chat + invites)
- [x] MUC self-ping detects disconnection

### Phase XC4: XEP-0479 Mobile Compliance (P1)

**Goal:** Meet XEP-0479 Mobile requirements

| Task | Status | Priority | Notes |
|------|--------|----------|-------|
| XEP-0352 Client State Indication | ✅ Complete | P1 | Traffic optimization for mobile (xep/xep0352.rs) |

**Verification:**
- [ ] Client can indicate active/inactive state
- [ ] Server reduces traffic for inactive clients
- [ ] State transitions handled correctly

---

### Phase 1: Foundation (Current MVP Items)

Core infrastructure and basic messaging for the CLI TUI client.

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| **Backend Infrastructure** |
| Rust project setup | ✅ Complete | P0 | [ADR-0001](adrs/0001-rust-backend.md) |
| Axum HTTP server | ✅ Complete | P0 | [ADR-0002](adrs/0002-axum-web-framework.md), [Spec: API](specs/api-contracts.md) |
| Native XMPP server (waddle-xmpp crate) | 🔄 In Progress | P0 | [ADR-0006](adrs/0006-xmpp-protocol.md), [Spec: XMPP](specs/xmpp-integration.md) |
| OpenTelemetry setup | ✅ Complete | P0 | [ADR-0014](adrs/0014-opentelemetry.md) |
| XMPP interop CI | ✅ Complete | P0 | [ADR-0006](adrs/0006-xmpp-protocol.md) |
| Turso/libSQL setup | ✅ Complete | P0 | [ADR-0004](adrs/0004-turso-libsql-database.md) |
| Database-per-Waddle sharding | ✅ Complete | P0 | [ADR-0004](adrs/0004-turso-libsql-database.md) |
| CQRS event system | ⬜ Not Started | P2 | [ADR-0007](adrs/0007-cqrs-architecture.md), [Spec: Events](specs/event-schema.md) |
| Kameo actor setup | ⬜ Not Started | P2 | [ADR-0008](adrs/0008-kameo-actors.md) |
| **Authentication** |
| ATProto OAuth flow | ✅ Complete | P0 | [ADR-0005](adrs/0005-atproto-identity.md), [Spec: ATProto](specs/atproto-integration.md) |
| DID resolution | ✅ Complete | P0 | [ADR-0005](adrs/0005-atproto-identity.md), [Spec: ATProto](specs/atproto-integration.md) |
| DID → JID mapping | ✅ Complete | P0 | [Spec: XMPP](specs/xmpp-integration.md) |
| XMPP account provisioning | ✅ Complete | P0 | [Spec: XMPP](specs/xmpp-integration.md) |
| Session management | ✅ Complete | P0 | [Spec: API](specs/api-contracts.md) |
| Token refresh | ✅ Complete | P0 | [Spec: ATProto](specs/atproto-integration.md) |
| **Authorization** |
| Zanzibar permission model | ✅ Complete | P0 | [ADR-0009](adrs/0009-zanzibar-permissions.md), [Spec: Permissions](specs/permission-model.md) |
| Permission tuple storage | ✅ Complete | P0 | [Spec: Permissions](specs/permission-model.md) |
| Permission check API | ✅ Complete | P0 | [Spec: Permissions](specs/permission-model.md) |
| MUC affiliation sync | ✅ Complete | P0 | [RFC-0002](rfcs/0002-channels.md), [Spec: XMPP](specs/xmpp-integration.md) |
| **Core Messaging** |
| Message schema | ✅ Complete | P0 | [RFC-0004](rfcs/0004-message-format.md), [Spec: Messages](specs/message-schema.md) |
| Send message (XMPP) | ✅ Complete | P0 | [RFC-0004](rfcs/0004-message-format.md) |
| Message history (MAM) | ✅ Complete | P0 | [RFC-0004](rfcs/0004-message-format.md) |
| Real-time delivery (XMPP) | ✅ Complete | P0 | [Spec: XMPP](specs/xmpp-integration.md) |
| Edit message (XEP-0308) | ⬜ Not Started | P2 | [RFC-0004](rfcs/0004-message-format.md) |
| Delete message (XEP-0424) | ⬜ Not Started | P2 | [RFC-0004](rfcs/0004-message-format.md) |
| **Waddles (Communities)** |
| Waddle CRUD | ✅ Complete | P0 | [RFC-0001](rfcs/0001-waddles.md) |
| Member management | ✅ Complete | P0 | [RFC-0001](rfcs/0001-waddles.md) |
| Invite system | ⬜ Not Started | P2 | [RFC-0001](rfcs/0001-waddles.md) |
| Role management | ⬜ Not Started | P2 | [RFC-0001](rfcs/0001-waddles.md), [Spec: Permissions](specs/permission-model.md) |
| **Channels** |
| Channel CRUD (MUC provisioning) | ✅ Complete | P0 | [RFC-0002](rfcs/0002-channels.md) |
| Channel permissions | ✅ Complete | P0 | [RFC-0002](rfcs/0002-channels.md), [Spec: Permissions](specs/permission-model.md) |
| Categories | ⬜ Not Started | P3 | [RFC-0002](rfcs/0002-channels.md) |
| **CLI TUI Client** |
| Ratatui setup | ✅ Complete | P0 | [ADR-0003](adrs/0003-ratatui-cli.md), [Spec: CLI](specs/cli-commands.md) |
| XMPP client integration | 🔄 In Progress | P0 | [Spec: CLI](specs/cli-commands.md), [Spec: XMPP](specs/xmpp-integration.md) |
| Layout (sidebar, messages, input) | ✅ Complete | P0 | [Spec: CLI](specs/cli-commands.md) |
| Keybindings (Vim-style) | ✅ Complete | P0 | [Spec: CLI](specs/cli-commands.md) |
| Markdown rendering | ⬜ Not Started | P2 | [Spec: CLI](specs/cli-commands.md) |
| Configuration file | 🔄 In Progress | P1 | [Spec: CLI](specs/cli-commands.md) |

### Phase 2: Rich Features

Enhanced messaging and collaboration features.

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| **Rich Messages** |
| XHTML-IM formatting | ⬜ Not Started | P2 | [RFC-0004](rfcs/0004-message-format.md) |
| Mentions (XEP-0372) | ⬜ Not Started | P2 | [RFC-0004](rfcs/0004-message-format.md) |
| Reactions (XEP-0444) | ⬜ Not Started | P2 | [RFC-0004](rfcs/0004-message-format.md) |
| Replies (XEP-0461) | ✅ Complete | P2 | [RFC-0004](rfcs/0004-message-format.md) |
| Threads (root-thread model) | 🔄 In Progress | P3 | [RFC-0002](rfcs/0002-channels.md), [RFC-0004](rfcs/0004-message-format.md) |
| **File Uploads** |
| S3 storage setup | ⬜ Not Started | P2 | [ADR-0011](adrs/0011-self-hosted-storage.md), [Spec: Uploads](specs/file-upload.md) |
| HTTP File Upload (XEP-0363) | ✅ Complete | P2 | [Spec: Uploads](specs/file-upload.md), [Spec: XMPP](specs/xmpp-integration.md) |
| Image processing (thumbnails) | ⬜ Not Started | P3 | [Spec: Uploads](specs/file-upload.md) |
| Link embeds | ⬜ Not Started | P3 | [RFC-0004](rfcs/0004-message-format.md) |
| **Direct Messages** |
| 1:1 DM (XMPP chat) | ⬜ Not Started | P2 | [RFC-0003](rfcs/0003-direct-messages.md) |
| Group DMs (private MUC) | ⬜ Not Started | P3 | [RFC-0003](rfcs/0003-direct-messages.md) |
| DM requests/approval | ⬜ Not Started | P3 | [RFC-0003](rfcs/0003-direct-messages.md) |
| Privacy controls | ⬜ Not Started | P3 | [RFC-0003](rfcs/0003-direct-messages.md) |
| **Presence** |
| Online/offline status (XMPP presence) | ⬜ Not Started | P2 | [RFC-0006](rfcs/0006-presence-system.md) |
| Custom status | ⬜ Not Started | P3 | [RFC-0006](rfcs/0006-presence-system.md) |
| Per-Waddle presence | ⬜ Not Started | P3 | [RFC-0006](rfcs/0006-presence-system.md) |
| Typing indicators (XEP-0085) | ⬜ Not Started | P2 | [RFC-0006](rfcs/0006-presence-system.md) |
| **Ephemeral Content** |
| Message TTL configuration | ⬜ Not Started | P3 | [RFC-0005](rfcs/0005-ephemeral-content.md) |
| Prosody expiry module | ⬜ Not Started | P3 | [RFC-0005](rfcs/0005-ephemeral-content.md) |
| Channel-level TTL | ⬜ Not Started | P3 | [RFC-0005](rfcs/0005-ephemeral-content.md) |
| **Search** |
| Full-text search (FTS5) | ⬜ Not Started | P3 | [RFC-0012](rfcs/0012-search.md) |
| Search API | ⬜ Not Started | P3 | [RFC-0012](rfcs/0012-search.md), [Spec: API](specs/api-contracts.md) |
| Search filters | ⬜ Not Started | P4 | [RFC-0012](rfcs/0012-search.md) |
| **End-to-End Encryption** |
| OMEMO (XEP-0384) | ⬜ Not Started | P3 | [RFC-0004](rfcs/0004-message-format.md) |

### Phase 3: Moderation & AI

Trust and safety features plus AI-powered enhancements.

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| **Moderation** |
| Timeout/kick/ban | ⬜ Not Started | P3 | [RFC-0013](rfcs/0013-moderation.md) |
| User reports | ⬜ Not Started | P3 | [RFC-0013](rfcs/0013-moderation.md) |
| Moderation queue | ⬜ Not Started | P3 | [RFC-0013](rfcs/0013-moderation.md) |
| Audit log | ⬜ Not Started | P3 | [RFC-0013](rfcs/0013-moderation.md) |
| Automod rules | ⬜ Not Started | P4 | [RFC-0013](rfcs/0013-moderation.md) |
| Ban appeals | ⬜ Not Started | P4 | [RFC-0013](rfcs/0013-moderation.md) |
| **AI Features** |
| AI provider abstraction | ⬜ Not Started | P4 | [RFC-0007](rfcs/0007-ai-integrations.md) |
| Message summarization | ⬜ Not Started | P4 | [RFC-0007](rfcs/0007-ai-integrations.md) |
| AI content moderation | ⬜ Not Started | P4 | [RFC-0007](rfcs/0007-ai-integrations.md), [RFC-0013](rfcs/0013-moderation.md) |
| Translation | ⬜ Not Started | P4 | [RFC-0007](rfcs/0007-ai-integrations.md) |
| Semantic search | ⬜ Not Started | P4 | [RFC-0007](rfcs/0007-ai-integrations.md), [RFC-0012](rfcs/0012-search.md) |

### Phase 4: Interactive Features

Real-time collaborative features.

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| **Watch Together** |
| Watch session management | ⬜ Not Started | P4 | [RFC-0008](rfcs/0008-watch-together.md) |
| Playback synchronization | ⬜ Not Started | P4 | [RFC-0008](rfcs/0008-watch-together.md) |
| Media source support | ⬜ Not Started | P4 | [RFC-0008](rfcs/0008-watch-together.md) |
| Queue system | ⬜ Not Started | P4 | [RFC-0008](rfcs/0008-watch-together.md) |
| **Screen Sharing** |
| Jingle signaling (XEP-0166) | ⬜ Not Started | P4 | [RFC-0009](rfcs/0009-screen-sharing.md) |
| SFU integration | ⬜ Not Started | P4 | [RFC-0009](rfcs/0009-screen-sharing.md) |
| Quality settings | ⬜ Not Started | P4 | [RFC-0009](rfcs/0009-screen-sharing.md) |
| Remote control | ⬜ Not Started | P4 | [RFC-0009](rfcs/0009-screen-sharing.md) |
| **Live Streaming** |
| RTMP ingest | ⬜ Not Started | P4 | [RFC-0010](rfcs/0010-live-streaming.md) |
| Transcoding pipeline | ⬜ Not Started | P4 | [RFC-0010](rfcs/0010-live-streaming.md) |
| HLS/WebRTC delivery | ⬜ Not Started | P4 | [RFC-0010](rfcs/0010-live-streaming.md) |
| VOD recording | ⬜ Not Started | P4 | [RFC-0010](rfcs/0010-live-streaming.md) |

### Phase 5: Integrations & Extensibility

External integrations and bot platform.

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| **Bluesky Integration** |
| Announcement posting | ⬜ Not Started | P3 | [RFC-0011](rfcs/0011-bluesky-broadcast.md), [Spec: ATProto](specs/atproto-integration.md) |
| Rich text conversion | ⬜ Not Started | P3 | [RFC-0011](rfcs/0011-bluesky-broadcast.md) |
| Image upload to PDS | ⬜ Not Started | P4 | [RFC-0011](rfcs/0011-bluesky-broadcast.md) |
| Broadcast permissions | ⬜ Not Started | P3 | [RFC-0011](rfcs/0011-bluesky-broadcast.md) |
| **Bot Framework** |
| Bot authentication | ⬜ Not Started | P4 | [RFC-0014](rfcs/0014-bot-framework.md) |
| XMPP bot accounts | ⬜ Not Started | P4 | [RFC-0014](rfcs/0014-bot-framework.md) |
| Slash commands | ⬜ Not Started | P4 | [RFC-0014](rfcs/0014-bot-framework.md) |
| Bot SDK | ⬜ Not Started | P4 | [RFC-0014](rfcs/0014-bot-framework.md) |
| AI assistants | ⬜ Not Started | P4 | [RFC-0014](rfcs/0014-bot-framework.md), [RFC-0007](rfcs/0007-ai-integrations.md) |
| Bot marketplace | ⬜ Not Started | P4 | [RFC-0014](rfcs/0014-bot-framework.md) |

---

## Documentation Status

### Architecture Decision Records (ADRs)

| ADR | Title | Status |
|-----|-------|--------|
| [0001](adrs/0001-rust-backend.md) | Use Rust for Backend | ✅ Accepted |
| [0002](adrs/0002-axum-web-framework.md) | Use Axum for HTTP | ✅ Accepted |
| [0003](adrs/0003-ratatui-cli.md) | Use Ratatui for CLI TUI | ✅ Accepted |
| [0004](adrs/0004-turso-libsql-database.md) | Use Turso/libSQL for Storage | ✅ Accepted |
| [0005](adrs/0005-atproto-identity.md) | ATProto OAuth for Identity | ✅ Accepted |
| [0006](adrs/0006-xmpp-protocol.md) | Native Rust XMPP Server | ✅ Accepted |
| [0007](adrs/0007-cqrs-architecture.md) | CQRS Pattern for Data | ✅ Accepted |
| [0008](adrs/0008-kameo-actors.md) | Kameo Actor Framework | ✅ Accepted |
| [0009](adrs/0009-zanzibar-permissions.md) | Zanzibar-Inspired Authorization | ✅ Accepted |
| [0010](adrs/0010-agpl-licensing.md) | AGPL-3.0 License | ✅ Accepted |
| [0011](adrs/0011-self-hosted-storage.md) | S3-Compatible File Storage | ✅ Accepted |
| [0012](adrs/0012-transport-encryption.md) | Transport-Only Encryption | ✅ Accepted |
| [0014](adrs/0014-opentelemetry.md) | OpenTelemetry Instrumentation | ✅ Accepted |
| [0015](adrs/0015-dual-authentication.md) | Dual Authentication Modes | ✅ Accepted |

### RFCs (Feature Proposals)

| RFC | Title | Status |
|-----|-------|--------|
| [0001](rfcs/0001-waddles.md) | Waddles (Communities) | 📝 Draft |
| [0002](rfcs/0002-channels.md) | Channel System (MUC) | 📝 Draft |
| [0003](rfcs/0003-direct-messages.md) | Direct Messages (XMPP) | 📝 Draft |
| [0004](rfcs/0004-message-format.md) | Rich Message Format (XEPs) | 📝 Draft |
| [0005](rfcs/0005-ephemeral-content.md) | Ephemeral Content | 📝 Draft |
| [0006](rfcs/0006-presence-system.md) | Presence & Status (XMPP) | 📝 Draft |
| [0007](rfcs/0007-ai-integrations.md) | AI Features | 📝 Draft |
| [0008](rfcs/0008-watch-together.md) | Watch Together | 📝 Draft |
| [0009](rfcs/0009-screen-sharing.md) | Screen Sharing | 📝 Draft |
| [0010](rfcs/0010-live-streaming.md) | Live Streaming | 📝 Draft |
| [0011](rfcs/0011-bluesky-broadcast.md) | Bluesky Announcements | 📝 Draft |
| [0012](rfcs/0012-search.md) | Full-Text Search | 📝 Draft |
| [0013](rfcs/0013-moderation.md) | Moderation System | 📝 Draft |
| [0014](rfcs/0014-bot-framework.md) | Bot/Assistant Framework | 📝 Draft |
| [0015](rfcs/0015-federation-architecture.md) | Federation Architecture | 📝 Draft |

### Technical Specifications

| Spec | Title | Status |
|------|-------|--------|
| [xmpp-integration](specs/xmpp-integration.md) | XMPP Integration | 📝 Draft |
| [message-schema](specs/message-schema.md) | Message Data Schema | 📝 Draft |
| [api-contracts](specs/api-contracts.md) | REST/HTTP API | 📝 Draft |
| [permission-model](specs/permission-model.md) | Permission Schema | 📝 Draft |
| [event-schema](specs/event-schema.md) | Event Types | 📝 Draft |
| [cli-commands](specs/cli-commands.md) | CLI TUI Specification | 📝 Draft |
| [atproto-integration](specs/atproto-integration.md) | ATProto Integration | 📝 Draft |
| [file-upload](specs/file-upload.md) | File Upload Protocol | 📝 Draft |
| [s2s-federation](specs/s2s-federation.md) | S2S Federation | 📝 Draft |

---

## Priority Definitions

| Priority | Meaning | Target |
|----------|---------|--------|
| **P0** | Critical for federation MVP & core compliance | Phase F1-F4, Phase XC1-XC2 |
| **P1** | Important for full federation & advanced compliance | Phase F5, Phase XC3-XC4, Phase 1 |
| **P2** | Enhances experience | Phase 1-2 |
| **P3** | Nice to have | Phase 2-3 |
| **P4** | Future consideration | Phase 3-5 |

## Status Legend

| Symbol | Meaning |
|--------|---------|
| ⬜ | Not Started |
| 🔄 | In Progress |
| ✅ | Complete |
| ⏸️ | On Hold |
| ❌ | Blocked |

---

## Milestones

### MF1: Native JID Authentication
- [x] SCRAM-SHA-256 mechanism implemented (waddle-xmpp/src/auth/scram.rs)
- [x] XEP-0077 registration working (waddle-xmpp/src/xep/xep0077.rs)
- [x] Native user can login via standard XMPP client
- [x] Password hashing with Argon2id (waddle-server/src/auth/native.rs)

### MF2: Server Modes ✅ COMPLETE
- [x] `WADDLE_MODE=standalone` disables ATProto
- [x] `WADDLE_MODE=homeserver` runs full stack
- [x] Mode-specific feature flags working

### MF3: S2S Federation
- [ ] S2S listener on 5269
- [ ] Server dialback (XEP-0220) working
- [ ] Two waddle instances can exchange messages
- [ ] DNS SRV resolution working

### MF4: Federated MUC
- [x] Remote user can join local MUC
- [x] Presence broadcasts to remote occupants
- [ ] Messages route to remote occupants
- [ ] Mixed local/remote channel working

### MF5: Hosted Waddles
- [ ] Subdomain provisioning API
- [ ] `general@penguin.waddle.social` routes correctly
- [ ] Per-waddle database isolation

### MXC1: XEP-0479 Core Compliance ✅ COMPLETE
- [x] XEP-0115 Entity Capabilities implemented (xep/xep0115.rs)
- [x] Capabilities advertised in presence (Caps::build_element())
- [x] disco#info responds with capability hash (parse_caps_node())

### MXC2: XEP-0479 IM Basic Compliance
- [x] RFC 6121 roster management working
- [ ] Presence subscription flow complete
- [x] XEP-0054 vcard-temp working
- [x] XEP-0249 Direct MUC Invitations working
- [x] XEP-0045 MUC fully compliant
- [x] XEP-0280 Message Carbons integrated
- [x] XEP-0363 HTTP File Upload working

### MXC3: XEP-0479 IM Advanced Compliance
- [x] XEP-0313 MAM fully working
- [x] XEP-0198 Stream Management complete
- [x] XEP-0048 Bookmark Storage working (legacy compat over XEP-0402)
- [x] XEP-0049 Private XML Storage working
- [x] XEP-0060 PubSub service discoverable and functional
- [x] XEP-0084 User Avatar working (PEP-based)
- [x] XEP-0153 vCard-Based Avatars working
- [x] XEP-0163 PEP discoverable and functional
- [x] XEP-0191 Blocking Command working
- [x] XEP-0223 Persistent Storage best practices
- [x] XEP-0398 Avatar Conversion (PEP↔vCard bridge)
- [x] XEP-0402 PEP Native Bookmarks working
- [x] XEP-0410 MUC Self-Ping working

### MXC4: XEP-0479 Mobile Compliance
- [x] XEP-0352 Client State Indication working

### M0: XMPP Foundation
- [x] waddle-xmpp crate created
- [x] WebSocket C2S endpoint served at `/xmpp-websocket`
- [x] RFC 7395 framing working
- [x] Stream negotiation completes
- [x] SASL authentication working (PLAIN mechanism)
- [x] XML stanza parsing with RFC 6120 compliance (minidom/rxml)
- [x] Error stanza generation (RFC 6120 Section 8.3)
- [ ] OpenTelemetry traces visible
- [x] RFC 6120 core interop tests passing

### M1: Hello Waddle (MVP)
- [x] User can authenticate via Bluesky
- [x] XMPP account provisioned from DID
- [x] User can create a Waddle
- [x] User can create channels (MUC rooms)
- [x] User can send/receive messages in CLI via XMPP
- [x] Messages delivered in real-time

### M2: Rich Messaging
- [x] File uploads working (XEP-0363)
- [ ] XHTML-IM rendering
- [ ] Reactions and replies (XEP-0444, XEP-0461)
- [ ] Direct messages (XMPP 1:1)
- [ ] Presence indicators

### M3: Community Ready
- [ ] Moderation tools
- [ ] Search functionality
- [ ] Ephemeral messages
- [ ] Bluesky announcements
- [ ] OMEMO encryption

### M4: Interactive
- [ ] Watch Together
- [ ] Screen sharing (Jingle)
- [ ] Bot framework

---

## Quick Links

- **Architecture**: [ADRs](adrs/)
- **Features**: [RFCs](rfcs/)
- **Technical Details**: [Specs](specs/)
- **Dependencies**: [Rust Crates](RUST_CRATES.md)
- **Federation**: [RFC-0015](rfcs/0015-federation-architecture.md)
