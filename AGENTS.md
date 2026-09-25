# Waddle Agent Guidance

## Scope and delivery

- Use `jj` when `.jj` exists; use Git otherwise. Check the working-copy status first and preserve unrelated changes. Isolate work when that helps protect existing edits.
- Work within the user's requested scope. Complete the requested work; report adjacent issues as follow-ups unless they block the requested result. Prefer targeted edits over broad rewrites when a focused change will do.
- Before non-trivial implementation work, create a branch and open a draft PR with a plan in its description, unless the user directs otherwise. Read-only investigation, planning, and review do not need a branch or PR.
- When creating or editing a PR with `gh`, use real newlines in Markdown bodies and verify the rendered title and description with `gh pr view`.
- Once implementation and required checks are complete, update the PR title and description to summarize the finished work and plan, then mark the draft ready. After pushing, monitor CI and resolve failures caused by the change until all checks are green.

## XMPP and XEP requirements

- Waddle is XMPP-native. Do not use out-of-band APIs for application or protocol semantics.
  - Operational telemetry only: browser beacons may go to the configured Grafana Faro collector. W3C trace context may go to configured backend origins as a `traceparent` query parameter on the XMPP WebSocket upgrade URL, since the browser WebSocket API cannot set headers.
  - Telemetry must not carry XMPP control semantics, replace XMPP APIs, or alter XMPP wire behavior.
- For work that changes XMPP wire behavior, feature advertisements, or protocol design, review the relevant specification in `./xeps/xep-NNNN.xml`. If the checkout or relevant specification is absent, obtain the official source from `xsf/xeps`. Use `docs/xep-conformance-audit.md` and `server/capabilities.toml` when their coverage applies.
- Prefer a suitable XEP-defined shape. If Waddle advertises an XEP feature or uses an official `urn:xmpp:*`, `jabber:*`, or `http://jabber.org/*` namespace, its wire shape and behavior MUST conform to that specification. Waddle-specific semantics must not use official XEP namespaces; use `urn:waddle:*` only when no suitable XEP shape exists.
- Rust protocol typing, XML construction, and XEP test requirements are in `server/AGENTS.md`.

## Coordination and review

- Match coordination to the task. Delegate bounded, independent work when it improves coverage or speed; keep one agent responsible for integration. Keep small tasks direct.
- For material design or risk changes, use an independent architecture review before implementation and when a material assumption changes. Ask the reviewer to challenge boundaries, contracts, data flow, security, concurrency, integration, and verification.
- After integration, consider a read-only adversarial review for material changes. Report only in-scope, actionable findings with a concrete failure path and evidence. Omit style preferences, speculation, duplicates, and already-covered behavior. Resolve actionable findings and stop when the relevant review lenses support `CLEAN`.
- Choose verification based on the change and match each claim to its evidence. A formatter, parser, test, build, runtime check, and live result prove different things; avoid unrelated repeated checks and report material limits.

## Workspace conventions

- This is a multi-workspace repository. Use the relevant workspace's manifest, documentation, and scripts; the root `package.json` defines workspaces but has no common test or lint scripts.
- For JavaScript and TypeScript work, use Bun and `bunx`; do not use npm, Yarn, or pnpm for installs, scripts, or CI.
- `server/AGENTS.md` contains Rust/XMPP server guidance. `chat/AGENTS.md` contains chat-specific lint and Knip requirements. `docs/rust-toolchain-bump.md` is the procedure for Rust toolchain updates.
- Use Conventional Commits with one scope, for example `fix(chat): ...`, `fix(server): ...`, or `feat(apple/ui): ...`. Use a lowercase subject after the colon.
- Assume there are no production servers or user data unless the task establishes otherwise; favor clean design over compatibility. Prefer breaking changes over compatibility shims unless compatibility is requested. Before changing external contracts or persisted data, inspect current callers and storage behavior.
