---

description: "Task list template for feature implementation"
---

# Tasks: [FEATURE NAME]

**Input**: Design documents from `/specs/[###-feature-name]/`
**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Tests are mandatory per the constitution—capture fail-first unit, integration, contract, accessibility, performance, and CQRS/federation validations for every story.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: Can run in parallel (different files, no dependencies)
- **[Story]**: Which user story this task belongs to (e.g., US1, US2, US3)
- Include exact file paths in descriptions

## Path Conventions

- **Single project**: `src/`, `tests/` at repository root
- **Web app**: `backend/src/`, `frontend/src/`
- **Mobile**: `api/src/`, `ios/src/` or `android/src/`
- Paths shown below assume single project - adjust based on plan.md structure
- **Data service**: `waddle/services/<name>/` with `generate.ts`, `data-model/`, `read-model/`, `read-model/schema.gql`, Bun scripts (`bun run test:<name>`) captured in package scripts, and docs updates linked in plan/spec.

### Data Service-Specific Tasks *(add when applicable)*

- Generator synthesis (`bunx tsx waddle/services/<name>/generate.ts`) with recorded options.
- D1 schema/migration updates in `data-model/` and corresponding Zod exports.
- Schema snapshot refresh (`read-model/schema.gql`) and Wrangler binding configuration.
- Quickstart/documentation validation (docs/README.md, service README, specs quickstart).

<!-- 
  ============================================================================
  IMPORTANT: The tasks below are SAMPLE TASKS for illustration purposes only.
  
  The /speckit.tasks command MUST replace these with actual tasks based on:
  - User stories from spec.md (with their priorities P1, P2, P3...)
  - Feature requirements from plan.md
  - Entities from data-model.md
  - Endpoints from contracts/
  
  Tasks MUST be organized by user story so each story can be:
  - Implemented independently
  - Tested independently
  - Delivered as an MVP increment
  
  DO NOT keep these sample tasks in the generated tasks.md file.
  ============================================================================
-->

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Project initialization and basic structure

- [ ] T001 Create project structure per implementation plan
- [ ] T002 Initialize [language] project with [framework] dependencies
- [ ] T003 [P] Configure linting, formatting, and strict type-check commands documented in the plan
- [ ] T004 [P] Establish shared test harness (`bun test`, `moon run test:*`, etc.) with sample fail-first cases covering GraphQL federation and workflow orchestration.
- [ ] T005 [P] Configure performance monitoring or logging baselines to measure p95 response and p75 LCP, plus workflow and federation tracing hooks.

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Core infrastructure that MUST be complete before ANY user story can be implemented

**⚠️ CRITICAL**: No user story work can begin until this phase is complete

Examples of foundational tasks (adjust based on your project):

- [ ] T006 Setup database schema and migrations framework
- [ ] T007 [P] Implement authentication/authorization framework
- [ ] T008 [P] Setup API routing, GraphQL federation gateway composition, and middleware structure
- [ ] T009 Create base models/entities that all stories depend on
- [ ] T010 Configure error handling and logging infrastructure
- [ ] T011 Setup environment configuration management

**Checkpoint**: Foundation ready - user story implementation can now begin in parallel

---

## Phase 3: User Story 1 - [Title] (Priority: P1) 🎯 MVP

**Goal**: [Brief description of what this story delivers]

**Independent Test**: [How to verify this story works on its own]

### Tests for User Story 1 (MANDATORY) ⚠️

> **NOTE: Write these tests FIRST, ensure they FAIL before implementation**

- [ ] T012 [P] [US1] Contract test for [endpoint] in tests/contract/test_[name].ts
- [ ] T013 [P] [US1] Integration test for [user journey] in tests/integration/test_[name].ts
- [ ] T014 [P] [US1] Unit tests for [module] in tests/unit/test_[name].ts

### Implementation for User Story 1

- [ ] T015 [P] [US1] Create [Entity1] model in src/models/[entity1].ts
- [ ] T016 [P] [US1] Create [Entity2] model in src/models/[entity2].ts
- [ ] T017 [US1] Implement [Service] in src/services/[service].ts (depends on T015, T016)
- [ ] T018 [US1] Implement [endpoint/feature] in src/[location]/[file].ts
- [ ] T019 [US1] Add validation and error handling
- [ ] T020 [US1] Add logging and telemetry for user story 1 operations, including workflow execution traces

### Experience & Performance Validation (MANDATORY)

- [ ] T021 [US1] Perform accessibility audit (WCAG 2.1 AA) using shared components from `shared/packages/ui-web`
- [ ] T022 [US1] Measure performance budgets (p95 <= 150 ms, p75 LCP <= 2.5 s) and document evidence in plan/spec alongside CQRS/federation validation artifacts

**Checkpoint**: At this point, User Story 1 should be fully functional and testable independently

---

## Phase 4: User Story 2 - [Title] (Priority: P2)

**Goal**: [Brief description of what this story delivers]

**Independent Test**: [How to verify this story works on its own]

### Tests for User Story 2 (MANDATORY) ⚠️

- [ ] T023 [P] [US2] Contract test for [endpoint] in tests/contract/test_[name].ts
- [ ] T024 [P] [US2] Integration test for [user journey] in tests/integration/test_[name].ts
- [ ] T025 [P] [US2] Unit tests for [module] in tests/unit/test_[name].ts

### Implementation for User Story 2

