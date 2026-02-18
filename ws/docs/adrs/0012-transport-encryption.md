# ADR-0012: Transport-Only Encryption

## Status

Accepted

## Context

Encryption choices for a messaging platform:
- **Transport Encryption (TLS)**: Encrypts data in transit; server can read messages
- **End-to-End Encryption (E2EE)**: Only sender/recipient can read; server is untrusted
- **At-Rest Encryption**: Database encryption; protects against storage breaches

Trade-offs:
- E2EE prevents server-side features (search, AI summaries, moderation)
- E2EE adds key management complexity (device sync, key rotation, recovery)
- Transport encryption is simpler and enables server-side features

## Decision

We will implement **transport-only encryption (TLS)** for the MVP. Messages are readable by the server.

## Consequences

### Positive

- **Full-Text Search**: Server can index and search message content
- **AI Features**: Summaries, translation, moderation can process messages
- **Simpler Implementation**: No key exchange protocols or device sync
- **Message Recovery**: Users don't lose messages if they lose devices
- **Moderation**: Trust & safety team can review reported content

### Negative

- **Server Trust Required**: Users must trust the server operator
- **Breach Risk**: Database breach exposes message content
- **Regulatory Concerns**: Some jurisdictions prefer E2EE for privacy
- **Competitive Disadvantage**: Some users specifically want E2EE

### Neutral

- **Future E2EE Option**: Could add opt-in E2EE channels later (with feature trade-offs)

## Implementation Notes

- Enforce TLS 1.3 for all connections
- HSTS headers for web clients
- Certificate pinning for mobile clients (future)
- At-rest encryption via database/storage provider features

## Security Considerations

While not E2EE, we will implement:
- TLS 1.3 minimum
- Perfect forward secrecy
- Regular security audits
- Minimal data retention where possible
- Ephemeral message TTL for sensitive conversations

## Related

- [RFC-0005: Ephemeral Content](../rfcs/0005-ephemeral-content.md)
- [RFC-0013: Moderation](../rfcs/0013-moderation.md)
- [RFC-0007: AI Integrations](../rfcs/0007-ai-integrations.md)
