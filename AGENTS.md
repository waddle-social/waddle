# waddle Development Guidelines

Auto-generated from all feature plans. Last updated: 2025-10-25

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

npm test && npm run lint

## Code Style

TypeScript 5.8.x; Bun 1.3.x: Follow standard conventions

## Recent Changes

- 001-setup-topics-service: Added TypeScript 5.8 (Bun toolchain) + Projen `WaddleDataService` generator, Drizzle ORM, Pothos GraphQL + federation plugins, GraphQL Yoga, Cloudflare D1
- 002-waddle-chat-clickdummy: Added TypeScript 5.8.x, Bun 1.3.x + Astro 5.x, `@astrojs/vue` 5.x, Vue 3.5.x, Tailwind CSS 4.x with `@tailwindcss/vite`, `@astrojs/cloudflare` 12.x (Pages adapter), `@ark-ui/vue` lates

- XML generation hard rule:
  - Never construct XML with `format!`, string concatenation, or `println!`.
  - Always build XMPP/XML payloads using Rust structs/builders (`xmpp_parsers`, `minidom::Element`, etc.) and serialize them.
