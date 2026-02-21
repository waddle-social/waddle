# Chat

Minimal Astro + Svelte client for `waddle-server`, wired for Cloudflare Workers, Tailwind 4, Bun, and `cuenv`.

## Commands

Run these from `chat/`:

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

## Configuration

All environment variables live in `env.cue` — not in `wrangler.jsonc` or `.dev.vars`.

| Variable | Default (local) | Production |
|---|---|---|
| `SERVER_BASE_URL` | `http://localhost:3000` | `https://api.waddle.social` |

`cuenv task dev` passes vars to wrangler via `--var`, so no `.dev.vars` file is needed.

## Local Setup

1. Start the server: `cd ../server && cuenv task dev`
2. Start the chat: `cd ../chat && cuenv task dev`

The server's `local/waddle.env.example` already includes `WADDLE_CORS_ORIGINS=http://localhost:4321,http://localhost:3000`.

## Notes

- `astro.config.mjs` is the source of truth for Astro configuration.
- `chat` does not authenticate against Colony directly. It sends the browser to `waddle-server`, which owns the Waddle session and delegates to Colony.
- `wrangler.jsonc` holds only infra config (routes, assets, compat flags). No `vars` block.
- `cuenv task dev` runs the Wrangler local runtime for the built worker.
- The `cuenv` tasks use Bun only. Do not introduce `npm`, `pnpm`, or `yarn`.
