# ADR-0005: ATProto OAuth for Identity

## Status

Accepted

## Context

Waddle Social needs an identity system that:
- Avoids building custom authentication from scratch
- Provides portable, user-owned identities
- Enables integration with the broader ATProto ecosystem (Bluesky)
- Supports decentralized identity (DIDs)

We evaluated:
- **Custom Auth**: Full control, but security burden and no portability
- **OAuth2 Providers**: Google/GitHub auth, but creates vendor dependency
- **ATProto OAuth**: Bluesky's identity system with DID-based handles
- **Passkeys/WebAuthn**: Modern, but requires account management infrastructure
- **Matrix Identity**: Federated, but different ecosystem

## Decision

We will use **ATProto OAuth** for identity, leveraging Bluesky DIDs for user identification.

## Consequences

### Positive

- **Portable Identity**: Users own their DID; can use same identity across ATProto apps
- **No Password Storage**: OAuth flow means we never handle passwords
- **Handle System**: Users identified by `@handle.bsky.social` or custom domains
- **Ecosystem Integration**: Natural path to Bluesky announcement features
- **DID Resolution**: Cryptographic identity verification via DID documents
- **PDS Integration**: Can post announcements to user's Personal Data Server

### Negative

- **ATProto Dependency**: Tied to ATProto ecosystem health
- **Complexity**: DID resolution and OAuth flows add implementation complexity
- **Limited Adoption**: Smaller user base than mainstream OAuth providers
- **Data Separation**: We use ATProto for identity only; messages stored separately

### Neutral

- **Custom Data Layer**: Intentional choice to not store messages on ATProto (privacy, performance)

## Implementation Notes

- Implement ATProto OAuth flow per specification
- Store DID as primary user identifier in local database
- Resolve handles via ATProto handle resolution
- Token refresh handling for long-lived sessions

## Related

- [RFC-0011: Bluesky Announcements](../rfcs/0011-bluesky-broadcast.md)
- [Spec: ATProto Integration](../specs/atproto-integration.md)
