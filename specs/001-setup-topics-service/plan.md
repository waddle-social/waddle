# Implementation Plan: Topics Service Setup

**Branch**: `001-setup-topics-service` | **Date**: 2025-10-25 | **Spec**: `/specs/001-setup-topics-service/spec.md`
**Input**: Feature specification from `/specs/001-setup-topics-service/spec.md`

**Note**: This template is filled in by the `/speckit.plan` command. See `.specify/scripts/` for the execution workflow.

## Summary

Introduce a dedicated Topics microservice mirroring `waddle/services/waddle`, provisioned via the `WaddleDataService` projen generator. The service stores topics in Cloudflare D1 and exposes read-only GraphQL queries `getAllTopics` and `getTopics(filter)` to support global, owner, and waddle contexts, backed by Drizzle models, Pothos schema, and Bun-based tooling.

## Technical Context

<!--
  ACTION REQUIRED: Replace the content in this section with the technical details
  for the project. The structure here is presented in advisory capacity to guide
  the iteration process.
-->

**Language/Version**: TypeScript 5.8 (Bun toolchain)  
**Primary Dependencies**: Projen `WaddleDataService` generator, Drizzle ORM, Pothos GraphQL + federation plugins, GraphQL Yoga  
**Storage**: Cloudflare D1 (Drizzle migrations)  
**Testing**: `bun test`, `bun run lint`, `bun run typecheck`  
**Target Platform**: Cloudflare Workers (GraphQL federation subgraph)
**Project Type**: Cloudflare-native microservice under `waddle/services/`  
**Performance Goals**: p95 GraphQL resolver latency <= 150 ms using Workers traces  
**Constraints**: Read-only service; deterministic migrations; comply with federation and CQRS principles  
**Scale/Scope**: Topics across all waddles and personal scopes; anticipated <10k records initially, must support horizontal D1 sharding

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **Code Quality**: Enforce repository lint/type gates via `bun run lint` and `bun run typecheck`. Generator ensures strict TypeScript without `any`. Reuse existing federation helpers; no duplicate shared package creation.
- **Testing**: Implement fail-first tests for Drizzle migrations, resolver logic (Vitest under Bun), and federation composition contracts. Execute with `bun test` locally and in CI pipelines.
- **User Experience**: API-only feature; no UI changes. Accessibility and copy updates documented as N/A with rationale.
- **Performance**: Maintain p95 <= 150 ms for queries; instrument via Wrangler traces and optional custom metrics documented in quickstart.
- **Federated CQRS**: Expose topics through federation read subgraph only; omit write model per microservice strategy. Contract tests assert read-only behavior and gateway composition success.
- **Exceptions**: None planned; future write capabilities will require dedicated service adhering to CQRS.

Post-design check: Phase 1 artifacts uphold these gates; no exceptions identified.

- **Validation Evidence (2025-10-28)**  
- `bun run test:topics` exercises federation contract, unit, and integration suites for `getTopics` and `getAllTopics`, emitting per-query metrics via `recordTopicsQuery` for sampling latency instrumentation.  
- Console metric hooks confirm result counts for both queries; p95 monitoring will reuse these hooks once deployed to Workers traces.

## Project Structure

### Documentation (this feature)

```text
specs/001-setup-topics-service/
├── plan.md              # This file (/speckit.plan command output)
├── research.md          # Phase 0 output (/speckit.plan command)
├── data-model.md        # Phase 1 output (/speckit.plan command)
├── quickstart.md        # Phase 1 output (/speckit.plan command)
├── contracts/           # Phase 1 output (/speckit.plan command)
└── tasks.md             # Phase 2 output (/speckit.tasks command - NOT created by /speckit.plan)
```

### Source Code (repository root)
<!--
  ACTION REQUIRED: Replace the placeholder tree below with the concrete layout
  for this feature. Delete unused options and expand the chosen structure with
  real paths (e.g., apps/admin, packages/something). The delivered plan must
  not include Option labels.
-->

```text
waddle/
└── services/
    ├── waddle/              # existing reference implementation
    └── topics/              # new generated service
        ├── data-model/
        ├── read-model/
        └── generate.ts
```

**Structure Decision**: Extend `waddle/services/` with a `topics` directory generated via projen, matching the existing `waddle` service layout for consistency.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
| None | - | - |
