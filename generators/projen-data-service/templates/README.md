# {{ serviceNamePascal }} Service

Data service read model for the Waddle platform.

## Stack

- Cloudflare Workers bundled with Vite (`@cloudflare/vite-plugin`)
- GraphQL Yoga with a Pothos schema builder
- Cloudflare D1 via Drizzle ORM
- Bun scripts with Wrangler for deployment

## Layout

```
{{ serviceName }}/
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
{%- if includeWriteModel %}
├── write-model/
│   └── wrangler.jsonc
{%- endif %}
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

Create or link a Cloudflare D1 database for `platform-{{ serviceName }}` and update the `database_id` inside `read-model/wrangler.jsonc`.

Migrations live in `data-model/migrations`. Generate them with Drizzle Kit and apply them with Wrangler.
The Drizzle configuration is defined in `data-model/drizzle.config.ts`.

## Deployment

```bash
bun run deploy
```

This builds the worker with Vite and deploys it using Wrangler.
