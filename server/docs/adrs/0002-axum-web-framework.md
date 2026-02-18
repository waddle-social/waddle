# ADR-0002: Use Axum for HTTP/WebSocket

## Status

Accepted

## Context

With Rust selected as our backend language (see [ADR-0001](./0001-rust-backend.md)), we need to choose a web framework that supports:
- HTTP REST APIs
- WebSocket connections for real-time messaging
- Middleware composition (auth, logging, rate limiting)
- High performance under concurrent load

We evaluated:
- **Actix-web**: Mature, performant, but actor model adds complexity
- **Axum**: Tower-based, composable, native WebSocket support
- **Rocket**: Ergonomic, but slower adoption of async features
- **Warp**: Filter-based composition, less intuitive API

## Decision

We will use **Axum** as our HTTP and WebSocket framework.

## Consequences

### Positive

- **Tower Ecosystem**: Seamless integration with Tower middleware (timeouts, rate limiting, tracing)
- **WebSocket Support**: First-class `axum::extract::ws` for WebSocket upgrades
- **Extractors**: Type-safe request parsing with compile-time validation
- **Tokio Native**: Built on Tokio, ensuring compatibility with async Rust ecosystem
- **Active Development**: Maintained by Tokio team, frequent updates
- **Router Composition**: Nested routers enable clean API organization

### Negative

- **Younger Framework**: Less battle-tested than Actix-web in production
- **Documentation**: Some advanced patterns lack comprehensive guides
- **Breaking Changes**: Still evolving API (though stabilizing)

### Neutral

- **Performance**: Comparable to Actix-web in benchmarks; both are "fast enough"

## Related

- [ADR-0001: Rust Backend](./0001-rust-backend.md)
- [ADR-0006: XMPP Protocol](./0006-xmpp-protocol.md)
- [Spec: XMPP Integration](../specs/xmpp-integration.md)
- [Spec: API Contracts](../specs/api-contracts.md)
