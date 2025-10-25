---

description: "Execution tasks for migrating to Bun workspaces with catalogs while keeping Colony"
---

# Tasks: Migrate Repo to Bun Workspaces with Catalogs (Keep Colony)

**Input**: Design documents from `/specs/001-migrate-bun-workspaces/`
**Prerequisites**: plan.md (required), spec.md (required for user stories), research.md, data-model.md, contracts/

**Tests**: Focus on smoke verification for install/build determinism and alias resolution; add unit/contract tests in follow‑ups if requested.

**Organization**: Tasks are grouped by user story to enable independent implementation and testing of each story.

## Format: `[ID] [P?] [Story] Description`

- [P]: Can run in parallel (different files, no dependencies)
- [Story]: Which user story this task belongs to (US1, US2, US3)
- All file paths are absolute

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: Prepare repo for a single Bun workspace flow

- [X] T001 Verify and, if needed, correct workspace globs in /Users/icepuma/development/waddle/package.json
- [X] T002 [P] Set `engines.bun` to ">=1.3.0" in /Users/icepuma/development/waddle/package.json
- [X] T003 [P] Confirm `packageManager` is `bun@1.3.0` in /Users/icepuma/development/waddle/package.json
- [X] T004 [P] Ensure text lockfile is enabled (`install.saveTextLockfile = true`) in /Users/icepuma/development/waddle/bunfig.toml
- [X] T005 [P] Align root scripts (dev, build, test, lint, format, typecheck) in /Users/icepuma/development/waddle/package.json
- [X] T006 [P] Add common build artifacts to ignore (e.g., `**/dist`, `**/.turbo`) in /Users/icepuma/development/waddle/.gitignore
- [X] T007 [P] Create cleanup script to remove legacy lockfiles if found at runtime in /Users/icepuma/development/waddle/scripts/cleanup-lockfiles.sh
- [X] T008 [P] Link Quickstart in README with a short section in /Users/icepuma/development/waddle/README.md

---

## Phase 2: Foundational (Blocking Prerequisites)

**Purpose**: Deterministic installs and smoke verification before story work

- [X] T009 Create deterministic install verifier script in /Users/icepuma/development/waddle/scripts/verify-install.sh
- [X] T010 [P] Add `verify:install` npm script to call the verifier in /Users/icepuma/development/waddle/package.json
- [X] T011 [P] Create smoke build script to build Colony and one shared package in /Users/icepuma/development/waddle/scripts/smoke-build.sh
- [X] T012 [P] Add `smoke:build` npm script wiring to the smoke script in /Users/icepuma/development/waddle/package.json
- [X] T013 [P] Document `verify:install` and `smoke:build` usage in /Users/icepuma/development/waddle/specs/001-migrate-bun-workspaces/quickstart.md

**Checkpoint**: Foundation ready — user story implementation can now begin in parallel

---

## Phase 3: User Story 1 — Root Developer Workflow (Priority: P1) 🎯 MVP

**Goal**: A developer can install, build, and test from the repo root; Colony fully participates in the new workflow without behavior regressions.

**Independent Test**: Fresh clone → `bun install` at root → `bun run build` builds Colony and one other app/package → `bun test` runs workspace tests without per‑package setup.

### Implementation for User Story 1

- [X] T014 [US1] Harmonize Colony scripts to expected names (dev/build/preview) in /Users/icepuma/development/waddle/colony/website/package.json
- [X] T015 [P] [US1] Ensure Waddle website builds via root commands in /Users/icepuma/development/waddle/waddle/website/package.json
- [X] T016 [P] [US1] Ensure UI library participates in root build in /Users/icepuma/development/waddle/shared/packages/ui-web/package.json
- [X] T017 [US1] Expand Quickstart with a "Working on Colony" section in /Users/icepuma/development/waddle/specs/001-migrate-bun-workspaces/quickstart.md

**Checkpoint**: US1 independently verifiable via Quickstart steps

---

## Phase 4: User Story 2 — Deterministic CI (Priority: P2)

**Goal**: CI performs workspace-aware install once and re‑runs deterministically with no changes.

**Independent Test**: CI job runs `bun install` once, caches by lockfile; re‑running install on the same commit produces no diffs and completes faster.

### Implementation for User Story 2

- [X] T018 [US2] Add Bun setup step to CI in /Users/icepuma/development/waddle/.github/workflows/deployment.yaml
- [X] T019 [P] [US2] Add `bun install` step before build/deploy in /Users/icepuma/development/waddle/.github/workflows/deployment.yaml
- [X] T020 [P] [US2] Add dependency cache keyed by lockfile in /Users/icepuma/development/waddle/.github/workflows/deployment.yaml
- [X] T021 [P] [US2] Add `verify:install` step post‑install in /Users/icepuma/development/waddle/.github/workflows/deployment.yaml
- [X] T022 [US2] Document CI flow (install + verify) in /Users/icepuma/development/waddle/specs/001-migrate-bun-workspaces/quickstart.md

