# ADR-0004: Use Turso/libSQL for Storage

## Status

Accepted

## Context

Waddle Social needs a database that supports:
- Self-hosted deployments (no vendor lock-in)
- Edge distribution for low-latency global access
- Local-first potential for offline-capable clients
- ACID transactions for message ordering guarantees

### Multi-Database Architecture

Rather than a single monolithic database, Waddle uses a sharded approach:

- **Global Database**: Handles authentication, user profiles, and system configuration
- **Per-Waddle Databases**: Each Waddle (community) gets its own isolated database

This architecture provides natural data isolation between communities and distributes write load across multiple databases.

We evaluated:
- **PostgreSQL**: Robust, but complex for self-hosting, no edge story
- **SQLite**: Embedded, simple, but single-writer limitation
- **Turso/libSQL**: SQLite fork with edge replication, HTTP API
- **CockroachDB**: Distributed SQL, but heavy for small deployments
- **ScyllaDB/Cassandra**: Write-optimized, but complex operations

## Decision

We will use **Turso** (libSQL) as the primary database with a **database-per-Waddle** sharding strategy:

- One global database for authentication and cross-Waddle data
- One database per Waddle for community-specific data (messages, channels, memberships)

This approach leverages SQLite's strengths (simplicity, embeddability) while avoiding its single-writer limitation through natural sharding at the community boundary.

## Consequences

### Positive

- **SQLite Compatible**: Familiar SQL dialect, existing tooling works
- **Edge Replication**: Turso provides global edge replicas for read performance
- **Self-Hostable**: libSQL can be self-hosted without Turso cloud
- **Embedded Option**: Can embed libSQL directly in single-binary deployments
- **HTTP API**: Turso's HTTP interface simplifies serverless/edge deployments
- **Local-First Ready**: Future clients could use local libSQL with sync
- **Natural Sharding**: Database-per-Waddle distributes writes across databases, avoiding single-writer bottleneck
- **Data Isolation**: Each Waddle's data is physically separated, simplifying backups and deletion

### Negative

- **Younger Technology**: libSQL fork is newer than battle-tested alternatives
- **Ecosystem**: Fewer ORMs and tools compared to PostgreSQL
- **Turso Dependency**: Edge features require Turso cloud (or self-hosting infra)
- **Database Management**: Per-Waddle databases require tooling for provisioning and migrations

### Neutral

- **Migration Path**: Can migrate to PostgreSQL if needed; SQL is portable

## Implementation Notes

### Database Types

**Global Database** contains:
- User accounts and authentication
- Waddle registry (metadata about all Waddles)
- System configuration

**Per-Waddle Database** contains:
- Messages and threads
- Channels and channel memberships
- Waddle-specific member data and roles

### Technical Details

- Use `libsql` crate for Rust connectivity
- Schema migrations via `sqlx` or custom migration runner
- Waddle databases created on-demand when a Waddle is provisioned
- Connection pooling per database with lazy initialization
- Consider read replicas for heavy read workloads within individual Waddles

## Related

- [ADR-0007: CQRS Architecture](./0007-cqrs-architecture.md)
- [Spec: Message Schema](../specs/message-schema.md)
