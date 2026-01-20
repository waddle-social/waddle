# Waddle Social - Recommended Rust Crates

This document lists the recommended Rust crates for Waddle Social, organized by functional area and linked to relevant Architecture Decision Records (ADRs).

---

## Core Runtime & Async

| Crate | Purpose | ADR Reference |
|-------|---------|---------------|
| `tokio` | Async runtime | [ADR-0001](adrs/0001-rust-backend.md) |
| `tower` | Middleware ecosystem | [ADR-0002](adrs/0002-axum-web-framework.md) |
| `futures` | Async utilities | [ADR-0001](adrs/0001-rust-backend.md) |

---

## Web Framework

| Crate | Purpose | ADR Reference |
|-------|---------|---------------|
| `axum` | HTTP/WebSocket framework | [ADR-0002](adrs/0002-axum-web-framework.md) |
| `axum-extra` | Additional extractors | [ADR-0002](adrs/0002-axum-web-framework.md) |
| `tower-http` | HTTP middleware (CORS, compression, tracing) | [ADR-0002](adrs/0002-axum-web-framework.md) |

---

## CLI TUI

| Crate | Purpose | ADR Reference |
|-------|---------|---------------|
| `ratatui` | Terminal UI framework | [ADR-0003](adrs/0003-ratatui-cli.md) |
| `crossterm` | Cross-platform terminal backend | [ADR-0003](adrs/0003-ratatui-cli.md) |
| `tui-textarea` | Multi-line input widget | [ADR-0003](adrs/0003-ratatui-cli.md) |

---

## Database

| Crate | Purpose | ADR Reference |
|-------|---------|---------------|
| `libsql` | Turso/libSQL client | [ADR-0004](adrs/0004-turso-libsql-database.md) |
| `sqlx` | SQL toolkit & migrations | [ADR-0004](adrs/0004-turso-libsql-database.md) |

---

## Actor Framework

| Crate | Purpose | ADR Reference |
|-------|---------|---------------|
| `kameo` | Actor framework | [ADR-0008](adrs/0008-kameo-actors.md) |

---

## XMPP Client

| Crate | Purpose | ADR Reference |
|-------|---------|---------------|
| `xmpp` | XMPP protocol client | [ADR-0006](adrs/0006-xmpp-protocol.md) |
| `xmpp-parsers` | XMPP stanza parsing | [ADR-0006](adrs/0006-xmpp-protocol.md) |

---

## Object Storage

| Crate | Purpose | ADR Reference |
|-------|---------|---------------|
| `object_store` | S3-compatible storage abstraction | [ADR-0011](adrs/0011-self-hosted-storage.md) |

---

## Serialization

| Crate | Purpose |
|-------|---------|
| `serde` | Serialization framework |
| `serde_json` | JSON support |
| `toml` | TOML config files |

---

## Error Handling

| Crate | Purpose |
|-------|---------|
| `thiserror` | Custom error types |
| `anyhow` | Application error handling |

---

## HTTP Client

| Crate | Purpose |
|-------|---------|
| `reqwest` | HTTP client (ATProto, webhooks) |

---

## CLI Arguments & Config

| Crate | Purpose |
|-------|---------|
| `clap` | Command-line argument parsing |
| `config` | Configuration management |
| `directories` | XDG paths (~/.config, ~/.cache) |

---

## Observability

| Crate | Purpose |
|-------|---------|
| `tracing` | Structured logging |
| `tracing-subscriber` | Log formatting/output |
| `metrics` | Metrics collection (optional) |

---

## Security/Crypto

| Crate | Purpose | ADR Reference |
|-------|---------|---------------|
| `rustls` | TLS implementation | [ADR-0012](adrs/0012-transport-encryption.md) |
| `hmac` | HMAC signatures | - |
| `sha2` | SHA-2 hashing | - |

---

## Utilities

| Crate | Purpose |
|-------|---------|
| `uuid` | UUID generation |
| `chrono` | Date/time handling |
| `url` | URL parsing |
| `base64` | Base64 encoding |

---

## Markdown/Syntax (CLI)

| Crate | Purpose |
|-------|---------|
| `pulldown-cmark` | Markdown parsing |
| `syntect` | Syntax highlighting |

---

## Testing

| Crate | Purpose |
|-------|---------|
| `tokio-test` | Async test utilities |
| `wiremock` | HTTP mocking |
| `proptest` | Property-based testing |

---

## Usage Guidelines

1. **Check ADR compatibility**: Before adding a new crate, verify it aligns with the relevant ADR decisions.
2. **Prefer listed crates**: Use the crates listed here over alternatives unless there's a compelling reason.
3. **Feature flags**: Enable only necessary feature flags to minimize compile times and binary size.
4. **Version pinning**: Use exact versions in `Cargo.toml` for reproducible builds.

## Adding New Crates

When proposing a new crate dependency:

1. Check if an existing crate already covers the use case
2. Verify the crate is actively maintained
3. Review the crate's license compatibility with AGPL-3.0
4. Consider the crate's compile-time impact
5. Update this document when adding new standard crates
