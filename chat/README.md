# Chat

Astro + TypeScript app for `chat/`, wired for Cloudflare Workers, D1, Better Auth, Vue, Tailwind 4, Bun, and `cuenv`.

## Commands

Run these from [`chat/`](/Users/icepuma/development/waddle/chat):

```sh
cuenv task install
cuenv task dev
cuenv task build
cuenv task deployPreview
cuenv task deployProduction
```

If you need Cloudflare runtime types without a full build:

```sh
cuenv task generateTypes
```

## Local Auth Setup

1. Copy `.dev.vars.example` to `.dev.vars` and `.auth.env.example` to `.auth.env`.
2. Create local and production `BETTER_AUTH_SECRET` entries in the shared Cloudflare Secrets Store:
   - `bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name BETTER_AUTH_SECRET --scopes workers`
   - `bun x wrangler secrets-store secret create 421985d6f384493c938c0554fea33c77 --name BETTER_AUTH_SECRET --scopes workers --remote`
3. Generate types and auth schema:
   - `bun run generate-types`
   - `bun run auth:generate`
   - `bun run db:generate`
4. Apply local migrations:
   - `bun x wrangler d1 migrations apply chat --local`
5. Replace the placeholder `database_id` in [`wrangler.jsonc`](/Users/icepuma/development/waddle/chat/wrangler.jsonc) with the real remote D1 database ID before running remote migrations or deploys.

## Notes

- `astro.config.ts` is the source of truth for Astro configuration.
- `chat` authenticates against the remote `https://colony.waddle.social` OIDC issuer using a public PKCE client with ID `waddle-chat`.
- `wrangler.jsonc` is aligned with the Cloudflare adapter setup used in `colony/`.
- The `cuenv` tasks use Bun only. Do not introduce `npm`, `pnpm`, or `yarn`.
