# RFC-0015: Federation Architecture

**Status:** Draft
**Priority:** P0 (Critical)
**Created:** 2025-01-21

## Summary

This RFC defines the federated XMPP architecture for Waddle Social, enabling a distributed ecosystem where:

1. **waddle.social** acts as the identity home-server for ATProto users
2. Anyone can run self-hosted waddles (independent XMPP servers)
3. Users can federate across waddles using standard XMPP S2S
4. Traditional JID users can participate without ATProto

## Motivation

The current architecture requires ATProto authentication for all users and operates as a single monolithic server. This limits:

- **Self-hosting**: Users cannot run their own waddle instances
- **Interoperability**: Standard XMPP clients/users cannot participate
- **Decentralization**: All users depend on waddle.social infrastructure

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│                    waddle.social (Home Server)                          │
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────┐ │
│  │ ATProto OAuth   │  │ XMPP Server     │  │ Hosted Waddles          │ │
│  │ Identity Issuer │  │ (single domain) │  │ MUC namespaced          │ │
│  │                 │  │                 │  │ general@penguin.w.s     │ │
│  │ did:plc:abc →   │  │ C2S: 5222       │  │ general@devs.w.s        │ │
│  │ abc@waddle.soc  │  │ S2S: 5269       │  │ Each has own SQLite     │ │
│  └─────────────────┘  └─────────────────┘  └─────────────────────────┘ │
└───────────────────────────────┬─────────────────────────────────────────┘
                                │ S2S Federation (XMPP)
        ┌───────────────────────┼───────────────────────┐
        │                       │                       │
        ▼                       ▼                       ▼
┌───────────────────┐  ┌───────────────────┐  ┌───────────────────┐
│ alice.dev         │  │ bob.io            │  │ company.internal  │
│ (self-hosted)     │  │ (self-hosted)     │  │ (self-hosted)     │
├───────────────────┤  ├───────────────────┤  ├───────────────────┤
│ XMPP Server       │  │ XMPP Server       │  │ XMPP Server       │
│ C2S: 5222         │  │ C2S: 5222         │  │ C2S: 5222         │
│ S2S: 5269         │  │ S2S: 5269         │  │ S2S: 5269         │
├───────────────────┤  ├───────────────────┤  ├───────────────────┤
│ Native JID users: │  │ Native JID users: │  │ Native JID users: │
│ - dave@alice.dev  │  │ - eve@bob.io      │  │ - frank@company.. │
│                   │  │                   │  │                   │
│ Federated users:  │  │ Federated users:  │  │ Federated users:  │
│ - abc@waddle.soc  │  │ - dave@alice.dev  │  │ - abc@waddle.soc  │
│   (ATProto)       │  │ - abc@waddle.soc  │  │ - eve@bob.io      │
└───────────────────┘  └───────────────────┘  └───────────────────┘
```

## Identity Types

| Identity | Example | Home Server | Auth Method | Can Federate |
|----------|---------|-------------|-------------|--------------|
| ATProto User | `abc123@waddle.social` | waddle.social | ATProto OAuth | Yes (S2S) |
| Native JID | `dave@alice.dev` | alice.dev | SCRAM-SHA-256 | Yes (S2S) |
| Hosted Waddle | `general@penguin.waddle.social` | waddle.social | N/A (MUC) | Yes |

## Server Modes

### Home Server Mode (waddle.social)

The global waddle.social instance operates as an identity provider:

- Issues XMPP credentials for ATProto-authenticated users
- Hosts multi-tenant "waddles" as MUC namespaces
- Federates with self-hosted waddles via S2S

```rust
ServerMode::HomeServer {
    domain: "waddle.social",
    atproto_enabled: true,
    federation_enabled: true,
    hosted_waddles: true,
}
```

### Standalone Waddle Mode (self-hosted)

Independent XMPP servers anyone can run:

- Native JID registration (XEP-0077)
- SCRAM-SHA-256 authentication
- Optional ATProto integration
- S2S federation with other waddles

```rust
ServerMode::StandaloneWaddle {
    domain: "alice.dev",
    atproto_enabled: false,
    federation_enabled: true,
    local_auth_enabled: true,
}
```

## Authentication Paths

### Path A: ATProto Identity (federated)

```
1. User authenticates via ATProto OAuth at waddle.social
2. Home server issues XMPP credential (session token)
3. User connects to ANY waddle with credential
4. Remote waddle verifies via S2S with home server
```

### Path B: Native JID (local)

```
1. User registers JID directly on a waddle (XEP-0077)
2. Standard XMPP SASL SCRAM-SHA-256 authentication
3. User is local to that waddle
4. Federates to other waddles via standard S2S
```

## Hosted Waddles (Multi-tenant)

Users who don't self-host get a waddle on waddle.social:

- Subdomain: `penguin.waddle.social`
- MUC namespace: `#general@penguin.waddle.social`
- Own SQLite database: `/data/waddles/penguin.db`
- Shared identity infrastructure

