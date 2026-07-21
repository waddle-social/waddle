# waddle Development Guidelines

- use jj when .jj exists, git otherwise
- Before implementation work begins, create a branch and open a draft PR whose
  title summarizes the intended work and whose description contains the plan.
  Make this the first action for all non-trivial tasks unless explicitly told
  not to.
- When creating or editing PR descriptions with `gh`, pass multiline Markdown
  with real newlines, never escaped `\n` sequences, and verify the rendered body
  with `gh pr view` before moving on.
- Always review your plan against the XEPS in ./xeps
    - If ./xeps doesn't exist, clone xsf/xeps into ./xeps
- Divide work up, use sub-agents and role specific agents to achieve our task.
- After you are finished, use multiple adverserial personas subagents to review our changes; repeat until they find no real and actionable issues.
- Always update the PR title and desc to desc the complete work done, with the plan; and remove draft status
- Our job doesn't stop after we push, we always monitor CI and fix it until all checks are green
- XMPP Native, Never use out of band non-XMPP APIs.
  - Exception: browser observability beacons to the configured Grafana Faro
    collector, plus W3C trace-context headers to configured backend origins
    (carried as a `traceparent` query parameter on the XMPP WebSocket
    upgrade URL, since the browser WebSocket API cannot set headers),
    are allowed as operational telemetry only. They must not carry XMPP
    control semantics, replace XMPP APIs, or alter XMPP wire behavior.
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

- Clippy hard rule (server/):
  - Local and CI clippy checks MUST run with `-D warnings`; warnings are
    errors.
  - Do not add `#[allow(...)]`, `#![allow(...)]`, or clippy-specific
    suppressions unless absolutely necessary and explicitly justified in the
    PR description.
  - Prefer deleting dead code, wiring unused code into the exercised path, or
    narrowing visibility over suppressing warnings.

## Code Style

- TypeScript 5.8.x; Bun 1.3.x: Follow standard conventions.
- Write single-purpose functions with explicit inputs and outputs; keep them
  small enough to review quickly and avoid multi-purpose helpers.
- Prefer many small, focused files over large modules; organize files around
  behavior, protocol boundary, or UI responsibility.
- Prefer functional patterns where they improve clarity: pure helpers,
  immutable data, composition, and local reasoning over hidden side effects.
- Avoid broad utility modules and implicit shared state. When functional style
  conflicts with local Rust, TypeScript, XMPP, or framework idioms, choose the
  clearest conformant implementation.

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
