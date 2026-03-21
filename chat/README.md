# Chat

Astro + TypeScript app for `chat/`, wired for Cloudflare Workers, Tailwind 4, Bun, and `cuenv`.

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

## Local Setup

1. Copy `.dev.vars.example` to `.dev.vars`.
2. Set `SERVER_BASE_URL` in `.dev.vars` to `http://localhost:3000` for full local development.
3. Generate Cloudflare runtime types if needed:
   - `bun run generate-types`

## Notes

- `astro.config.ts` is the source of truth for Astro configuration.
- `chat` does not authenticate against Colony directly. It sends the browser to `server.waddle.social`, which owns the Waddle session and delegates to Colony.
- `wrangler.jsonc` is aligned with the Cloudflare adapter setup used in `colony/`.
- The `cuenv` tasks use Bun only. Do not introduce `npm`, `pnpm`, or `yarn`.
