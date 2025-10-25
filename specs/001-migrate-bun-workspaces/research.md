# Research: Bun Workspaces + Catalogs Migration

**Branch**: `001-migrate-bun-workspaces`  
**Date**: October 25, 2025  
**Context**: Migrate the repository to a single Bun workspace with a central catalog; fully migrate `colony`; Windows developer support out of scope; catalog covers internal aliases only.

---

## Tasks Dispatched

- Research workspace discovery for existing repo globs and root commands.
- Define catalog policy for internal packages and alias naming.
- Establish lockfile and deterministic install policy.
- Define CI root workflow for install/build/test using the workspace graph.
- Plan `colony` migration: preserve or improve behavior, document command changes.

---

## Findings & Decisions

### 1) Workspace discovery and root commands
- Decision: Use the existing `package.json` `workspaces` globs at the repo root to discover apps and shared packages; run install and tasks only from the root.
- Rationale: The repo already lists globs for `colony/*`, `waddle/*`, `huddle/*`, and `shared/packages/*`, enabling a single graph without per‑package installs.
- Alternatives considered: Per‑package installs (rejected: duplicates effort and risks drift); multiple workspace roots (rejected: adds complexity without value).

### 2) Catalog policy and alias naming
- Decision: Create a central catalog that enumerates internal packages using their canonical names (e.g., `@waddle/ui-web`, `@waddle/types`, `@waddle/core`, `@waddle/auth`). The catalog focuses on internal aliases only and does not pin third‑party packages.
- Rationale: Reinforces consistent import specifiers and keeps external dependency policy unchanged.
- Alternatives considered: Broad third‑party pinning in the catalog (rejected: governance overhead and out of scope); not using a catalog (rejected: less discoverability and consistency).

### 3) Deterministic installs and lockfile
- Decision: Lockfile is the single source of truth; commits include the textual lockfile; repeat installs on the same commit must report no changes.
- Rationale: Guarantees reproducibility locally and in CI.
- Alternatives considered: Allow per‑package updates during CI (rejected: non‑deterministic builds).

### 4) CI workflow
- Decision: CI runs `bun install` once at the repo root, then executes `bun run build` or targeted tasks. Cache based on lockfile and workspace metadata.
- Rationale: Minimizes install time and avoids per‑package duplication.
- Alternatives considered: Independent per‑package pipelines (rejected: slower, more fragile).

### 5) `colony` migration
- Decision: Fully migrate `colony` to the root workflow. Scripts/tooling may change; behavior must be preserved or improved. Document any command name changes at the package level and surface equivalent root aliases.
- Rationale: Aligns every app with the same developer experience and CI model.
- Alternatives considered: Adapters/shims to keep all old commands unchanged (rejected: added maintenance for minimal benefit).

### 6) Windows scope
- Decision: Windows developer support is out of scope for this phase. Document known differences only if discovered during review.
- Rationale: Focus scope and accelerate adoption for macOS/Linux.
- Alternatives considered: Full Windows parity (defer to a later phase).

---

## Unknowns Resolved

All clarifications from the spec have been resolved within this research: `colony` fully migrates; Windows is out of scope; the catalog covers internal aliases only.

*** End of research.md ***
