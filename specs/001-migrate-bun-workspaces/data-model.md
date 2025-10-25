# Data Model: Workspace & Catalog (Documentation Contracts)

**Branch**: `001-migrate-bun-workspaces`  
**Date**: October 25, 2025

This feature does not add product data models. It introduces configuration entities for development workflow.

## Entities

- Workspace
  - Fields: `name` (string), `globs` (string[]), `rootScripts` (map of string → string), `lockfilePolicy` (enum: `deterministic`)
  - Relationships: contains many `Package` entries discovered via `globs`.
  - Validation: running install at repo root produces no per‑package installs; subsequent install on same commit yields zero changes.

- Package
  - Fields: `name` (string), `path` (string), `private` (boolean), `scripts` (map), `deps` (map)
  - Relationships: member of `Workspace`.
  - Validation: internal deps that are part of the workspace resolve to local versions (no duplicates).

- CatalogAlias
  - Fields: `alias` (string), `target` (string; canonical internal package name), `policy` (enum: `internal-only`)
  - Relationships: alias maps to a `Package` within the `Workspace`.
  - Validation: imports using the alias resolve consistently across apps/packages.

## State Transitions

- Add Workspace Member
  - Pre: package.json has a unique `name` and is inside a matched `globs` path.
  - Action: run root install.
  - Post: package is linked into the workspace; imports via CatalogAlias resolve locally.

- Add Catalog Alias
  - Pre: target package exists in the workspace and exposes a public entrypoint.
  - Action: add alias entry to the central catalog and commit.
  - Post: imports using the alias resolve to the target package across all workspaces.

