# Tasks: Topics Service Setup

**Input**: Design documents from `/specs/001-setup-topics-service/`
**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Tests are mandatory per the constitution—capture fail-first unit, integration, contract, accessibility, performance, and CQRS/federation validations for every story.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- `[P]` marks tasks that can proceed in parallel.
- `[Story]` identifies the user story (e.g., `US1`).
- Every task description references the concrete file path to touch.

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and basic structure

- [X] T001 Create topics service generator with `WaddleDataService` settings in `waddle/services/topics/generate.ts`
- [X] T002 Synthesize projen output so `waddle/services/topics/README.md` and scaffolded directories are generated
- [X] T003 [P] Add topics service workspace entry and helper scripts to `package.json`

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

- [X] T004 Implement topics Drizzle schema with scope constraints in `waddle/services/topics/data-model/schema.ts`
- [X] T005 [P] Define zod validators and enum exports in `waddle/services/topics/data-model/zod.ts`
- [X] T006 [P] Author initial D1 migration creating topics table in `waddle/services/topics/data-model/migrations/0000_lyrical_karen_page.sql`
- [X] T007 Add migration regression test in `waddle/services/topics/data-model/tests/topics.migration.test.ts`
- [X] T008 Configure Cloudflare D1 binding and env typing in `waddle/services/topics/read-model/wrangler.jsonc`
- [X] T009 [P] Scaffold Pothos builder and Drizzle client wiring in `waddle/services/topics/read-model/src/schema.ts`
- [X] T010 Establish Vitest config and npm scripts in `waddle/services/topics/read-model/vitest.config.ts`

**Checkpoint**: Foundation ready - user story implementation can now begin in parallel

---

## Phase 3: User Story 1 - Query Topics by Context (Priority: P1) 🎯 MVP

**Goal**: Allow authenticated users to filter topics by owner or waddle via GraphQL.

**Independent Test**: Execute `getTopics(filter: { waddleSlug })` and `getTopics(filter: { ownerId })` through the worker; responses must include only scoped topics with correct pagination metadata.

### Tests for User Story 1 (MANDATORY) ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [X] T011 [P] [US1] Add `getTopics` federation contract test in `waddle/services/topics/read-model/tests/contract/get-topics.contract.test.ts`
- [X] T012 [P] [US1] Add integration test covering owner/waddle filtering in `waddle/services/topics/read-model/tests/integration/get-topics.integration.test.ts`
- [X] T013 [P] [US1] Add unit tests for filter predicates in `waddle/services/topics/read-model/tests/unit/get-topics.service.test.ts`

### Implementation for User Story 1

- [X] T014 [US1] Implement filtered topic query logic in `waddle/services/topics/read-model/src/repositories/topics.repository.ts`
- [X] T015 [US1] Define `Topic` type and `TopicScope` enum exposure in `waddle/services/topics/read-model/src/schema.ts`
- [X] T016 [US1] Add `getTopics` field with cursor pagination in `waddle/services/topics/read-model/src/schema.ts`
- [X] T017 [US1] Add authorization guard ensuring caller scope in `waddle/services/topics/read-model/src/guards/authorize-topics.ts`
- [X] T018 [US1] Wire guard and resolver registration in `waddle/services/topics/read-model/src/index.ts`
- [X] T019 [US1] Update GraphQL contract defaults for `getTopics` in `specs/001-setup-topics-service/contracts/topics.graphql`

### Experience & Performance Validation (MANDATORY)

- [X] T020 [US1] Capture p95 latency evidence and federation validation notes in `specs/001-setup-topics-service/plan.md`

**Checkpoint**: At this point, User Story 1 should be fully functional and testable independently

---

## Phase 4: User Story 2 - Discover All Topics (Priority: P2)

**Goal**: Provide administrators tooling to list all topics with pagination via `getAllTopics`.

**Independent Test**: Call `getAllTopics` as an authorized admin and confirm paginated results include every topic across scopes while unauthorized callers are denied.

### Tests for User Story 2 (MANDATORY) ⚠️

- [X] T021 [P] [US2] Add `getAllTopics` federation contract test in `waddle/services/topics/read-model/tests/contract/get-all-topics.contract.test.ts`
- [X] T022 [P] [US2] Add integration test covering admin pagination in `waddle/services/topics/read-model/tests/integration/get-all-topics.integration.test.ts`
- [X] T023 [P] [US2] Add unit tests for admin list service in `waddle/services/topics/read-model/tests/unit/get-all-topics.service.test.ts`

### Implementation for User Story 2

- [X] T024 [US2] Extend repository with admin listing + cursor helpers in `waddle/services/topics/read-model/src/repositories/topics.repository.ts`
- [X] T025 [P] [US2] Implement admin authorization guard in `waddle/services/topics/read-model/src/guards/authorize-topics-admin.ts`
- [X] T026 [US2] Register `getAllTopics` query and admin guard in `waddle/services/topics/read-model/src/schema.ts`
- [X] T027 [US2] Add instrumentation for admin query in `waddle/services/topics/read-model/src/metrics/topics.metrics.ts`