- [ ] T026 [P] [US2] Create [Entity] model in src/models/[entity].ts
- [ ] T027 [US2] Implement [Service] in src/services/[service].ts
- [ ] T028 [US2] Implement [endpoint/feature] in src/[location]/[file].ts
- [ ] T029 [US2] Integrate with User Story 1 components (if needed) using shared modules

### Experience & Performance Validation (MANDATORY)

- [ ] T030 [US2] Perform accessibility audit (WCAG 2.1 AA) for new/updated UI
- [ ] T031 [US2] Measure performance budgets (p95 <= 150 ms, p75 LCP <= 2.5 s) and document evidence

**Checkpoint**: At this point, User Stories 1 AND 2 should both work independently

---

## Phase 5: User Story 3 - [Title] (Priority: P3)

**Goal**: [Brief description of what this story delivers]

**Independent Test**: [How to verify this story works on its own]

### Tests for User Story 3 (MANDATORY) ⚠️

- [ ] T032 [P] [US3] Contract test for [endpoint] in tests/contract/test_[name].ts
- [ ] T033 [P] [US3] Integration test for [user journey] in tests/integration/test_[name].ts
- [ ] T034 [P] [US3] Unit tests for [module] in tests/unit/test_[name].ts

### Implementation for User Story 3

- [ ] T035 [P] [US3] Create [Entity] model in src/models/[entity].ts
- [ ] T036 [US3] Implement [Service] in src/services/[service].ts
- [ ] T037 [US3] Implement [endpoint/feature] in src/[location]/[file].ts

### Experience & Performance Validation (MANDATORY)

- [ ] T038 [US3] Perform accessibility audit (WCAG 2.1 AA) for new/updated UI
- [ ] T039 [US3] Measure performance budgets (p95 <= 150 ms, p75 LCP <= 2.5 s) and document evidence plus CQRS/federation validation

**Checkpoint**: All user stories should now be independently functional

---

[Add more user story phases as needed, following the same pattern]

---

## Phase N: Polish & Cross-Cutting Concerns

**Purpose**: Improvements that affect multiple user stories

- [ ] TXXX [P] Documentation updates in docs/
- [ ] TXXX Code cleanup and refactoring
- [ ] TXXX Performance optimization across all stories with updated metrics
- [ ] TXXX [P] Expand automated test coverage (unit, integration, contract) where risks remain
- [ ] TXXX Accessibility regression sweep (WCAG 2.1 AA)
- [ ] TXXX Security hardening
- [ ] TXXX Run quickstart.md validation

---

## Dependencies & Execution Order

### Phase Dependencies

- **Setup (Phase 1)**: No dependencies - can start immediately
- **Foundational (Phase 2)**: Depends on Setup completion - BLOCKS all user stories
- **User Stories (Phase 3+)**: All depend on Foundational phase completion
  - User stories can then proceed in parallel (if staffed)
  - Or sequentially in priority order (P1 → P2 → P3)
- **Polish (Final Phase)**: Depends on all desired user stories being complete

### User Story Dependencies

- **User Story 1 (P1)**: Can start after Foundational (Phase 2) - No dependencies on other stories
- **User Story 2 (P2)**: Can start after Foundational (Phase 2) - May integrate with US1 but should be independently testable
- **User Story 3 (P3)**: Can start after Foundational (Phase 2) - May integrate with US1/US2 but should be independently testable

### Within Each User Story

- Tests MUST be written and FAIL before implementation
- Models before services
- Services before endpoints
- Core implementation before integration
- Story complete before moving to next priority

### Parallel Opportunities

- All Setup tasks marked [P] can run in parallel
- All Foundational tasks marked [P] can run in parallel (within Phase 2)
- Once Foundational phase completes, all user stories can start in parallel (if team capacity allows)
- All tests for a user story marked [P] can run in parallel
- Models within a story marked [P] can run in parallel
- Different user stories can be worked on in parallel by different team members

---

## Parallel Example: User Story 1

```bash
# Launch all tests for User Story 1 together:
Task: "Contract test for [endpoint] in tests/contract/test_[name].ts"
Task: "Integration test for [user journey] in tests/integration/test_[name].ts"

# Launch all models for User Story 1 together:
Task: "Create [Entity1] model in src/models/[entity1].ts"
Task: "Create [Entity2] model in src/models/[entity2].ts"
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (CRITICAL - blocks all stories)
3. Complete Phase 3: User Story 1
4. **STOP and VALIDATE**: Test User Story 1 independently
5. Deploy/demo if ready

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Add User Story 1 → Test independently → Deploy/Demo (MVP!)
3. Add User Story 2 → Test independently → Deploy/Demo
4. Add User Story 3 → Test independently → Deploy/Demo
5. Each story adds value without breaking previous stories

### Parallel Team Strategy

With multiple developers:

1. Team completes Setup + Foundational together
2. Once Foundational is done:
   - Developer A: User Story 1
   - Developer B: User Story 2
   - Developer C: User Story 3
3. Stories complete and integrate independently

---

## Notes

- [P] tasks = different files, no dependencies
- [Story] label maps task to specific user story for traceability
- Each user story should be independently completable and testable
- Verify tests fail before implementing
- Commit after each task or logical group
- Stop at any checkpoint to validate story independently
- Avoid: vague tasks, same file conflicts, cross-story dependencies that break independence
