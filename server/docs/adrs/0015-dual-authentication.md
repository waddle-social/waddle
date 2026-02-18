# ADR-0015: Dual Authentication Modes

**Status:** Accepted
**Date:** 2025-01-21
**Deciders:** Core Team
**Priority:** P0 (Critical)

## Context

The current Waddle implementation requires ATProto OAuth for all users. This creates barriers for:

1. **Self-hosted deployments** - Operators may want native XMPP auth without ATProto
2. **XMPP interoperability** - Standard XMPP clients expect SCRAM-SHA-256
3. **Federation** - Remote servers need to verify identities via standard mechanisms

## Decision

Support multiple authentication paths:

### Path A: Session Token (ATProto users on home server)

```
Client → Home Server
1. ATProto OAuth flow completes
2. Server issues session_id
3. XMPP SASL PLAIN with session_id as password
4. Server validates session against session store
```

**JID format:** `{did_hash}@waddle.social`

### Path B: Federated Credential (ATProto users on remote servers)

```
Client → Remote Waddle
1. Client presents SignedCredential from home server
2. Remote server verifies signature via S2S
3. Access granted with federated identity
```

**JID format:** `{did_hash}@waddle.social` (verified remotely)

### Path C: SCRAM-SHA-256 (Native JID users)

```
Client → Any Waddle
1. Standard XMPP SASL SCRAM-SHA-256 exchange
2. Server verifies against local credential store
3. Access granted with local identity
```

**JID format:** `{username}@{waddle_domain}`

### Path D: In-Band Registration (XEP-0077)

```
Client → Waddle with registration enabled
1. Client sends registration request
2. Server creates local account
3. Subsequent auth via SCRAM-SHA-256
```

## Authentication Mechanism Selection

```rust
pub trait XmppAuth {
    async fn authenticate(&self, mechanism: AuthMechanism) -> Result<AuthResult>;
}

pub enum AuthMechanism {
    /// Current: Session-based for ATProto users on home server
    SessionToken { jid: Jid, session_id: String },

    /// New: For ATProto users connecting to remote servers
    FederatedCredential { jid: Jid, credential: SignedCredential },

    /// New: Standard XMPP auth for native JID users
    ScramSha256 { jid: Jid, scram_state: ScramState },

    /// New: Account registration (XEP-0077)
    Register { username: String, password: String },
}

pub enum AuthResult {
    Success { session: Session, identity_type: IdentityType },
    NeedMoreSteps { continuation: AuthContinuation },
    Failure { reason: AuthError },
}

pub enum IdentityType {
    /// ATProto-authenticated user
    Federated { did: String, home_server: String },
    /// Native JID user local to this server
    Local { domain: String },
}
```

## Credential Storage

### ATProto Sessions (existing)

```sql
-- Existing session table for ATProto users
CREATE TABLE sessions (
    id TEXT PRIMARY KEY,
    did TEXT NOT NULL,
    jid TEXT NOT NULL,
    created_at DATETIME NOT NULL,
    expires_at DATETIME NOT NULL
);
```

### Native JID Credentials (new)

```sql
-- New table for native JID users
CREATE TABLE native_users (
    id TEXT PRIMARY KEY,
    username TEXT NOT NULL,
    domain TEXT NOT NULL,
    password_hash TEXT NOT NULL,  -- argon2id
    created_at DATETIME NOT NULL,
    updated_at DATETIME NOT NULL,
    UNIQUE(username, domain)
);
```

## Configuration

```rust
pub struct AuthConfig {
    /// Enable ATProto OAuth (required for home server mode)
    pub atproto_enabled: bool,

    /// Enable native JID registration (XEP-0077)
    pub registration_enabled: bool,

    /// Enable SCRAM-SHA-256 for native users
    pub scram_enabled: bool,

    /// Enable federated credential verification
    pub federation_enabled: bool,
}
```

### Home Server Configuration

```toml
[auth]
atproto_enabled = true
registration_enabled = false  # ATProto is the identity source
scram_enabled = true          # For local verification
federation_enabled = true
```

### Standalone Waddle Configuration

```toml
[auth]
atproto_enabled = false       # Optional
registration_enabled = true   # Native JID registration
scram_enabled = true
federation_enabled = true     # Accept federated users
```

## SASL Mechanism Advertisement

Server advertises available mechanisms based on configuration:

```xml
<mechanisms xmlns='urn:ietf:params:xml:ns:xmpp-sasl'>
  <!-- Always for session-based auth -->
  <mechanism>PLAIN</mechanism>
  <!-- When scram_enabled = true -->
  <mechanism>SCRAM-SHA-256</mechanism>
</mechanisms>
```

## Consequences

### Positive

- Self-hosted waddles work without ATProto dependency
- Standard XMPP clients can connect natively
- Full XMPP federation support
- Gradual adoption path for ATProto

### Negative

- Increased authentication complexity
- Two credential stores to maintain
- Different identity semantics (DID vs JID)

### Neutral

- Native users cannot use ATProto features (Bluesky posting, etc.)
- Federation requires trust establishment

## Related

- [RFC-0015: Federation Architecture](../rfcs/0015-federation-architecture.md)
- [ADR-0005: ATProto Identity](0005-atproto-identity.md)
- [Spec: S2S Federation](../specs/s2s-federation.md)
