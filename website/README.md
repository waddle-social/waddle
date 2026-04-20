# Waddle Website

Cloudflare-hosted landing page for Waddle, built with Astro and Tailwind CSS.

## Setup

Install from the repository root:

```sh
bun install
```

Generate Cloudflare binding types from the `website/` folder:

```sh
bun run generate-types
```

The D1 binding is configured for the `waddle-social` database. There is no
active schema or migration yet; `src/db/schema.ts` is intentionally empty.

Database generation and migration commands are still present for when the
schema returns.

## Commands

```sh
bun run build
bun run db:generate
bun run db:migrate:local
bun run db:migrate:remote
bun run deploy
```
