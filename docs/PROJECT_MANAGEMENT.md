# Waddle Social - Project Management

## Overview

This document tracks implementation progress for Waddle Social, an open-source consumer chat/communication platform with ATProto integration.

**License**: AGPL-3.0
**MVP Target**: CLI TUI client with core messaging

---

## Implementation Phases

### Phase 1: Foundation (MVP)

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
| CQRS event system | ⬜ Not Started | P1 | [ADR-0007](adrs/0007-cqrs-architecture.md), [Spec: Events](specs/event-schema.md) |
| Kameo actor setup | ⬜ Not Started | P1 | [ADR-0008](adrs/0008-kameo-actors.md) |
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
| Message schema | 🔄 In Progress | P0 | [RFC-0004](rfcs/0004-message-format.md), [Spec: Messages](specs/message-schema.md) |
| Send message (XMPP) | ⬜ Not Started | P0 | [RFC-0004](rfcs/0004-message-format.md) |
| Message history (MAM) | 🔄 In Progress | P0 | [RFC-0004](rfcs/0004-message-format.md) |
| Real-time delivery (XMPP) | ⬜ Not Started | P0 | [Spec: XMPP](specs/xmpp-integration.md) |
| Edit message (XEP-0308) | ⬜ Not Started | P1 | [RFC-0004](rfcs/0004-message-format.md) |
| Delete message (XEP-0424) | ⬜ Not Started | P1 | [RFC-0004](rfcs/0004-message-format.md) |
| **Waddles (Communities)** |
| Waddle CRUD | ✅ Complete | P0 | [RFC-0001](rfcs/0001-waddles.md) |
| Member management | ✅ Complete | P0 | [RFC-0001](rfcs/0001-waddles.md) |
| Invite system | ⬜ Not Started | P1 | [RFC-0001](rfcs/0001-waddles.md) |
| Role management | ⬜ Not Started | P1 | [RFC-0001](rfcs/0001-waddles.md), [Spec: Permissions](specs/permission-model.md) |
| **Channels** |
| Channel CRUD (MUC provisioning) | ✅ Complete | P0 | [RFC-0002](rfcs/0002-channels.md) |
| Channel permissions | ✅ Complete | P0 | [RFC-0002](rfcs/0002-channels.md), [Spec: Permissions](specs/permission-model.md) |
| Categories | ⬜ Not Started | P2 | [RFC-0002](rfcs/0002-channels.md) |
| **CLI TUI Client** |
| Ratatui setup | ✅ Complete | P0 | [ADR-0003](adrs/0003-ratatui-cli.md), [Spec: CLI](specs/cli-commands.md) |
| XMPP client integration | 🔄 In Progress | P0 | [Spec: CLI](specs/cli-commands.md), [Spec: XMPP](specs/xmpp-integration.md) |
| Layout (sidebar, messages, input) | ✅ Complete | P0 | [Spec: CLI](specs/cli-commands.md) |
| Keybindings (Vim-style) | ✅ Complete | P0 | [Spec: CLI](specs/cli-commands.md) |
| Markdown rendering | ⬜ Not Started | P1 | [Spec: CLI](specs/cli-commands.md) |
| Configuration file | 🔄 In Progress | P1 | [Spec: CLI](specs/cli-commands.md) |

### Phase 2: Rich Features

Enhanced messaging and collaboration features.

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| **Rich Messages** |
| XHTML-IM formatting | ⬜ Not Started | P1 | [RFC-0004](rfcs/0004-message-format.md) |
| Mentions (XEP-0372) | ⬜ Not Started | P1 | [RFC-0004](rfcs/0004-message-format.md) |
| Reactions (XEP-0444) | ⬜ Not Started | P1 | [RFC-0004](rfcs/0004-message-format.md) |
| Replies (XEP-0461) | ⬜ Not Started | P1 | [RFC-0004](rfcs/0004-message-format.md) |
| Threads | ⬜ Not Started | P2 | [RFC-0002](rfcs/0002-channels.md), [RFC-0004](rfcs/0004-message-format.md) |
| **File Uploads** |
| S3 storage setup | ⬜ Not Started | P1 | [ADR-0011](adrs/0011-self-hosted-storage.md), [Spec: Uploads](specs/file-upload.md) |
| HTTP File Upload (XEP-0363) | ⬜ Not Started | P1 | [Spec: Uploads](specs/file-upload.md), [Spec: XMPP](specs/xmpp-integration.md) |
| Image processing (thumbnails) | ⬜ Not Started | P2 | [Spec: Uploads](specs/file-upload.md) |
| Link embeds | ⬜ Not Started | P2 | [RFC-0004](rfcs/0004-message-format.md) |
| **Direct Messages** |
| 1:1 DM (XMPP chat) | ⬜ Not Started | P1 | [RFC-0003](rfcs/0003-direct-messages.md) |
| Group DMs (private MUC) | ⬜ Not Started | P2 | [RFC-0003](rfcs/0003-direct-messages.md) |
| DM requests/approval | ⬜ Not Started | P2 | [RFC-0003](rfcs/0003-direct-messages.md) |
| Privacy controls | ⬜ Not Started | P2 | [RFC-0003](rfcs/0003-direct-messages.md) |
| **Presence** |
| Online/offline status (XMPP presence) | ⬜ Not Started | P1 | [RFC-0006](rfcs/0006-presence-system.md) |
| Custom status | ⬜ Not Started | P2 | [RFC-0006](rfcs/0006-presence-system.md) |
| Per-Waddle presence | ⬜ Not Started | P2 | [RFC-0006](rfcs/0006-presence-system.md) |
| Typing indicators (XEP-0085) | ⬜ Not Started | P1 | [RFC-0006](rfcs/0006-presence-system.md) |
| **Ephemeral Content** |
| Message TTL configuration | ⬜ Not Started | P2 | [RFC-0005](rfcs/0005-ephemeral-content.md) |
| Prosody expiry module | ⬜ Not Started | P2 | [RFC-0005](rfcs/0005-ephemeral-content.md) |
| Channel-level TTL | ⬜ Not Started | P2 | [RFC-0005](rfcs/0005-ephemeral-content.md) |
| **Search** |
| Full-text search (FTS5) | ⬜ Not Started | P2 | [RFC-0012](rfcs/0012-search.md) |
| Search API | ⬜ Not Started | P2 | [RFC-0012](rfcs/0012-search.md), [Spec: API](specs/api-contracts.md) |
| Search filters | ⬜ Not Started | P3 | [RFC-0012](rfcs/0012-search.md) |
| **End-to-End Encryption** |
| OMEMO (XEP-0384) | ⬜ Not Started | P2 | [RFC-0004](rfcs/0004-message-format.md) |

