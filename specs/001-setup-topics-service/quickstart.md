# Topics Service Quickstart

Follow these steps to scaffold and verify the Topics microservice using the existing projen workflow.

## 1. Generate the service scaffold

```bash
# from repository root
bun install      # ensure generator dependencies are available
bunx tsx waddle/services/topics/generate.ts
```

The generator emits:
- `waddle/services/topics/data-model` (Drizzle schema + migrations skeleton)
- `waddle/services/topics/read-model` (GraphQL worker + schema stub)

## 2. Configure environment

Set the Cloudflare bindings in `read-model/wrangler.jsonc`:
- `D1_DATABASE` → Topics D1 instance ID (e.g., `TOPICS_DB`)
- `GRAPHQL_ENDPOINT` → federation gateway publish target (if required)

## 3. Implement schema & migrations

1. Define the topics table in `data-model/schema.ts` following `specs/001-setup-topics-service/data-model.md`.
2. Regenerate Drizzle migration SQL:
   ```bash
   cd waddle/services/topics/data-model
   bunx drizzle-kit generate
   ```
3. Export zod validators alongside the schema for runtime validation.

## 4. Run database migrations (local D1)

Activate the migrations using drizzle-kit or Wrangler:

```bash
cd waddle/services/topics/data-model
# Option A: drizzle-kit
bunx drizzle-kit push
# Option B: Wrangler (after binding name is set)
bunx wrangler d1 migrations apply TOPICS_DB
```

## 5. Extend the GraphQL schema and resolvers

1. Update `read-model/src/schema.ts` to:
   - Import the topics table from the data-model package.
   - Register `TopicScope` enum and `Topic` object type with federation `@key` directives.
   - Add `getTopics` and `getAllTopics` query fields that use Drizzle selectors with filter + pagination helpers.
   - Wire `authorizeTopics`/`authorizeTopicsAdmin` (currently no-ops) and emit `recordTopicsQuery` metrics.
2. Publish the schema snapshot for gateway review:
   ```bash
   cd waddle/services/topics
   bun run schema:publish
   ```

## 6. Testing & verification

```bash
cd waddle/services/topics
bun run lint
bun run typecheck
bun run test:topics
```

- Contract tests validate federation schema shape; integration suites exercise scoped (`getTopics`) and global (`getAllTopics`) pagination plus the quickstart smoke path.
- Console metrics (`recordTopicsQuery`) surface result counts that feed future p95 instrumentation.

## 7. Local preview

```bash
cd waddle/services/topics
bun run dev
```

The worker exposes an open GraphQL endpoint at `http://127.0.0.1:8787/graphql`. Sample queries live in `contracts/topics.graphql` and in the quickstart smoke test.

## 8. Deployment checklist

- Ensure Wrangler bindings reference production D1 instance IDs.
- Confirm D1 migrations have run via `bun run migrate:prod` or CI equivalent.
- Attach the service to the federation gateway deployment pipeline before enabling traffic.
