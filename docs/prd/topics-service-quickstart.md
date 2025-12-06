# Topics Service Onboarding Guide

This quickstart captures the minimal steps for a developer to bootstrap and verify the Topics read model locally.

## Prerequisites

- Bun 1.3+
- Cloudflare Wrangler 4.45+
- Access to a Cloudflare D1 database (or a local test binding)

## Workflow

1. **Generate the service scaffold**
   ```bash
   bun install
   bunx tsx waddle/services/topics/generate.ts
   ```
2. **Configure Wrangler bindings** (`waddle/services/topics/read-model/wrangler.jsonc`)
   - Set the `DB` binding (`database_id`) to your D1 instance.
3. **Apply migrations**
   ```bash
   bunx drizzle-kit push --config waddle/services/topics/data-model/drizzle.config.ts
   ```
4. **Run the worker locally**
   ```bash
   bun run --filter @waddlesocial/waddle-service-topics-read-model dev
   ```
   - GraphQL endpoint: `http://127.0.0.1:8787/graphql`
5. **Execute validation suite**
   ```bash
   bun run test:topics
   ```
   - Includes federation contract checks, scoped/global integration tests, and a quickstart smoke test.
6. **Publish schema snapshot (optional)**
   ```bash
   bun run --filter @waddlesocial/waddle-service-topics-read-model schema:publish
   ```

## Sample Queries

```graphql
query OwnerTopics($owner: ID!) {
  getTopics(filter: { ownerId: $owner }) {
    edges { node { id title scope } }
  }
}

query AllTopics($first: Int, $after: String) {
  getAllTopics(pagination: { first: $first, after: $after }) {
    totalCount
    pageInfo { endCursor hasNextPage }
    edges { cursor node { id title scope } }
  }
}
```

## Notes

- AuthN/AuthZ is intentionally open for the initial iteration; future revisions will delegate to SpiceDB.
- Metric hooks (`recordTopicsQuery`) emit query result counts for p95 latency monitoring once deployed.
