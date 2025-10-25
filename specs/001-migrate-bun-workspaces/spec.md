# Feature Specification: Migrate Repo to Bun Workspaces with Catalogs (Keep Colony)

**Feature Branch**: `001-migrate-bun-workspaces`  
**Created**: October 25, 2025  
**Status**: Draft  
**Input**: User description: "we want to keep colony, but want to move the entire repo to bun workspaces (https://bun.com/docs/pm/workspaces) with catalogs (https://bun.com/docs/pm/catalogs). can you do this for us"

## User Scenarios & Testing *(mandatory)*

### User Story 1 - Developer installs and works across all packages from the repo root (Priority: P1)

A developer can clone the repository and, from the root, install dependencies and run common tasks (develop, build, test) across all packages via a single workspace-aware workflow. The existing "colony" app/package remains usable after a full migration to the new workflow; scripts/tooling may change as needed without regressing behavior.

**Why this priority**: Enables every contributor to be productive immediately and validates the new workspace model end‑to‑end.

**Independent Test**: From a fresh clone, running the documented install and a sample task at the root succeeds across at least two apps and one shared package without manual linking or per‑package setup.

**Acceptance Scenarios**:

1. **Given** a fresh clone, **When** the documented install command is executed at the root, **Then** all workspace packages are discovered and installed as a single graph, with internal links created.
2. **Given** a fresh clone after installation, **When** the documented build command is executed at the root, **Then** at least one app in `colony` and one in another area build successfully using workspace links.
3. **Given** the repository is already installed, **When** the documented test command is run at the root, **Then** tests execute across workspaces and report aggregated results.

---

### User Story 2 - CI uses deterministic, workspace-aware installs and tasks (Priority: P2)

The CI pipeline installs and runs tasks once at the root, using the workspace graph for caching and parallelization. Re-running install without changes produces no diffs.

**Why this priority**: Ensures reproducibility and reduces CI time and flakiness.

**Independent Test**: CI job runs install + a representative task from the root; a subsequent install step produces no dependency changes.

**Acceptance Scenarios**:

1. **Given** a CI runner with a clean workspace, **When** the documented install step runs at the root, **Then** the lockfile is respected and no per‑package installs occur.
2. **Given** a subsequent CI step on the same commit, **When** install is re‑run, **Then** zero packages are added/removed/updated and the step completes faster due to caching.

---

### User Story 3 - Internal modules resolve via catalogs/aliases (Priority: P3)

Developers can reference internal packages using stable aliases defined in a central catalog. Apps and packages resolve these aliases consistently without relative path imports.

**Why this priority**: Improves ergonomics and consistency, reducing path maintenance and mistakes.

**Independent Test**: Introduce or update an import in one app/package to use a catalog alias; the project builds and runs tests without additional configuration.

**Acceptance Scenarios**:

1. **Given** a shared package with a public API, **When** an app imports it via a catalog alias, **Then** the import resolves to the local workspace version.
2. **Given** two internal packages that depend on a shared library, **When** both import via the same alias, **Then** both resolve to the same version and no duplicate copies are installed.

---

### Edge Cases

- Mixed environments: a contributor has legacy package-manager lockfiles present; running the documented install must ignore or gracefully warn about conflicting lockfiles.
- Cross‑platform: developers on macOS and Linux can install and run tasks with the same root commands; Windows developer support is out of scope for this phase.
- External versions: the catalog governs internal aliases only; version conflicts for third‑party dependencies are handled by the standard workspace resolution policy and documented where relevant.
- Incremental adoption: a workspace temporarily not compatible with the new flow is excluded without breaking the rest of the repo; re‑inclusion process is documented.

## Quality & Non-Functional Standards *(mandatory)*

- **Code Quality**: Consistent linting, formatting, and type‑checking across all workspaces. Shared code is reused instead of duplicated.
- **Testing Strategy**: Automated unit and integration tests run from the repository root and can be executed locally and in CI with a single command. A smoke test verifies cross‑workspace linking.
- **Documentation**: Root README includes setup, common commands, workspace structure, and troubleshooting. Each app/package lists its primary tasks and how they roll up to the root.
- **Developer Experience**: A new contributor can install and run at least one app and one shared package within minutes using only the documented root flow.
- **Determinism**: Re‑running install on the same commit produces no changes. Lockfiles are committed and reviewed.
- **Security/Compliance**: Third‑party dependency sources and license policies remain unchanged; new workflow does not bypass existing checks.

## Requirements *(mandatory)*

### Functional Requirements

- **FR-001**: Provide a single repository‑level workspace configuration that discovers all existing apps and packages as a coherent dependency graph.
- **FR-002**: Define a central catalog of internal package aliases only (no third‑party pins), so apps/packages import shared modules via stable aliases.
- **FR-003**: From the repository root, contributors can execute standard tasks (install, develop, build, test) that operate across workspaces without per‑package setup.
- **FR-004**: Fully migrate "colony" to the new workflow; scripts/tooling may change to align with the root process. Preserve or improve user‑visible behavior, and document any command changes.
- **FR-005**: Ensure deterministic installs: repeating the install on the same commit yields zero dependency changes; lockfiles are the single source of truth.
- **FR-006**: Document the migration and daily‑use workflow, including how to add a new workspace and how to add a new catalog alias.
- **FR-007**: Avoid conflicting package‑manager metadata: remove or neutralize obsolete workspace/lockfile configurations so the new workflow is authoritative.
- **FR-008**: Internal package dependencies default to resolving to the local workspace version; consuming apps do not fetch duplicate copies.

### Assumptions & Dependencies

- The repository will standardize on a single workspace workflow at the root.
- Existing CI can run shell steps at the repo root; no new external services are required.
- Some packages may require minor script adjustments to integrate with the new root workflow, except for "colony" per FR‑004.

<!-- No data entities applicable for this infrastructure change. -->

## Success Criteria *(mandatory)*

### Measurable Outcomes

- **SC-001**: A new contributor completes clone → install → run a sample task in ≤ 5 minutes on a typical laptop.
- **SC-002**: Re‑running install on the same commit results in 0 file changes and completes in ≤ 30 seconds with warm cache.
- **SC-003**: 100% of intended apps/packages are recognized as part of the workspace graph and can be built from the root.
- **SC-004**: "Colony" can be built and started for development via the root workflow in ≤ 3 minutes on a typical laptop; any script/tooling changes are documented, and the primary developer task completes successfully.
