# S2S Federation Specification

**Status:** Draft
**Priority:** P0 (Critical)
**Last Updated:** 2025-01-21

## Overview

This specification defines server-to-server (S2S) federation for Waddle, enabling XMPP communication between independent waddle instances.

## Protocol

XMPP Server-to-Server connections follow RFC 6120 and RFC 6121:

- **Port:** 5269 (standard XMPP S2S)
- **Security:** TLS 1.3 required
- **Verification:** Server Dialback (XEP-0220)

## Connection Flow

```
alice.dev                                    waddle.social
    │                                              │
    │  1. TCP connect to :5269                     │
    │─────────────────────────────────────────────>│
    │                                              │
    │  2. TLS handshake                            │
    │<─────────────────────────────────────────────│
    │                                              │
    │  3. Stream open                              │
    │  <stream:stream to='waddle.social'          │
    │   from='alice.dev'>                          │
    │─────────────────────────────────────────────>│
    │                                              │
    │  4. Stream features (dialback)               │
    │  <stream:features>                           │
    │    <dialback xmlns='urn:xmpp:features:      │
    │              dialback'/>                     │
    │  </stream:features>                          │
    │<─────────────────────────────────────────────│
    │                                              │
    │  5. Dialback key                             │
    │  <db:result to='waddle.social'              │
    │   from='alice.dev'>key123</db:result>        │
    │─────────────────────────────────────────────>│
    │                                              │
    │  6. Verify (reverse connection)              │
    │  <db:verify to='alice.dev'                  │
    │   from='waddle.social'>key123</db:verify>    │
    │<─────────────────────────────────────────────│
    │                                              │
    │  7. Verification result                      │
    │  <db:verify type='valid'.../>                │
    │─────────────────────────────────────────────>│
    │                                              │
    │  8. Connection authorized                    │
    │  <db:result type='valid'.../>                │
    │<─────────────────────────────────────────────│
    │                                              │
    │  9. Stanzas can now be routed               │
    │─────────────────────────────────────────────>│
```

## DNS Discovery

Remote servers are discovered via DNS SRV records:

```
_xmpp-server._tcp.waddle.social. 86400 IN SRV 5 0 5269 xmpp.waddle.social.
```

Fallback to A/AAAA record on port 5269 if no SRV record exists.

## Stanza Routing

### Outbound Routing

When a local user sends a stanza to a remote JID:

```rust
async fn route_stanza(&self, stanza: Stanza) -> Result<()> {
    let to = stanza.to().ok_or(Error::MissingTo)?;

    if self.is_local_domain(&to.domain()) {
        // Local delivery
        self.local_router.route(stanza).await
    } else {
        // Remote delivery via S2S
        let connection = self.s2s_pool.get_or_connect(&to.domain()).await?;
        connection.send(stanza).await
    }
}
```

### Inbound Processing

When receiving a stanza from a remote server:

```rust
async fn process_inbound(&self, stanza: Stanza, from_domain: &str) -> Result<()> {
    // Verify stanza originated from authenticated domain
    if let Some(from) = stanza.from() {
        if from.domain() != from_domain {
            return Err(Error::SpoofedFrom);
        }
    }

    // Route to local recipient
    self.local_router.route(stanza).await
}
```

## Federated MUC

Remote users join local MUC rooms via S2S:

```
dave@alice.dev joins #general@waddle.social

1. alice.dev sends presence to waddle.social:
   <presence from='dave@alice.dev/resource'
             to='general@waddle.social/dave'>
     <x xmlns='http://jabber.org/protocol/muc'/>
   </presence>

2. waddle.social adds dave as occupant
3. waddle.social routes room messages to dave via S2S
```

### Occupant Tracking

```rust
struct MucOccupant {
    real_jid: Jid,           // dave@alice.dev
    room_nick: String,        // dave
    affiliation: Affiliation, // member
    role: Role,              // participant
    is_remote: bool,         // true
    home_server: String,      // alice.dev
}
```

## Identity Verification (ATProto)

When an ATProto user from waddle.social joins a remote waddle:

