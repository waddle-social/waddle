# PR: Migrate to Bun Workspaces + Catalogs (Keep Colony)

Branch: `001-migrate-bun-workspaces`
Spec: `specs/001-migrate-bun-workspaces/spec.md`
Plan: `specs/001-migrate-bun-workspaces/plan.md`
Tasks: `specs/001-migrate-bun-workspaces/tasks.md`

## Summary
- Move repo to a single Bun workspace with a central catalog while keeping Colony.
- Deliver deterministic installs and CI, root developer workflow, and internal aliasing via catalog.
- Windows dev support out of scope for this phase; catalog covers internal aliases only.

## Changes
- Workspace & Catalog
  - `bunfig.toml`: enable text lockfile; add `[catalog]` with internal aliases (`@waddle/{ui-web,types,core,auth}`).
  - Root `package.json`: engines.bun >= 1.3.0; scripts `verify:install`, `smoke:build`.
  - `.gitignore`: add `dist/`, `build/`, `coverage/`, `.turbo/`, `bun.lockb`.
- Shared Packages
  - `shared/packages/ui-web`: add minimal `src/index.ts`; add `build` script.
- Apps
  - `colony/website`: add `better-call` dependency; add devDeps `@waddle/{ui-web,types}` as `workspace:*`.
  - `waddle/website`: add devDeps `@waddle/{ui-web,types}` as `workspace:*`.
- CI
  - `.github/workflows/deployment.yaml`: setup Bun, cache deps by lockfile, `bun install --frozen-lockfile`, `bun run verify:install`.
- Docs
  - `README.md`: Workspace Quickstart.
  - `specs/.../quickstart.md`: deterministic install, smoke build, CI workflow, alias usage.
  - Full spec/plan/research/data‑model/contracts/tasks under `specs/001-migrate-bun-workspaces/`.

## Validation
- Deterministic installs: `bun run verify:install` → OK (no lockfile changes).
- Smoke build: `bun run smoke:build` → OK (shared + Colony build).
- CI prepared for deterministic installs on PRs and main branch.

## How to Test Locally
- `bun install`
- `bun run verify:install` (should pass with no changes)
- `bun run smoke:build` (should succeed)
- Optionally: `bun run build` and `bun test`

## Risks & Mitigations
- Build regressions in Colony due to dependency resolution → Addressed by adding `better-call` and verifying build.
- Alias adoption across apps → Catalog configured; guidance documented; no existing relative imports detected.

## Follow‑ups (Optional)
- Extend CI to run `smoke:build` on PRs.
- Add Windows developer support if needed in a future phase.
- Incrementally refactor imports to use catalog aliases where beneficial.

