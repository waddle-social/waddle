# Colony

Bun-based Astro 6 app targeting Cloudflare Workers with D1, Drizzle Kit, Better Auth GitHub OAuth, and a Better Auth OIDC provider used by `waddle-server`.

## Setup

1. Copy .dev.vars.example to .dev.vars and copy .auth.env.example to .auth.env.
2. Create a GitHub OAuth app with these callback URLs:
   - http://localhost:4321/api/auth/callback/github
   - https://colony.waddle.social/api/auth/callback/github
3. Local Astro development reads `BETTER_AUTH_URL` from `.dev.vars`. The Better Auth generator reads `BETTER_AUTH_URL`, `BETTER_AUTH_SECRET`, `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET`, and `WADDLE_SERVER_OIDC_CLIENT_SECRET` from `.auth.env`.
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
   - bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name WADDLE_SERVER_OIDC_CLIENT_SECRET --scopes workers
7. Create the production Secrets Store entries:
   - bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name BETTER_AUTH_SECRET --scopes workers --remote
   - bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name GITHUB_CLIENT_SECRET --scopes workers --remote
   - bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name WADDLE_SERVER_OIDC_CLIENT_SECRET --scopes workers --remote
8. Start development with bun run dev.

## Server OIDC

- `colony` is the remote OIDC issuer for `waddle-server`.
- The first-party trusted client ID is `waddle-server`.
- Runtime OIDC clients are code-whitelisted only. Database-backed client registration is disabled.
- Configure allowed server redirect URIs with `WADDLE_SERVER_OIDC_REDIRECT_URLS` as a comma-separated list of absolute callback URLs (for example `https://api.example.com/api/auth/callback`).
- If `WADDLE_SERVER_OIDC_REDIRECT_URLS` is unset, Colony defaults to `http://localhost:3000/api/auth/callback` for local development.
- The provider uses a first-party `web` client with a shared secret for `waddle-server`. Configure the shared secret as `WADDLE_SERVER_OIDC_CLIENT_SECRET` in local generator env and in the Cloudflare Secrets Store.
- The production value of `WADDLE_SERVER_OIDC_CLIENT_SECRET` is stored in 1Password at `op://waddle-production/waddle.chat/colonoy-oidc-client-secret`. It must match the `client_secret` field of the `colony` entry in `WADDLE_AUTH_PROVIDERS_JSON` on the `waddle-server-secrets` Kubernetes secret in the `waddle` namespace. When rotating, update both sides and redeploy colony + restart waddle-server.
