# ADR-006: AT Protocol Identity Layer

**Status:** Accepted

**Date:** 2025-09-30

## Context

Identity and authentication are foundational to Waddle. We need a system that:

- Provides decentralized identity (users own their identity)
- Works across multiple Waddles (one identity, many communities)
- Supports portable social graphs
- Enables future federation and data portability
- Integrates with existing atproto ecosystem (Bluesky, etc.)

Traditional email/password auth or OAuth with centralized providers (Google, GitHub) creates vendor lock-in and doesn't support our vision of user-owned, portable identity across communities.

## Decision

We will use **AT Protocol (atproto)** as our identity layer, leveraging:

1. **DIDs (Decentralized Identifiers)** for user identity
2. **DPoP OAuth** for authentication
3. **Better-auth** with atproto plugin for session management
4. **Colony service** as our atproto OAuth provider

### Identity Model

```typescript
interface WaddleUser {
  id: string;                    // Internal user ID
  did: string;                   // did:plc:abc123 or did:web:user.example.com
  handle: string;                // user.bsky.social or user@domain.com
  displayName?: string;
  avatar?: string;
  profile?: AtProtoProfile;      // Full atproto profile
  createdAt: Date;
}

interface AtProtoProfile {
  displayName?: string;
  description?: string;
  avatar?: string;
  banner?: string;
  // Additional atproto profile fields
}
```

### Authentication Flow

```
┌─────────────┐
│   Client    │
└──────┬──────┘
       │ 1. Sign in with handle
       ▼
┌─────────────────────────────────┐
│  Colony (Auth Service)          │
│  - Resolve handle to DID        │
│  - Discover auth server         │
│  - Generate PKCE + DPoP         │
└──────┬──────────────────────────┘
       │ 2. Redirect to PDS
       ▼
┌─────────────────────────────────┐
│  User's PDS (bsky.social, etc.) │
│  - User authenticates           │
│  - Consent screen               │
└──────┬──────────────────────────┘
       │ 3. Callback with code
       ▼
┌─────────────────────────────────┐
│  Colony (Auth Service)          │
│  - Exchange code for tokens     │
│  - Validate DPoP proof          │
│  - Create session               │
└──────┬──────────────────────────┘
       │ 4. Redirect to Waddle
       ▼
┌─────────────┐
│   Waddle    │
│  (Chat App) │
└─────────────┘
```

## Consequences

### Positive

- **User sovereignty**: Users own their identity via DIDs
- **Portability**: Identity works across any atproto-compatible service
- **Social graph**: Users can bring followers/following from Bluesky
- **Ecosystem integration**: Waddle joins the atproto ecosystem
- **Future-proof**: Supports federation and decentralization
- **Privacy**: Users control what data Waddle can access
- **No password management**: Delegated to user's PDS
- **Brand alignment**: Aligns with web3/decentralization values

### Negative

- **Complexity**: OAuth + DPoP + DID resolution is complex
- **User confusion**: Most users unfamiliar with DIDs/handles
- **Onboarding friction**: Requires Bluesky account or PDS setup
- **Dependency**: Relies on user's PDS being available
- **Limited adoption**: atproto ecosystem is still maturing
- **Key management**: Private key rotation and security concerns
- **Performance**: DID resolution adds latency to auth flow

### Mitigation Strategies

- **Simplified onboarding**: "Sign in with Bluesky" flow
- **Fallback auth**: Optional email/password for non-atproto users (future)
- **Clear messaging**: Explain benefits of decentralized identity
- **PDS hosting**: Offer managed PDS for users without one
- **Caching**: Cache DID documents to reduce resolution latency
- **Graceful degradation**: Handle PDS downtime gracefully

## Alternatives Considered

### Traditional Email/Password

**Pros:** Simple, familiar, no dependencies
**Cons:** Centralized, not portable, password management burden

**Rejected because:** Conflicts with decentralization and portability goals.

### OAuth with Social Providers (Google, GitHub)

**Pros:** Easy onboarding, familiar to users
**Cons:** Vendor lock-in, not portable, centralized control