### Phase 3: Moderation & AI

Trust and safety features plus AI-powered enhancements.

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| **Moderation** |
| Timeout/kick/ban | ⬜ Not Started | P2 | [RFC-0013](rfcs/0013-moderation.md) |
| User reports | ⬜ Not Started | P2 | [RFC-0013](rfcs/0013-moderation.md) |
| Moderation queue | ⬜ Not Started | P2 | [RFC-0013](rfcs/0013-moderation.md) |
| Audit log | ⬜ Not Started | P2 | [RFC-0013](rfcs/0013-moderation.md) |
| Automod rules | ⬜ Not Started | P3 | [RFC-0013](rfcs/0013-moderation.md) |
| Ban appeals | ⬜ Not Started | P3 | [RFC-0013](rfcs/0013-moderation.md) |
| **AI Features** |
| AI provider abstraction | ⬜ Not Started | P3 | [RFC-0007](rfcs/0007-ai-integrations.md) |
| Message summarization | ⬜ Not Started | P3 | [RFC-0007](rfcs/0007-ai-integrations.md) |
| AI content moderation | ⬜ Not Started | P3 | [RFC-0007](rfcs/0007-ai-integrations.md), [RFC-0013](rfcs/0013-moderation.md) |
| Translation | ⬜ Not Started | P3 | [RFC-0007](rfcs/0007-ai-integrations.md) |
| Semantic search | ⬜ Not Started | P3 | [RFC-0007](rfcs/0007-ai-integrations.md), [RFC-0012](rfcs/0012-search.md) |

### Phase 4: Interactive Features

Real-time collaborative features.

| Task | Status | Priority | Documentation |
|------|--------|----------|---------------|
| **Watch Together** |
| Watch session management | ⬜ Not Started | P3 | [RFC-0008](rfcs/0008-watch-together.md) |
| Playback synchronization | ⬜ Not Started | P3 | [RFC-0008](rfcs/0008-watch-together.md) |
| Media source support | ⬜ Not Started | P3 | [RFC-0008](rfcs/0008-watch-together.md) |
| Queue system | ⬜ Not Started | P4 | [RFC-0008](rfcs/0008-watch-together.md) |
| **Screen Sharing** |
| Jingle signaling (XEP-0166) | ⬜ Not Started | P3 | [RFC-0009](rfcs/0009-screen-sharing.md) |
| SFU integration | ⬜ Not Started | P3 | [RFC-0009](rfcs/0009-screen-sharing.md) |
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
| Announcement posting | ⬜ Not Started | P2 | [RFC-0011](rfcs/0011-bluesky-broadcast.md), [Spec: ATProto](specs/atproto-integration.md) |
| Rich text conversion | ⬜ Not Started | P2 | [RFC-0011](rfcs/0011-bluesky-broadcast.md) |
| Image upload to PDS | ⬜ Not Started | P3 | [RFC-0011](rfcs/0011-bluesky-broadcast.md) |
| Broadcast permissions | ⬜ Not Started | P2 | [RFC-0011](rfcs/0011-bluesky-broadcast.md) |
| **Bot Framework** |
| Bot authentication | ⬜ Not Started | P3 | [RFC-0014](rfcs/0014-bot-framework.md) |
| XMPP bot accounts | ⬜ Not Started | P3 | [RFC-0014](rfcs/0014-bot-framework.md) |
| Slash commands | ⬜ Not Started | P3 | [RFC-0014](rfcs/0014-bot-framework.md) |
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

---

## Priority Definitions

| Priority | Meaning | Target |
|----------|---------|--------|
| **P0** | Critical for MVP | Phase 1 |
| **P1** | Important for usability | Phase 1-2 |
| **P2** | Enhances experience | Phase 2-3 |
| **P3** | Nice to have | Phase 3-4 |
| **P4** | Future consideration | Phase 4-5 |

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

### M0: XMPP Foundation
- [x] waddle-xmpp crate created
- [x] TCP connections accepted on 5222
- [x] STARTTLS working
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
- [ ] User can send/receive messages in CLI via XMPP
- [ ] Messages delivered in real-time

### M2: Rich Messaging
- [ ] File uploads working (XEP-0363)
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

### M5: Federation
- [ ] S2S listener on 5269
- [ ] Server dialback (XEP-0220)
- [ ] Remote user presence
- [ ] Cross-instance MUC participation

---

## Quick Links

- **Architecture**: [ADRs](adrs/)
- **Features**: [RFCs](rfcs/)
- **Technical Details**: [Specs](specs/)
- **Dependencies**: [Rust Crates](RUST_CRATES.md)