```
abc123@waddle.social joins #dev@alice.dev

1. abc123 presents federated credential to alice.dev
2. alice.dev verifies credential signature
3. alice.dev optionally verifies with waddle.social via S2S:
   <iq type='get' to='waddle.social' from='alice.dev'>
     <verify xmlns='urn:waddle:identity'>
       <jid>abc123@waddle.social</jid>
       <credential>...</credential>
     </verify>
   </iq>
4. waddle.social confirms identity
5. alice.dev grants access
```

## Connection Pool

Maintain persistent S2S connections:

```rust
struct S2SConnectionPool {
    connections: HashMap<String, S2SConnection>,
    max_connections_per_domain: usize,
    idle_timeout: Duration,
    connect_timeout: Duration,
}

impl S2SConnectionPool {
    async fn get_or_connect(&self, domain: &str) -> Result<S2SConnection> {
        if let Some(conn) = self.connections.get(domain) {
            if conn.is_healthy() {
                return Ok(conn.clone());
            }
        }

        self.establish_connection(domain).await
    }
}
```

## Configuration

```toml
[s2s]
# Enable S2S federation
enabled = true

# S2S listener port
port = 5269

# TLS certificate for S2S
cert_path = "/etc/waddle/certs/server.crt"
key_path = "/etc/waddle/certs/server.key"

# Connection settings
connect_timeout = "10s"
idle_timeout = "300s"
max_connections_per_domain = 5

# Federation policy
policy = "open"  # "open", "allowlist", "blocklist"

# Allowlist (when policy = "allowlist")
allowed_domains = ["waddle.social", "trusted.example"]

# Blocklist (when policy = "blocklist")
blocked_domains = ["spam.example"]
```

## Security

### TLS Requirements

- TLS 1.3 minimum
- Valid certificates required (no self-signed in production)
- SNI must match target domain

### Rate Limiting

```rust
struct S2SRateLimiter {
    /// Max new connections per domain per minute
    connection_rate: u32,
    /// Max stanzas per domain per second
    stanza_rate: u32,
    /// Max total S2S bandwidth
    bandwidth_limit: ByteSize,
}
```

### Abuse Prevention

- Connection rate limiting per remote domain
- Stanza rate limiting per remote domain
- Blocklist for known bad actors
- Optional allowlist mode for private deployments

## Error Handling

### Connection Failures

```rust
enum S2SError {
    DnsResolutionFailed(String),
    ConnectionRefused(String),
    TlsHandshakeFailed(String),
    DialbackFailed(String),
    StreamError(StreamError),
    StanzaRoutingFailed(String),
}
```

### Retry Policy

```rust
struct RetryPolicy {
    max_retries: u32,
    initial_delay: Duration,
    max_delay: Duration,
    backoff_multiplier: f64,
}

// Default: 3 retries, 1s initial, 30s max, 2x backoff
```

## Metrics

Track federation health:

- `s2s_connections_active` - Current active connections by domain
- `s2s_connections_total` - Total connections established
- `s2s_stanzas_sent` - Stanzas sent by domain
- `s2s_stanzas_received` - Stanzas received by domain
- `s2s_errors` - Errors by type and domain
- `s2s_latency` - Round-trip latency by domain

## Implementation Files

| File | Purpose |
|------|---------|
| `crates/waddle-xmpp/src/s2s/mod.rs` | S2S module entry point |
| `crates/waddle-xmpp/src/s2s/connection.rs` | S2S connection handling |
| `crates/waddle-xmpp/src/s2s/dialback.rs` | XEP-0220 dialback |
| `crates/waddle-xmpp/src/s2s/pool.rs` | Connection pool management |
| `crates/waddle-xmpp/src/s2s/dns.rs` | SRV record resolution |
| `crates/waddle-xmpp/src/routing.rs` | Local vs remote routing |

## Related Documents

- [RFC-0015: Federation Architecture](../rfcs/0015-federation-architecture.md)
- [ADR-0015: Dual Authentication Modes](../adrs/0015-dual-authentication.md)
- [ADR-0006: XMPP Protocol](../adrs/0006-xmpp-protocol.md)
- [XEP-0220: Server Dialback](https://xmpp.org/extensions/xep-0220.html)
- [RFC 6120: XMPP Core](https://tools.ietf.org/html/rfc6120)
