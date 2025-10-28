# Research: Waddle Chat Clickdummy (Astro + Vue + Cloudflare + Ark UI + Tailwind + Bun)

Date: 2025-10-27
Branch: 001-waddle-chat-clickdummy
Source Input: /Users/icepuma/Downloads/waddle-chat (Next.js app directory)

This document captures decisions and rationale to transform the downloaded Next.js project into an Astro + Vue clickdummy deployable to Cloudflare Pages. All prior NEEDS CLARIFICATION items are resolved via the assumptions and choices below.

## Decisions

1) Framework and runtime
- Decision: Use Astro 5 with the Vue 3 integration (`@astrojs/vue`) and output: `static` for Cloudflare Pages.
- Rationale: Astro’s islands architecture provides small client bundles; clickdummy does not require SSR or a backend. Cloudflare Pages supports static output natively.
- Alternatives: SSR on Workers via `@astrojs/cloudflare` server adapter was considered but rejected for clickdummy scope.

2) UI component strategy
- Decision: Use `@ark-ui/vue` for accessible, headless UI primitives and Tailwind CSS 4 for styling.
- Rationale: Ark UI offers composable interactions; Tailwind speeds layout/theming. Both integrate cleanly with Vue SFCs under Astro.
- Alternatives: Headless UI Vue or plain Tailwind components; kept Ark UI to align with request and accessibility goals.

3) Styling
- Decision: Tailwind CSS 4.x with `@tailwindcss/vite` plugin; global tokens in `src/styles/global.css`.
- Rationale: Tailwind 4 uses the Vite plugin pipeline and simplifies configuration. Matches existing repo direction.
- Alternatives: Tailwind 3.x; kept 4.x per “latest deps” requirement.

4) Package manager and scripts
- Decision: Bun 1.3.x exclusively; keep text lockfile (`bun.lock`). Root scripts will filter to `./waddle/*` workspaces.
- Rationale: Consistent with monorepo and “always use bun”.
- Alternatives: pnpm (present in downloaded source) rejected; we will not import `pnpm-lock.yaml`.

5) Cloudflare integration
- Decision: Target Cloudflare Pages static hosting; include `@astrojs/cloudflare` to ensure CF compatibility, but stay `output: 'static'`.
- Rationale: No server code needed; preserves deploy path to Pages. We will not add Durable Objects/WebSockets for this phase.
- Alternatives: Workers + DO for real-time chat; postponed to later feature.

6) Routing and pages
- Decision: Convert the Next.js app directory structure into Astro pages and Vue components:
  - Next `/app/*` → Astro `src/pages/*` and Vue components in `src/components/*`.
  - Preserve `public/` assets.
- Rationale: Aligns with Astro conventions and enables island hydration where needed.
- Alternatives: Keep React and use `@astrojs/react`; declined per “Vuejs” requirement.

7) State and data
- Decision: Use local mock state (TypeScript models) to simulate login and chat interactions; optionally seed from JSON under `public/mock`.
- Rationale: Clickdummy requirement; avoids backend complexity.
- Alternatives: LocalStorage for persistence (optional); not required for first pass.

8) Accessibility
- Decision: Use Ark UI patterns, ensure focus outlines, keyboard navigation, and color contrast; run an automated axe check in CI smoke.
- Rationale: Satisfy WCAG 2.1 AA basics.
- Alternatives: Manual-only review; automated checks are low-cost.

9) Testing
- Decision: Minimal smoke tests with Vitest: Astro build succeeds; main pages render and contain expected headings/elements.
- Rationale: Guardrails for “working UI” state.
- Alternatives: Cypress e2e deferred due to clickdummy scope.

10) Directory placement
- Decision: Implement the app at `/Users/icepuma/development/waddle/waddle/app` to match existing workspace filters.
- Rationale: Root `package.json` uses `waddle/**` workspace filters; this path integrates cleanly.
- Alternatives: Create a new top-level `apps/` folder; rejected to avoid workspace changes.

## Unknowns resolved by assumption

- Branding and exact visual spec: Assume the downloaded `public/` assets and styles reflect desired look-and-feel; we will adapt where necessary to Ark UI patterns.
- Exact component list from Ark UI: We will use primitives for buttons, inputs, dialog/sheet if needed; deeper library choices can be extended later.
- Authentication semantics: Simulated username-only flow; no real auth.
- Real-time chat: Simulated in-memory; no network.

## Migration outline from Next.js to Astro + Vue

1. Create new Astro project under `waddle/app` with Vue, Tailwind, Cloudflare adapter; enable TypeScript `strict`.
2. Copy assets from `/Users/icepuma/Downloads/waddle-chat/public` to `waddle/app/public`.
3. Translate presentational React components to Vue SFCs; map Next.js routes to `src/pages/*`.
4. Implement local mock stores for user and messages; seed with sample data.
5. Compose screens using Ark UI components and Tailwind classes.
6. Add smoke tests (build/render) and documentation.

