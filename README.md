# Waddle Service

<<<<<<< HEAD
## Workspace Quickstart

This repository uses a single workspace at the root with internal catalogs.

- Install: `bun install`
- Build: `bun run build`
- Test: `bun test`

See full instructions: `specs/001-migrate-bun-workspaces/quickstart.md`.
A modern web application built with Astro and deployed on Cloudflare Workers.
||||||| parent of 044f032 (feat: better package isolation via projen)
A modern web application built with Astro and deployed on Cloudflare Workers.
=======
Data service read model for the Waddle platform.
>>>>>>> 044f032 (feat: better package isolation via projen)

## Stack

- Cloudflare Workers bundled with Vite (`@cloudflare/vite-plugin`)
- GraphQL Yoga with a Pothos schema builder
- Cloudflare D1 via Drizzle ORM
- Bun scripts with Wrangler for deployment

## Layout

```
waddle/
├── data-model/
│   ├── drizzle.config.ts
│   ├── schema.ts
│   └── migrations/
├── read-model/
│   ├── publish.ts
│   ├── vite.config.ts
│   ├── wrangler.jsonc
│   └── src/
│       ├── index.ts
│       └── schema.ts
└── package.json
```

## Getting Started

```bash
bun install
bun run dev
```

This starts the read model with `vite dev --config read-model/vite.config.ts`.

To generate an initial schema file:

```bash
bun run schema:publish
```

## Database

Create or link a Cloudflare D1 database for `platform-waddle` and update the `database_id` inside `read-model/wrangler.jsonc`.

Migrations live in `data-model/migrations`. Generate them with Drizzle Kit and apply them with Wrangler.
The Drizzle configuration is defined in `data-model/drizzle.config.ts`.

## Deployment

```bash
bun run deploy
```
<<<<<<< HEAD
waddle/
├── services/
│   └── website/        # Main web application
└── .moon/             # Moon build configuration
```
||||||| parent of 044f032 (feat: better package isolation via projen)
waddle/
├── services/
│   └── website/        # Main web application
└── .moon/             # Moon build configuration
```
=======

This builds the worker with Vite and deploys it using Wrangler.
>>>>>>> 044f032 (feat: better package isolation via projen)
