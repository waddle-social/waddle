# Quickstart: Bun Workspaces + Catalogs

**Branch**: `001-migrate-bun-workspaces`  
**Date**: October 25, 2025

## Prerequisites
- Bun 1.3.x installed
- macOS or Linux shell

## Install
- From repository root: `bun install`
- Re-run on the same commit: should report no changes

### Deterministic install check
- Run: `bun run verify:install`
- Expects: no lockfile changes

## Common Commands (root)
- Develop apps: `bun run dev` (starts key apps in parallel)
- Build everything: `bun run build`
- Test all workspaces: `bun test`
- Lint + format: `bun run lint` and `bun run format`
- Type-check: `bun run typecheck`

### Smoke build
- Run: `bun run smoke:build`
- Expects: shared packages and Colony build successfully

## Filtering per area (examples)
- Colony only: `bun run --filter "./colony/*" build`
- Shared packages only: `bun run --filter "./shared/packages/*" build`

## Add a new workspace package
- Create a package under `shared/packages/<name>` (or another globbed path)
- Give it a unique `name` in its `package.json`
- Run `bun install` at the root to link

## Catalog: internal aliases
- Define internal aliases in `bunfig.toml` under a central catalog section.
- Example snippet (replace with actual package names):

```
# bunfig.toml
[catalog]
"@waddle/ui-web" = "*"
"@waddle/types" = "*"
"@waddle/core" = "*"
"@waddle/auth" = "*"
```

- After editing, run `bun install` to refresh links if needed.

## Colony migration notes
- `colony` now participates fully in the root workflow; its scripts/tooling may change, but behavior remains the same or improves.
- Use root commands for local development and CI; package-level commands continue to work where defined.

## Using aliases in apps

1. Add internal packages to the app `package.json` using workspace protocol:

```
// example: waddle/website/package.json
{
  "devDependencies": {
    "@waddle/ui-web": "workspace:*",
    "@waddle/types": "workspace:*"
  }
}
```

2. Import using the alias in source files:

```
import { uiWebReady } from "@waddle/ui-web";
```

3. Build from the repo root. Imports resolve to local workspace versions.

## CI Workflow (Deterministic Install)

In `.github/workflows/deployment.yaml`, CI performs a workspace-aware install and verification:

1. Setup Bun (`oven-sh/setup-bun@v1`, bun-version: 1.3.x)
2. Cache `**/node_modules` and `~/.cache/bun` keyed by `bun.lock`
3. Run `bun install --frozen-lockfile` at the repo root
4. Run `bun run verify:install` to ensure lockfile stability
5. Proceed with build/deploy steps

This ensures re-runs on the same commit are deterministic and fast.
