# Colony

Bun-based Astro 6 app targeting Cloudflare Workers with D1, Drizzle Kit, and Better Auth GitHub OAuth.

## Setup

1. Copy .dev.vars.example to .dev.vars and fill in the placeholders.
2. Create a GitHub OAuth app with these callback URLs:
   - http://localhost:4321/api/auth/callback/github
   - https://colony.waddle.social/api/auth/callback/github
3. Local development reads secrets from Cloudflare-compatible bindings in `.dev.vars`, while production reads them from Worker bindings and secrets.
4. Generate types and auth schema:
   - bun run generate-types
   - bun run auth:generate
   - bun run db:generate
5. Apply migrations locally or remotely:
   - bun x wrangler d1 migrations apply colony --local
   - bun x wrangler d1 migrations apply colony --remote
6. Start development with bun run dev.
