# Research

- Decision: Generate the Topics microservice with `WaddleDataService` (includeWriteModel: false) mirroring `waddle/services/waddle`.
  - Rationale: Keeps service layout consistent (projen-managed config, Drizzle setup, Pothos schema) and minimizes bespoke boilerplate; see `waddle/services/waddle/generate.ts` for the proven pattern.
  - Alternatives considered: Hand-crafting a service scaffold (rejects automation, risks drift), extending the existing waddle service with topics (breaks microservice-per-feature guideline).

- Decision: Model topics in Drizzle as a `topics` table with `id` (uuid), `title`, `description`, optional `ownerId`, optional `waddleSlug`, `scope` enum (`global`, `owner`, `waddle`), and timestamps.
  - Rationale: Meets filter requirements (owner or waddle) while supporting global records; enum enforces scope discipline and eases query predicates.
  - Alternatives considered: Separate tables per scope (adds migrations complexity) or soft constraints via nullable foreign keys without scope enum (risks inconsistent data).

- Decision: Implement GraphQL queries (`getAllTopics`, `getTopics`) with Pothos + Drizzle plugin, using builder patterns from `read-model/src/schema.ts` in the waddle service.
  - Rationale: Reuses existing federation-compatible approach, enabling Drizzle-powered resolvers and schema directives without custom glue code.
  - Alternatives considered: Using raw GraphQL Yoga resolvers (more boilerplate, bypasses Pothos federation helpers) or merging into a REST endpoint (violates federation requirement).

- Decision: Enforce read-only behavior by omitting write model generation and covering it with contract tests that assert resolvers have no side effects.
  - Rationale: Aligns with constitution’s CQRS principle and user scope (topics status or writes belong to future services); simplifies initial delivery.
  - Alternatives considered: Bundling write functionality now (contradicts "status gets its own service" rule) or deferring tests (would fail Code Quality/Test principles).

- Decision: Apply Vitest via `bun test` for resolver and schema validation, plus Drizzle migration snapshot tests using the generated data-model package.
  - Rationale: Vitest ships with the template, integrates tightly with Bun, and supports worker environment mocks; migration tests catch schema drift early.
  - Alternatives considered: Relying solely on manual CLI verification (insufficient coverage), or introducing Jest (duplicates tooling, slower in Bun env).
