# waddle Development Guidelines

## Active Technologies

- TypeScript 5.8.x, Bun 1.3.x + Astro 5.x, `@astrojs/vue` 5.x, Vue 3.5.x, Tailwind CSS 4.x with `@tailwindcss/vite`, `@astrojs/cloudflare` 12.x (Pages adapter), `@ark-ui/vue` lates (001-waddle-chat-clickdummy)
- N/A (clickdummy; local mock state only) (001-waddle-chat-clickdummy)
- TypeScript 5.8 (Bun toolchain) + Projen `WaddleDataService` generator, Drizzle ORM, Pothos GraphQL + federation plugins, GraphQL Yoga, Cloudflare D1 (001-setup-topics-service)

- TypeScript 5.8.x; Bun 1.3.x + Astro 5, Vue 3, Tailwind, TypeScript, Biome; Cloudflare Workers toolchain for apps that deploy there (001-migrate-bun-workspaces)

## Project Structure

```text
src/
tests/
```

## Commands

bun test && bun run lint

- Bun-only hard rule:
  - Never use `npm`, `yarn`, or `pnpm` for installs, scripts, or CI.
  - Use `bun` and `bunx` exclusively.

## Code Style

TypeScript 5.8.x; Bun 1.3.x: Follow standard conventions

## Recent Changes

- 001-setup-topics-service: Added TypeScript 5.8 (Bun toolchain) + Projen `WaddleDataService` generator, Drizzle ORM, Pothos GraphQL + federation plugins, GraphQL Yoga, Cloudflare D1
- 002-waddle-chat-clickdummy: Added TypeScript 5.8.x, Bun 1.3.x + Astro 5.x, `@astrojs/vue` 5.x, Vue 3.5.x, Tailwind CSS 4.x with `@tailwindcss/vite`, `@astrojs/cloudflare` 12.x (Pages adapter), `@ark-ui/vue` lates

- XML generation hard rule:
  - Never construct XML with `format!`, string concatenation, or `println!`.
  - Always build XMPP/XML payloads using Rust structs/builders (`xmpp_parsers`, `minidom::Element`, etc.) and serialize them.

- XEP custom test-suite hard rule:
  - Every implemented XEP (including advertised compatibility/profile support) MUST have a dedicated Rust custom test suite.
  - Any PR that adds or expands XEP behavior MUST add or update that XEP’s dedicated Rust tests in the same PR.
  - If a feature is advertised but lacks testable behavior, either implement behavior with tests or remove the advertisement.

<!-- MANUAL ADDITIONS START -->
- Breaking changes by default: do not add backwards compatibility layers, migration shims, or legacy aliases unless explicitly requested.
- Assume no production servers/users/data for this project; prioritize clean design over compatibility.
- Keep the codebase clean: remove dead compatibility code immediately instead of preserving legacy paths.
<!-- MANUAL ADDITIONS END -->
