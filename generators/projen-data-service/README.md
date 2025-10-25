# Waddle Data Service Template

This package provides a [projen](https://github.com/projen/projen) template for creating Waddle data services that run on Cloudflare Workers. It scaffolds a Bun-powered project with a federated read model, optional write model, and sensible defaults for Drizzle ORM and Vitest.

## Features

- GraphQL Yoga read API packaged for Cloudflare Workers using the `@cloudflare/vite-plugin`
- Drizzle ORM configuration for Cloudflare D1
- Pothos schema builder with a ready-to-extend schema stub
- Optional write model targeting Cloudflare Workflows
- Managed Biome, TypeScript, and Wrangler configuration
- Bun-friendly scripts for local development and deployment

## Installation

```bash
# pick the package manager that suits you
bun add -d projen-data-service
# yarn add -D projen-data-service
# npm install -D projen-data-service
```

## Usage

Create or update your `.projenrc.ts`:

```ts
import { WaddleDataService } from "projen-data-service";

const service = new WaddleDataService({
  serviceName: "waddle-service-chirps",
  databaseId: process.env.D1_DATABASE_ID ?? "TODO-D1-ID",
  includeWriteModel: false,
});

service.synth();
```

Then synthesize the project:

```bash
npx projen
```

## Options

- `serviceName` *(string, required)* – kebab-case name for the service (used for package name and Cloudflare resources).
- `databaseId` *(string, required)* – Cloudflare D1 database identifier for the read/write models.
- `includeWriteModel` *(boolean, default: `false`)* – generate the Cloudflare Workflows write model wrapper.
- `additionalDependencies` *(Record<string, string>)* – extra runtime dependencies merged into the generated `package.json`.
- `additionalDevDependencies` *(Record<string, string>)* – extra development dependencies merged into the generated `package.json`.

## Generated Layout

```
<service-name>/
├── README.md
├── package.json
├── tsconfig.json
├── biome.json
├── drizzle.config.ts
├── data-model/
│   └── migrations/
├── read-model/
│   ├── publish.ts
│   ├── vite.config.ts
│   ├── wrangler.jsonc
│   └── src/
│       ├── index.ts
│       └── schema.ts
└── write-model/ (only when includeWriteModel is true)
    └── wrangler.jsonc
```

## NPM Scripts

The generated `package.json` contains:

- `dev` – `vite dev --config read-model/vite.config.ts`
- `build` – `vite build --config read-model/vite.config.ts`
- `preview` – local preview of the built worker
- `deploy` – build then deploy the worker with Wrangler
- `schema:publish` – emit the current schema snapshot to `read-model/schema.gql`

Run them with `bun run`, `npm run`, or `yarn` depending on your tool of choice.

## Next Steps

1. Implement your Drizzle schema in `data-model/schema.ts`.
2. Extend the placeholder Pothos builder in `read-model/src/schema.ts` with your fields and resolvers.
3. Configure any additional bindings (KV, R2, etc.) in `read-model/wrangler.jsonc`.
4. (Optional) Enable the write model and add workflows as your service requires.
