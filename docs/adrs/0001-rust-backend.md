# ADR-0001: Use Rust for Backend

## Status

Accepted

## Context

Waddle Social requires a backend that can handle:
- High-concurrency real-time messaging (WebSockets)
- Low-latency message delivery
- Memory-safe concurrent code
- Potential code sharing with future native clients (via Dioxus or similar)

We evaluated several languages and runtimes:
- **Go**: Excellent concurrency, but garbage collection pauses could affect real-time performance
- **Node.js/TypeScript**: Fast development, but single-threaded model limits scaling
- **Rust**: Memory safety without GC, excellent async ecosystem, growing web framework maturity
- **Elixir/Erlang**: Outstanding for real-time systems, but smaller talent pool

## Decision

We will use **Rust** as the primary backend language.

## Consequences

### Positive

- **Memory Safety**: Compile-time guarantees prevent common security vulnerabilities (buffer overflows, use-after-free)
- **Performance**: No garbage collector means predictable latency for real-time messaging
- **Async Ecosystem**: Tokio provides a mature async runtime; Tower ecosystem enables middleware composition
- **Type Safety**: Strong type system catches errors at compile time
- **Code Sharing**: Potential to share core logic with Dioxus-based desktop/mobile clients
- **WASM Compilation**: Can compile performance-critical code to WebAssembly for web clients

### Negative

- **Learning Curve**: Rust's ownership model requires investment to learn
- **Compile Times**: Longer iteration cycles compared to interpreted languages
- **Ecosystem Maturity**: Some libraries are less mature than Node.js/Go equivalents
- **Hiring**: Smaller talent pool compared to mainstream languages

### Neutral

- **Community**: Growing rapidly, strong OSS ethos aligns with Waddle's AGPL license

## Related

- [ADR-0002: Axum Web Framework](./0002-axum-web-framework.md)
- [ADR-0008: Kameo Actor Framework](./0008-kameo-actors.md)
