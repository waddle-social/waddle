# Feature Specification: Topics Service Setup

**Feature Branch**: `001-setup-topics-service`  
**Created**: 2025-10-25  
**Status**: Draft  
**Input**: User description: "Copy the projen/microservice approach for ./waddle/services/waddle to add Topics with GraphQL getAllTopics and filtered getTopics endpoints."

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Query Topics by Context (Priority: P1)

Authenticated users query topics scoped to their waddle or globally via GraphQL so they can filter conversational context.

**Why this priority**: Enables end users to surface topics relevant to their current collaboration context, the core value of the feature.

**Independent Test**: From a GraphQL client, execute `getTopics(filter: { waddleId })` and receive only topics linked to that waddle; repeat with `filter: { ownerId }` for personal topics.

**Acceptance Scenarios**:

1. **Given** a waddle with topics A and B and user-owned topic C, **When** the client queries `getTopics(filter: { waddleId: <waddle> })`, **Then** the response includes A and B only with their metadata.
2. **Given** the same user, **When** the client queries `getTopics(filter: { ownerId: <user> })`, **Then** the response includes topic C only.

---

### User Story 2 - Discover All Topics (Priority: P2)

Administrators or tooling can list all topics across contexts via `getAllTopics` to audit or seed downstream caches.

**Why this priority**: Supports governance and operational needs without building separate tooling.

**Independent Test**: Execute `getAllTopics` via GraphQL and receive the complete ordered list of topics with pagination metadata.

**Acceptance Scenarios**:

1. **Given** topics exist in multiple waddles and owners, **When** `getAllTopics` is requested, **Then** the response includes each topic with associated context identifiers and respects pagination defaults.

---

### User Story 3 - Document GraphQL Access (Priority: P3)

Developers integrating with the Topics service access quickstart documentation describing the contract and workspace commands.

**Why this priority**: Ensures internal teams adopt the microservice easily and reduces onboarding time.

**Independent Test**: Follow quickstart instructions from a clean checkout to generate the service, run migrations, and execute sample queries successfully.

**Acceptance Scenarios**:

1. **Given** a clean repository checkout, **When** the documented quickstart commands are run, **Then** the Topics service scaffold is generated and D1 migrations applied without manual edits.
2. **Given** the quickstart’s GraphQL examples, **When** they are executed, **Then** responses match the documented shape.

---

### Edge Cases

- No topics exist for the provided filter.
- A user requests both owner and waddle filters simultaneously—define precedence or allow intersection.
- Unauthorized callers attempt to query topics outside their scope.
- D1 schema evolves; migrations should avoid downtime for read-only queries.

## Quality & Non-Functional Standards *(mandatory)*

- **Code Quality**: Topics service generation follows the existing `WaddleDataService` projen pattern, inheriting lint/type checks from the shared service template. No duplicate shared logic; reuse GraphQL federation schema utilities under `waddle/services/`.
- **Testing Strategy**: Add fail-first tests covering D1 schema migrations (drizzle), GraphQL resolvers (unit + contract), and GraphQL federation composition checks. Commands run via `bun test` and `bun run lint` both locally and in CI.
- **User Experience**: Not user-facing UI; no `shared/packages/ui-web` components required. Accessibility and copy updates are N/A; document reasoning.
- **Performance Budgets**: Service must respond within p95 <= 150 ms using Cloudflare Workers instrumentation (console metrics + Wrangler traces). No LCP requirement because service is API-only.
- **Federated CQRS**: Topics resolvers exposed through GraphQL federation read subgraph. All write paths are out of scope for this iteration; document read-only behavior and ensure contract tests assert absence of writes.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: Provide `getAllTopics` GraphQL query returning all topics with identifiers, owners, and waddle associations.
- **FR-002**: Provide `getTopics(filter: { ownerId?: ID; waddleId?: ID })` GraphQL query supporting owner or waddle scoped retrieval.
- **FR-003**: Persist topics in Cloudflare D1 with schema supporting global, owner, and waddle scopes plus audit metadata.
- **FR-004**: Enforce authorization checks consistent with requesting user context before returning topics (details TBD by existing auth middleware).
- **FR-005**: Deliver migrations and seeding scripts via projen generator so environments can bootstrap consistently.

### Key Entities *(include if feature involves data)*

- **Topic**: Represents a labeled annotation optionally linked to a waddle or owned globally by a user; fields include `id`, `title`, `description`, `ownerId`, `waddleId`, `visibility`, timestamps.

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: GraphQL queries `getAllTopics` and `getTopics` respond with valid data in <= 150 ms p95 under synthetic load of 100 RPS.
- **SC-002**: Running documented quickstart on a clean checkout provisions schema and returns sample query results without manual edits.
- **SC-003**: Contract tests against the federation gateway pass, verifying schema composition and resolver outputs for global and filtered queries.
- **SC-004**: D1 migrations apply idempotently across staging and production without manual intervention.
- **SC-005**: Quickstart documentation validated on 2025-10-28 via `bun run test:topics` quickstart smoke suite.
