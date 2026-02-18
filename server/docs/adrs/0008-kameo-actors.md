# ADR-0008: Kameo Actor Framework

## Status

Accepted

## Context

Waddle Social requires concurrent handling of:

- Background tasks (TTL cleanup, notifications, Bluesky sync)
- ATProto token refresh
- XMPP account provisioning events

With XMPP handling real-time messaging (see [ADR-0006](./0006-xmpp-protocol.md)), the actor model scope is reduced but still valuable for background processing.

Actor model options:

- **Tokio Tasks**: Raw async tasks, manual synchronization
- **Actix Actors**: Mature, but tied to Actix ecosystem
- **Kameo**: Pure Rust actors on Tokio, lightweight
- **Bastion**: Erlang-inspired supervision, but heavier

## Decision

We will use **Kameo** as our actor framework for backend background tasks.

## Consequences

### Positive

- **Tokio Native**: Built on Tokio, integrates with existing async code
- **Lightweight**: Minimal overhead, actors are just async tasks
- **Type-Safe Messages**: Rust enums for message types, compile-time guarantees
- **Supervision**: Built-in actor supervision and restart strategies
- **No Framework Lock-in**: Can gradually adopt or replace actors

### Negative

- **Younger Library**: Less mature than Actix actors
- **Smaller Community**: Fewer examples and documentation

### Neutral

- **Reduced Scope**: XMPP server handles session management and message routing

## Implementation Notes

Actor types planned:

- `CleanupActor`: Periodic TTL enforcement for ephemeral messages (coordinates with XMPP MAM)
- `TokenRefreshActor`: ATProto token refresh before expiry
- `ProvisioningActor`: XMPP account creation when users authenticate
- `BlueskyActor`: Handles announcement broadcasts to Bluesky

## Related

- [ADR-0001: Rust Backend](./0001-rust-backend.md)
- [ADR-0006: XMPP Protocol](./0006-xmpp-protocol.md)
- [ADR-0007: CQRS Architecture](./0007-cqrs-architecture.md)
- [RFC-0005: Ephemeral Content](../rfcs/0005-ephemeral-content.md)