**Rejected because:** User doesn't own identity, can't move between services.

### Self-Issued OpenID Provider (SIOP)

**Pros:** Decentralized, user-controlled
**Cons:** Immature ecosystem, limited tooling, complex implementation

**Rejected because:** atproto provides more complete solution.

### Web3 Wallets (MetaMask, WalletConnect)

**Pros:** Decentralized, crypto-native
**Cons:** Poor UX, crypto association, limited to Web3 users

**Rejected because:** Too niche, alienates non-crypto users.

## Implementation Details

### Colony Service Architecture

Colony is a dedicated Cloudflare Workers service that handles:

1. **OAuth flow management**: PKCE, DPoP, state management
2. **DID resolution**: Convert handles to DIDs, fetch DID documents
3. **Session management**: Issue JWT tokens, store sessions in D1
4. **Profile sync**: Fetch and cache atproto profiles
5. **Client metadata**: Serve OAuth client metadata JSON

### DID Resolution

```typescript
// colony/lib/auth/handle-resolver.ts
export async function resolveHandle(handle: string): Promise<string> {
  // Remove @ prefix if present
  const normalizedHandle = handle.startsWith('@') ? handle.slice(1) : handle;

  // Query DNS TXT record for _atproto subdomain
  const dnsResult = await fetch(
    `https://cloudflare-dns.com/dns-query?name=_atproto.${normalizedHandle}&type=TXT`,
    {
      headers: { 'Accept': 'application/dns-json' },
    }
  );

  const dnsData = await dnsResult.json();
  const didRecord = dnsData.Answer?.find((a: any) =>
    a.data.startsWith('did=')
  );

  if (didRecord) {
    return didRecord.data.replace('did=', '');
  }

  // Fallback: Try well-known endpoint
  const wellKnownResult = await fetch(
    `https://${normalizedHandle}/.well-known/atproto-did`
  );

  if (wellKnownResult.ok) {
    return await wellKnownResult.text();
  }

  throw new Error(`Could not resolve handle: ${handle}`);
}
```

### DPoP OAuth Implementation

```typescript
// colony/lib/auth/atproto-oauth-worker.ts
export class WorkerOAuthClient {
  private clientId: string;
  private redirectUri: string;
  private privateKey: BlueskyJWK;

  async createClientAssertion(): Promise<string> {
    const payload = {
      iss: this.clientId,
      sub: this.clientId,
      aud: 'https://bsky.social',
      jti: crypto.randomUUID(),
      iat: Math.floor(Date.now() / 1000),
      exp: Math.floor(Date.now() / 1000) + 300, // 5 minutes
    };

    return await this.signJWT(payload, this.privateKey);
  }

