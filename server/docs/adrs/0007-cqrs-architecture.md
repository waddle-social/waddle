# ADR-0007: CQRS Pattern for Data

## Status

Accepted

## Context

Waddle Social has different read and write patterns:
- **Writes**: Message creation, reactions, edits (must be consistent, ordered)
- **Reads**: Message history, search, channel lists (can tolerate slight staleness)

Traditional CRUD approaches couple these concerns. We considered:
- **CRUD**: Simple, but write contention affects read performance
- **CQRS**: Separate read/write models, optimized for each
- **Event Sourcing**: Full event history, but storage overhead
- **Hybrid**: CQRS without full event sourcing

## Decision

We will implement **CQRS (Command Query Responsibility Segregation)** with event-driven updates.

## Consequences

### Positive

- **Optimized Models**: Write model ensures consistency; read models optimized for queries
- **Scalability**: Read replicas can scale independently of write path
- **Event-Driven**: Changes propagate as events; enables real-time updates
- **Flexibility**: Can add new read models without modifying write path
- **Audit Trail**: Events provide natural change history

### Negative

- **Eventual Consistency**: Read models may lag behind writes
- **Complexity**: Two models to maintain, sync logic required
- **Learning Curve**: Pattern is less familiar than CRUD
- **Debugging**: Tracing issues across event flows is harder

### Neutral

- **Not Full Event Sourcing**: We store current state, not full event history (simpler, less storage)

## Implementation Notes

- Commands: `SendMessage`, `AddReaction`, `EditMessage`, `DeleteMessage`
- Events: `MessageSent`, `ReactionAdded`, `MessageEdited`, `MessageDeleted`
- Read models: Denormalized channel views, search indices, unread counts
- Event bus: In-process initially; can externalize to NATS/Kafka later

## Related

- [ADR-0008: Kameo Actors](./0008-kameo-actors.md)
- [Spec: Event Schema](../specs/event-schema.md)
- [Spec: Message Schema](../specs/message-schema.md)
