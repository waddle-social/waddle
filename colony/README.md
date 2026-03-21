# Colony

Bun-based Astro 6 app targeting Cloudflare Workers with D1, Drizzle Kit, Better Auth GitHub OAuth, and a Better Auth OIDC provider used by `chat`.

## Setup

1. Copy .dev.vars.example to .dev.vars and copy .auth.env.example to .auth.env.
2. Create a GitHub OAuth app with these callback URLs:
   - http://localhost:4321/api/auth/callback/github
   - https://colony.waddle.social/api/auth/callback/github
3. Local Astro development reads `BETTER_AUTH_URL` from `.dev.vars`. The Better Auth generator reads `BETTER_AUTH_URL`, `BETTER_AUTH_SECRET`, `GITHUB_CLIENT_ID`, and `GITHUB_CLIENT_SECRET` from `.auth.env`.
4. Generate types and auth schema:
   - bun run generate-types
   - bun run auth:generate
   - bun run db:generate
5. Apply migrations locally or remotely:
   - bun x wrangler d1 migrations apply colony --local
   - bun x wrangler d1 migrations apply colony --remote
6. Create local Secrets Store entries for runtime development:
   - bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name BETTER_AUTH_SECRET --scopes workers
   - bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name GITHUB_CLIENT_SECRET --scopes workers
7. Create the production Secrets Store entries:
   - bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name BETTER_AUTH_SECRET --scopes workers --remote
   - bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name GITHUB_CLIENT_SECRET --scopes workers --remote
8. Start development with bun run dev.

## Chat OIDC

- `colony` is the remote OIDC issuer for `chat`.
- The first-party trusted client ID is `waddle-chat`.
- Runtime OIDC clients are code-whitelisted only. Database-backed client registration is disabled.
- Allowed Chat redirect URIs are:
  - `http://localhost:4321/api/auth/oauth2/callback/colony`
  - `https://chat.waddle.social/api/auth/oauth2/callback/colony`
- The provider uses public-client PKCE for Chat, so Chat does not need an OIDC client secret.