  async createDPoPProof(
    method: string,
    url: string,
    nonce?: string,
    existingKeyPair?: CryptoKeyPair
  ): Promise<{ proof: string; publicKey: JsonWebKey; keyPair: CryptoKeyPair }> {
    // Generate or reuse DPoP key pair
    const keyPair = existingKeyPair || await crypto.subtle.generateKey(
      { name: 'ECDSA', namedCurve: 'P-256' },
      true,
      ['sign', 'verify']
    );

    const publicKey = await crypto.subtle.exportKey('jwk', keyPair.publicKey);

    const header = {
      typ: 'dpop+jwt',
      alg: 'ES256',
      jwk: publicKey,
    };

    const payload = {
      htm: method,
      htu: url,
      jti: crypto.randomUUID(),
      iat: Math.floor(Date.now() / 1000),
      ...(nonce && { nonce }),
    };

    const proof = await this.signJWT(payload, keyPair.privateKey, header);

    return { proof, publicKey, keyPair };
  }
}
```

### Session Management with Better-auth

```typescript
// colony/lib/auth/better-auth.ts
export async function createAuth(db: D1Database, env: any) {
  return betterAuth({
    database: drizzleAdapter(drizzle(db), {
      provider: 'sqlite',
      schema: { user, session, account, verification },
    }),
    plugins: [
      genericOAuth({
        config: [{
          providerId: 'atproto',
          clientId: `${env.SITE_URL}/client-metadata.json`,
          clientSecret: 'not-used', // Uses private_key_jwt
          authorizationUrl: 'https://bsky.social/oauth/authorize',
          tokenUrl: 'https://bsky.social/oauth/token',
          scopes: ['atproto', 'transition:email'],
          pkce: true,
          getAccessToken: async ({ code, codeVerifier, redirectUri }) => {
            // Custom token exchange with DPoP
            const oauthClient = new WorkerOAuthClient({ ... });
            const tokens = await oauthClient.exchangeCode(code, codeVerifier);
            return tokens;
          },
          getUserInfo: async ({ accessToken }) => {
            // Fetch profile from atproto
            const profile = await fetchAtProtoProfile(accessToken);
            return profile;
          },
        }],
      }),
    ],
    session: {
      expiresIn: 60 * 60 * 24 * 7, // 7 days
    },
  });
}
```

### Client Metadata Endpoint

```typescript
// colony/website/public/client-metadata.json
{
  "client_id": "https://colony.waddle.social/client-metadata.json",
  "client_name": "Waddle",
  "client_uri": "https://waddle.social",
  "logo_uri": "https://waddle.social/logo.png",
  "redirect_uris": [
    "https://colony.waddle.social/api/auth/oauth2/callback/atproto"
  ],
  "scope": "atproto transition:email",
  "grant_types": ["authorization_code", "refresh_token"],
  "response_types": ["code"],
  "token_endpoint_auth_method": "private_key_jwt",
  "token_endpoint_auth_signing_alg": "ES256",
  "dpop_bound_access_tokens": true,
  "jwks_uri": "https://colony.waddle.social/.well-known/jwks.json"
}
```

## Identity Usage in Waddle

### Cross-Waddle Identity

Users maintain one identity across all Waddles:

```typescript
// When user joins a Waddle
async function joinWaddle(userId: string, waddleId: string, env: Env) {
  // User already exists in central DB with DID
  await env.CENTRAL_DB.prepare(`
    INSERT INTO waddle_members (waddle_id, user_id, joined_at)
    VALUES (?, ?, ?)
  `).bind(waddleId, userId, new Date().toISOString()).run();

  // User's DID is portable across all Waddles
  // No need to re-authenticate
}
```

### Profile Sync

Periodically sync profiles from atproto:

```typescript
async function syncUserProfile(did: string, env: Env) {
  const profile = await fetchAtProtoProfile(did);

  await env.CENTRAL_DB.prepare(`
    UPDATE users
    SET display_name = ?, avatar = ?, profile_synced_at = ?
    WHERE did = ?
  `).bind(
    profile.displayName,
    profile.avatar,
    new Date().toISOString(),
    did
  ).run();
}
```

## Migration Strategy

### Phase 1: Colony MVP (Complete)
- ✅ Better-auth integration
- ✅ atproto OAuth flow
- ✅ DPoP token handling
- ✅ Session management

### Phase 2: Waddle Integration
- Validate Colony sessions in Waddle workers
- Service binding from Waddle to Colony
- User profile display from atproto

### Phase 3: Profile Features
- Edit display name (local override)
- Profile sync from Bluesky
- Social graph import (future)

### Phase 4: Advanced Identity
- Multi-PDS support
- Portable data via atproto
- Federation with other atproto services

## Security Considerations

- **Private key security**: Store OAuth signing keys in Cloudflare Secrets
- **DPoP validation**: Always validate DPoP proofs on token refresh
- **Session rotation**: Rotate session tokens regularly
- **PDS verification**: Verify PDS identity before trusting
- **Scope limiting**: Request minimal OAuth scopes needed

## References

- [AT Protocol Specification](https://atproto.com/specs/atp)
- [AT Protocol OAuth](https://atproto.com/specs/oauth)
- [DPoP RFC 9449](https://datatracker.ietf.org/doc/html/rfc9449)
- [Better-auth Documentation](https://better-auth.com/)
- [Colony Implementation](@colony/website/src/lib/auth/)