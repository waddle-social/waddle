<!--
Sync Impact Report
Version change: 1.0.0 → 1.1.0
Modified principles:
- (new) V. Federated Read/Write Discipline
Added sections:
- None
Removed sections:
- None
Templates requiring updates:
- ✅ .specify/templates/plan-template.md
- ✅ .specify/templates/spec-template.md
- ✅ .specify/templates/tasks-template.md
Follow-up TODOs:
- None
-->

# Waddle Constitution

## Core Principles

### I. Intentional Code Quality

- Production TypeScript MUST compile with `strict` settings; new code MAY NOT introduce `any` or `unknown` escapes without a documented exception in the plan.
- Merge requests MUST pass repository lint, formatting, and type-check jobs with zero warnings before review is considered complete.
- Shared packages in `shared/packages/` MUST be preferred to duplicating UI or business logic; new packages require an Architecture Decision Record in `docs/adr/`.

*Rationale: Enforced quality gates keep the Cloudflare Workers codebase maintainable and prevent entropy across multiple packages.*

### II. Test-First Delivery

- Every behavioral change MUST land with automated tests that fail before implementation and cover unit, integration, and contract boundaries proportionate to risk.
- Continuous integration pipelines (`moon run test:*` or equivalent) MUST succeed before merge; flaky tests require quarantine, root-cause analysis, and fixes within the same sprint.
- Feature plans MUST specify how tests will be executed locally (for example, `bun test` or `moon run website:test`) and in CI prior to writing production code.

*Rationale: Treating tests as executable contracts preserves trust in deployments and keeps shared packages stable.*

### III. Unified User Experience

- Web surfaces MUST consume components, tokens, and styles from `shared/packages/ui-web` or documented successors; deviations demand UX sign-off.
- User-facing changes MUST document accessibility outcomes meeting WCAG 2.1 AA (landmark structure, focus order, and contrast) in the spec or tasks.
- Product copy changes MUST reference the approved tone-of-voice guide in `docs/prd/` and update localization resources alongside UI updates.

*Rationale: Consistent interfaces, accessibility, and tone preserve the Waddle brand and reduce rework across properties.*

### IV. Performance Budget Ownership

- Each new route, component, or worker MUST declare and meet performance budgets: p95 server response <= 150 ms on Cloudflare Workers and p75 Largest Contentful Paint <= 2.5 s on primary pages.
- Plans MUST describe instrumentation or monitoring (for example, Cloudflare Analytics or Web Vitals logging) used to validate budgets before and after release.
- Regressions beyond 10 percent of the agreed budget require rollback or a corrective follow-up task with an owner before release notes are published.

*Rationale: Explicit budgets keep the experience fast on resource-constrained devices and maintain platform efficiency at scale.*

### V. Federated Read/Write Discipline

- All read-facing resolvers MUST be exposed through the GraphQL federation gateway; teams MAY NOT introduce direct service reads that bypass federation.
- Read services MUST honor CQRS boundaries: resolvers stay side-effect free and persist no state beyond caches declared in the plan; write logic lives in Cloudflare Workflows or `capnweb`.
- Write operations MUST orchestrate via Cloudflare Workflows or `capnweb` actions; direct datastore writes from read services require an approved architecture RFC and migration plan.
- Feature plans MUST include contract or integration tests that validate federation schema composition and workflow invocations before merge.

*Rationale: Enforcing CQRS with federated reads and workflow-driven writes preserves service boundaries, operational resilience, and auditability.*

## Delivery Quality Standards

- Feature specs MUST enumerate code-quality, testing, UX, and performance acceptance criteria and reference how compliance will be verified.
- Implementation plans MUST list linting, type-check, accessibility, performance, and CQRS/federation validation tasks before feature work begins.
- Architecture sections in specs MUST map read paths through GraphQL federation and write paths through Cloudflare Workflows or `capnweb`, including failure handling.
- Release notes and docs updates in `docs/` MUST be included when behavior, UX, or performance expectations change.

## Delivery Workflow & Reviews

- Constitution Check in plans MUST confirm all five principles are satisfied, highlighting mitigations when temporary exceptions are required.
- Pull requests MUST link to plan and spec artifacts and record outcomes of linting, testing, accessibility, performance, and CQRS/federation checks in review comments.
- Quarterly governance reviews MUST sample recent releases to audit adherence and publish findings in `docs/adr/` or `docs/rfc/`.

## Governance

- Amendments require an RFC in `docs/rfc/` with maintainer approval from at least two code owners and confirmation that dependent templates are updated.
- Semantic versioning applies to this constitution: MAJOR for principle removals or incompatible rewrites, MINOR for new principles or enforcement scope, PATCH for clarifications.
- Compliance reviews occur monthly within the engineering sync; unresolved violations gain owners and due dates tracked in project management tooling.

**Version**: 1.1.0 | **Ratified**: 2025-10-25 | **Last Amended**: 2025-10-25