**Checkpoint**: US2 independently verifiable by viewing CI logs and reruns on the same commit

---

## Phase 5: User Story 3 — Catalog Aliases (Priority: P3)

**Goal**: Internal packages resolve via stable catalog aliases; imports no longer use brittle relative paths.

**Independent Test**: Replace at least one import in each of Colony and Waddle website to use an alias; build succeeds without extra config.

### Implementation for User Story 3

- [X] T023 [US3] Populate internal aliases in `[catalog]` of /Users/icepuma/development/waddle/bunfig.toml (e.g., `"@waddle/ui-web" = "*"`, `"@waddle/types" = "*"`, `"@waddle/core" = "*"`, `"@waddle/auth" = "*"`)
- [X] T024 [P] [US3] Ensure consuming deps reference internal packages using workspace spec in /Users/icepuma/development/waddle/colony/website/package.json
- [X] T025 [P] [US3] Ensure consuming deps reference internal packages using workspace spec in /Users/icepuma/development/waddle/waddle/website/package.json
- [X] T026 [P] [US3] Update imports to use aliases in /Users/icepuma/development/waddle/colony/website/**/**/*.{ts,tsx,js,jsx,vue,astro}
- [X] T027 [P] [US3] Update imports to use aliases in /Users/icepuma/development/waddle/waddle/website/**/**/*.{ts,tsx,js,jsx,vue,astro}
- [X] T028 [US3] Add "Using aliases" section to Quickstart in /Users/icepuma/development/waddle/specs/001-migrate-bun-workspaces/quickstart.md

**Checkpoint**: US3 independently verifiable by building after alias adoption in each target app

---

## Phase N: Polish & Cross-Cutting Concerns

**Purpose**: Consolidate docs and ensure repo‑wide coherence

- [ ] T029 [P] Update top‑level README with Workspace & Catalog overview in /Users/icepuma/development/waddle/README.md
- [ ] T030 Lint and format repository per root scripts in /Users/icepuma/development/waddle/package.json
- [ ] T031 [P] Run `verify:install` and `smoke:build` and record outcomes in /Users/icepuma/development/waddle/specs/001-migrate-bun-workspaces/quickstart.md

---

## Dependencies & Execution Order

### Phase Dependencies

- Setup (Phase 1): No dependencies
- Foundational (Phase 2): Depends on Phase 1 — BLOCKS all user stories
- US1 (Phase 3): Depends on Phase 2 — MVP
- US2 (Phase 4): Depends on Phase 2 (can run parallel to US1 once Phase 2 completes)
- US3 (Phase 5): Depends on Phase 2 (can run parallel to US1/US2)
- Polish: Depends on stories selected for delivery

### User Story Dependencies

- US1 (P1): No dependency on other stories; validates root developer workflow
- US2 (P2): Independent of US1; validates CI determinism
- US3 (P3): Independent of US1/US2; validates alias resolution via catalog

### Within Each User Story

- Perform smoke verification steps after implementation tasks
- Keep story changes scoped to the listed files to preserve independence

---

## Parallel Examples

### User Story 1 (Root Developer Workflow)

```bash
# In parallel after Phase 2:
Task: "Ensure Waddle website builds via root" (T015)
Task: "Ensure UI library participates in root build" (T016)
```

### User Story 2 (Deterministic CI)

```bash
# In parallel after Phase 2:
Task: "Add bun install step" (T019)
Task: "Add cache by lockfile" (T020)
Task: "Add verify:install step" (T021)
```

### User Story 3 (Catalog Aliases)

```bash
# In parallel after Phase 2:
Task: "Ensure deps use workspace spec in Colony" (T024)
Task: "Ensure deps use workspace spec in Waddle website" (T025)
Task: "Refactor imports to aliases (Colony)" (T026)
Task: "Refactor imports to aliases (Waddle website)" (T027)
```

---

## Implementation Strategy

### MVP First (User Story 1 Only)

1. Complete Phase 1: Setup
2. Complete Phase 2: Foundational (deterministic install + smoke build)
3. Complete Phase 3: User Story 1 tasks (T014–T017)
4. Validate via Quickstart — this is the MVP

### Incremental Delivery

1. Complete Setup + Foundational → Foundation ready
2. Deliver US1 → Validate → Demo
3. Deliver US2 → Validate CI determinism
4. Deliver US3 → Validate alias adoption

### Parallel Team Strategy

After Phase 2 completes:
- Developer A: US1 (T014–T017)
- Developer B: US2 (T018–T022)
- Developer C: US3 (T023–T028)

---