```rust
struct HostedWaddle {
    id: Uuid,
    subdomain: String,              // "penguin"
    full_domain: String,            // "penguin.waddle.social"
    database_path: PathBuf,         // /data/waddles/penguin.db
    owner_did: String,              // Creator's ATProto DID
}
```

## User Journeys

### Journey 1: ATProto User on Hosted Waddle

```
1. User logs in with Bluesky (ATProto OAuth) at waddle.social
2. Gets JID: abc123@waddle.social
3. Creates/joins hosted waddle "penguin"
4. Joins channel: general@penguin.waddle.social
5. Can also join self-hosted waddles via S2S federation
```

### Journey 2: ATProto User on Self-Hosted Waddle

```
1. User logs in with Bluesky at waddle.social (gets abc123@waddle.social)
2. Waddle CLI connects to alice.dev (self-hosted waddle)
3. S2S federation verifies abc123@waddle.social is valid
4. User joins channels on alice.dev as federated user
```

### Journey 3: Native JID User (No ATProto)

```
1. User registers directly on alice.dev via XMPP (XEP-0077)
2. Gets JID: dave@alice.dev
3. Uses any XMPP client (Conversations, Gajim, etc.)
4. Can join channels on alice.dev (home)
5. Can federate to bob.io or waddle.social via S2S
6. Does NOT have ATProto identity, cannot use Bluesky features
```

### Journey 4: Mixed Waddle

```
Channel: #general@alice.dev

Participants:
- dave@alice.dev      (native, owner)
- eve@bob.io          (native, federated from bob.io)
- abc123@waddle.soc   (ATProto, federated from waddle.social)

All can chat together via standard XMPP MUC.
```

## Key Decisions

1. **Native JID users CAN federate** - Via standard XMPP S2S. Their identity is their full JID.

2. **MUC namespacing for hosted waddles** - Share one XMPP server on waddle.social, waddles distinguished by MUC room JIDs.

3. **Only waddle.social is the ATProto home server** - Centralized identity provider. Self-hosted waddles accept federated users but don't issue ATProto credentials.

## Implementation Phases

See [PROJECT_MANAGEMENT.md](../PROJECT_MANAGEMENT.md) for detailed implementation tracking:

- **Phase F1**: Native JID Authentication (SCRAM-SHA-256, XEP-0077)
- **Phase F2**: Server Mode Configuration
- **Phase F3**: S2S Federation Core (XEP-0220)
- **Phase F4**: Federated MUC Participation
- **Phase F5**: Hosted Waddle Subdomains

## Related Documents

- [ADR-0015: Dual Authentication Modes](../adrs/0015-dual-authentication.md)
- [Spec: S2S Federation](../specs/s2s-federation.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [ADR-0005: ATProto Identity](../adrs/0005-atproto-identity.md)

## Security Considerations

- S2S connections MUST use TLS 1.3
- Server dialback (XEP-0220) for identity verification
- Rate limiting on federation endpoints
- Allowlist/blocklist for federated servers

## Open Questions

1. Should hosted waddles support custom domains (CNAME)?
2. Federation trust levels (open, allowlist, blocklist)?
3. Cross-server moderation coordination?
