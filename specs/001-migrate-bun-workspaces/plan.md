# Implementation Plan: Migrate Repo to Bun Workspaces with Catalogs (Keep Colony)

**Branch**: `001-migrate-bun-workspaces` | **Date**: October 25, 2025 | **Spec**: /Users/icepuma/development/waddle/specs/001-migrate-bun-workspaces/spec.md
**Input**: Feature specification from `/specs/001-migrate-bun-workspaces/spec.md`

**Note**: This plan is produced by `/speckit.plan` following the repository’s constitution and templates.

## Summary

Move the repository to a single Bun workspace with a centralized catalog, while fully migrating the `colony` app to the new workflow. Windows developer support is out of scope for this phase. The catalog covers internal aliases only. The plan standardizes root‑level install/build/test commands, ensures deterministic installs via the lockfile, and documents how to add workspaces and aliases.

## Technical Context

**Language/Version**: TypeScript 5.8.x; Bun 1.3.x  
**Primary Dependencies**: Astro 5, Vue 3, Tailwind, TypeScript, Biome; Cloudflare Workers toolchain for apps that deploy there  
**Storage**: Existing app storage remains unchanged (e.g., Cloudflare D1 used by `colony`); no new storage introduced by this feature  
**Testing**: `bun test` at repo root; type‑checking via `tsc --noEmit` and `vue-tsc` where relevant; linting via `biome`  
**Target Platform**: macOS/Linux developer environments; Cloudflare Workers and static hosting for deployments  
**Project Type**: Monorepo with multiple apps (`colony`, `waddle`, `huddle`) and shared packages under `shared/packages`  
**Performance Goals**: No user‑visible performance regressions; adhere to existing budgets (p95 server <= 150 ms; p75 LCP <= 2.5 s)  
**Constraints**: Deterministic root‑level install; Windows developer support out of scope; catalog covers internal aliases only; `colony` fully migrates to the new workflow  
**Scale/Scope**: Multiple apps and shared packages; all must participate in the single workspace install/build/test flow

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

- **Code Quality**: Gate on `bun run lint`, `bun run format --check` (or CI equivalent), and `bun run typecheck` at repo root. Prefer reuse of `shared/packages/*` over adding new packages. New shared packages require an ADR in `docs/adr/`.
- **Testing**: Fail‑first tests accompany changes to scripts/config. Run `bun test` locally and in CI at the repo root. Add a smoke test that builds one app and one shared package using workspace links.
- **User Experience**: No UI changes expected; this feature does not add or modify web surfaces. If any UI scripts change inside apps, those apps must continue using `shared/packages/ui-web` components as before.
- **Performance**: No production runtime changes expected. Existing budgets (p95 server <= 150 ms; p75 LCP <= 2.5 s) remain in force; no new instrumentation required for this change.
- **Exceptions**: None anticipated.

## Project Structure

### Documentation (this feature)

```text
specs/001-migrate-bun-workspaces/
├── plan.md
├── research.md
├── data-model.md
├── quickstart.md
└── contracts/
```

### Source Code (repository root)

```text
colony/
└── website/

waddle/
├── app/
└── website/

huddle/
├── services/
└── website/

shared/
└── packages/
    ├── auth/
    ├── core/
    ├── huddle-core/
    ├── types/
    └── ui-web/

.specify/
docs/
```

**Structure Decision**: Monorepo with apps in `colony`, `waddle`, and `huddle`, and shared libraries in `shared/packages`. A single workspace defined at the repo root discovers these projects via existing globs. A central catalog enumerates internal package aliases.

## Complexity Tracking

> **Fill ONLY if Constitution Check has violations that must be justified**

| Violation | Why Needed | Simpler Alternative Rejected Because |
|-----------|------------|-------------------------------------|
|           |            |                                     |

---

Post‑design Constitution Check: PASS (no exceptions).