### Experience & Performance Validation (MANDATORY)

- [X] T028 [US2] Document p95 results and CQRS notes for admin queries in `specs/001-setup-topics-service/plan.md`

**Checkpoint**: User Story 2 is independently functional and validated

---

## Phase 5: User Story 3 - Document GraphQL Access (Priority: P3)

**Goal**: Provide clear documentation and quickstart guidance for developers integrating with the Topics service.

**Independent Test**: Follow the documented quickstart to generate the service, migrate the database, and run sample `getTopics`/`getAllTopics` queries successfully.

### Tests for User Story 3 (MANDATORY) ⚠️

- [X] T029 [P] [US3] Add quickstart smoke test executing documented queries in `waddle/services/topics/read-model/tests/integration/quickstart-smoke.test.ts`

### Implementation for User Story 3

- [X] T030 [US3] Update service README with setup + query instructions in `waddle/services/topics/README.md`
- [X] T031 [US3] Append CLI walkthrough and sample queries to `specs/001-setup-topics-service/quickstart.md`
- [X] T032 [US3] Publish developer onboarding guide in `docs/prd/topics-service-quickstart.md`

### Experience & Performance Validation (MANDATORY)

- [X] T033 [US3] Record documentation validation evidence in `specs/001-setup-topics-service/spec.md`

**Checkpoint**: Documentation consumers can onboard independently using the published materials

---

## Phase 6: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [X] T034 [P] Add topics-focused build/test scripts to `package.json`
- [X] T035 [P] Export latest schema snapshot to `waddle/services/topics/read-model/schema.gql`
- [X] T036 Update performance + testing outcomes in `specs/001-setup-topics-service/plan.md`
- [X] T037 Refresh high-level docs to reference topics service in `docs/README.md`

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)** → prerequisite for all other phases
- **Foundational (Phase 2)** → depends on Phase 1; blocks Phase 3+
- **User Stories (Phase 3-5)** → each depends on Phase 2 completion; can proceed sequentially (P1 → P2 → P3) or in parallel once prerequisites satisfied
- **Polish (Phase 6)** → depends on completion of targeted user stories and tests

### User Story Dependencies

- **US1 (P1)** → only depends on Foundational work
- **US2 (P2)** → depends on Foundational and any shared repository logic from US1 (runs after US1 for safest sequencing)
- **US3 (P3)** → depends on foundational docs + runnable queries (after US1/US2 or once endpoints stable)

### Within Each User Story

1. Create fail-first tests (contract → integration → unit)
2. Implement repository/service logic
3. Wire GraphQL schema and guards
4. Capture performance & federation evidence

---

## Parallel Execution Examples

### User Story 1

- `T011`, `T012`, `T013` can be authored in parallel once test harness is ready.
- After repository work (`T014`), schema wiring (`T015` & `T016`) and guard implementation (`T017`) can progress concurrently by different contributors.

### User Story 2

- Contract/integration/unit tests (`T021`-`T023`) can proceed simultaneously.
- Admin guard (`T025`) and instrumentation (`T027`) can run parallel after repository updates (`T024`).

### User Story 3

- Quickstart smoke test (`T029`) and README updates (`T030`) can be tackled in parallel once endpoints are live.

---

## Independent Test Criteria

- **US1**: `bun test waddle/services/topics/read-model/tests/integration/get-topics.integration.test.ts` passes with scoped results
- **US2**: `bun test waddle/services/topics/read-model/tests/integration/get-all-topics.integration.test.ts` passes with admin pagination
- **US3**: Quickstart smoke test (`bun test waddle/services/topics/read-model/tests/integration/quickstart-smoke.test.ts`) validates documentation flow end-to-end

---

## Task Counts

- **Total tasks**: 37
- **Setup**: 3
- **Foundational**: 7
- **User Story 1 (P1)**: 10
- **User Story 2 (P2)**: 8
- **User Story 3 (P3)**: 5
- **Polish**: 4

Parallel opportunities identified in Phases 1, 2, 3, 4, 5, and 6 as marked with `[P]`.

---

## Suggested MVP Scope

Deliver Phase 1 → Phase 2 → Phase 3 (User Story 1). This enables scoped topic queries backed by tests, fulfilling the minimal end-user value and federation requirements.

---

## Implementation Strategy

1. Complete foundational setup (Phase 1 & 2) to establish repeatable scaffolding and schema integrity.
2. Implement MVP (US1) with fail-first tests, resolver logic, and contract validation.
3. Layer admin visibility (US2) ensuring authorization and instrumentation.
4. Finalize documentation and onboarding (US3) once endpoints are stable.
5. Execute polish tasks to capture evidence, publish schema snapshots, and update shared docs.
6. At each stage, run the targeted integration and contract tests to validate independence before progressing.
