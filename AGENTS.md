# waddle Development Guidelines

- use jj when .jj exists, git otherwise
- Before implementation work begins, create a branch and open a draft PR whose
  title summarizes the intended work and whose description contains the plan.
  Make this the first action for all non-trivial tasks unless explicitly told
  not to.
- Always review your plan against the XEPS in ./xeps
    - If ./xeps doesn't exist, clone xsf/xeps into ./xeps
- Divide work up, use sub-agents and role specific agents to achieve our task.
- After you are finished, use multiple adverserial personas subagents to review our changes; repeat until they find no real and actionable issues.
- Our job doesn't stop after we push, we always monitor CI and fix it until all checks are green
- XMPP Native, Never use out of band non-XMPP APIs.
- XEP conformance hard rule:
  - Prefer conformant XEP/XMPP shapes at all times. Use custom `urn:waddle:*`
    namespaces only when no suitable XEP-defined shape exists.
  - If Waddle advertises an official XEP feature or uses an official
    `urn:xmpp:*`, `jabber:*`, or `http://jabber.org/*` namespace, the wire
    shape and behavior MUST conform to that XEP exactly.
  - Do not use official XEP namespaces for Waddle-specific semantics.

## Active Technologies

- TypeScript 5.8.x, Bun 1.3.x + Astro 5.x, `@astrojs/vue` 5.x, Vue 3.5.x, Tailwind CSS 4.x with `@tailwindcss/vite`, `@astrojs/cloudflare` 12.x (Pages adapter), `@ark-ui/vue` lates (001-waddle-chat-clickdummy)
- N/A (clickdummy; local mock state only) (001-waddle-chat-clickdummy)
- TypeScript 5.8 (Bun toolchain) + Projen `WaddleDataService` generator, Drizzle ORM, Pothos GraphQL + federation plugins, GraphQL Yoga, Cloudflare D1 (001-setup-topics-service)

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

- Knip hard rule (chat/):
  - `bun run lint` in `chat/` runs `knip` and must exit clean on every PR.
  - A PR that introduces an unused file, export, or dependency MUST either
    wire it up or remove it; do not silence findings via broad `ignore`
    entries in `knip.json`. Targeted entry/ignoreDependencies additions are
    acceptable only with a one-line justification in the PR description.
  - CI runs `knip` via `cuenv` (task `lint` in `chat/env.cue`) before the
    Cloudflare preview upload on every pull request.

## Code Style

TypeScript 5.8.x; Bun 1.3.x: Follow standard conventions

## Commit Messages

- Use Conventional Commits with a single scope: `fix(chat): ...`, `fix(server): ...`, `feat(apple/ui): ...`.
- Do not use unscoped subjects. If a change spans multiple areas, scope it to the dominant subsystem/user-facing surface and keep the subject lowercase after the colon.

## Recent Changes

- 001-setup-topics-service: Added TypeScript 5.8 (Bun toolchain) + Projen `WaddleDataService` generator, Drizzle ORM, Pothos GraphQL + federation plugins, GraphQL Yoga, Cloudflare D1
- 002-waddle-chat-clickdummy: Added TypeScript 5.8.x, Bun 1.3.x + Astro 5.x, `@astrojs/vue` 5.x, Vue 3.5.x, Tailwind CSS 4.x with `@tailwindcss/vite`, `@astrojs/cloudflare` 12.x (Pages adapter), `@ark-ui/vue` lates

- XML generation hard rule:
  - Never construct XML with `format!`, string concatenation, or `println!`.
  - Always build XMPP/XML payloads using Rust structs/builders (`xmpp_parsers`, `minidom::Element`, etc.) and serialize them.

- Typed-payloads hard rule:
  - Protocol data MUST be modelled with typed Rust values — never `String`, `&str`, or `Vec<u8>` blobs — at every boundary: event enums, handler traits, actor messages, dispatcher entries, callback envelopes, storage writes, routing effects, and public return types.
  - Stanzas use `crate::connection::Stanza` / `xmpp_parsers::{Iq, Message, Presence}`; arbitrary XML uses `minidom::Element`; JIDs use `jid::{Jid, BareJid, FullJid}`; namespaces and XEP identifiers use dedicated enums or `&'static str` constants from the `xep::*` modules — never ad-hoc string literals at call sites.
  - Serialization to `String` / `Vec<u8>` happens only at the I/O boundary (the transport adapter writing bytes to a socket). Any event that is not the literal write-to-wire effect carries typed values.
  - Parsing untyped input (raw frames from the transport, rows from storage) into typed values happens exactly once, as early as possible, and the untyped form is dropped immediately — typed values flow through the rest of the system.
  - Error results are typed (`thiserror` enums or typed stanza-error structs), never stringly-typed diagnostics masquerading as payloads. `String` is acceptable only for human-facing log messages emitted via the `Log` outbound event.
  - A PR that introduces a new `String`/`&str` field on an event, message, trait method, or public struct to carry structured data MUST be rejected; convert to a typed value or add a typed enum variant instead.

- XEP custom test-suite hard rule:
  - Every implemented XEP (including advertised compatibility/profile support) MUST have a dedicated Rust custom test suite.
  - Any PR that adds or expands XEP behavior MUST add or update that XEP’s dedicated Rust tests in the same PR.
  - If a feature is advertised but lacks testable behavior, either implement behavior with tests or remove the advertisement.

- Breaking changes by default: do not add backwards compatibility layers, migration shims, or legacy aliases unless explicitly requested.
- Assume no production servers/users/data for this project; prioritize clean design over compatibility.
- Keep the codebase clean: remove dead compatibility code immediately instead of preserving legacy paths.

