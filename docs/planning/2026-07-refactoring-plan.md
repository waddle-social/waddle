# Waddle Monorepo Refactoring Plan

## Context

The user asked for a codebase-wide review and a refactoring plan improving quality on all axes. Three parallel explorations (Rust server, chat client, rest of monorepo) plus manual verification found a codebase with sound architecture (sans-io typed-event server core, shared Rust XMPP client consumed via WASM/UniFFI by web and Apple clients) but with concentrated debt:

- **Server** (`server/`, 9 crates, ~364k LOC): a ~20k-line XEP-0357 push/notification god-module cluster; one genuine violation of the banned `format!`-XML rule (`waddle-server/src/auth/profile.rs:122-153`); 46 `#[allow(...)]` suppressions (29 `dead_code`) contradicting the no-allows rule; grab-bag modules (`iq/pubsub_helpers.rs` 2,618 LOC untested, `iq/misc.rs`, duplicate `parser_utils`); 22 XEP modules lacking the mandated dedicated test suites; and a documented half-finished delivery migration (`OutboundEvent::RouteToConnection` still on legacy `SendDirect`, issue #229).
- **Chat** (`chat/`, ~84k LOC src): `src/lib/xmpp/client.ts` is a 4,083-line god class with a hand-rolled ~50-field handler bus and 29 `any`s at the WASM boundary; `src/shell/chat-app-controller.ts` is 2,944 lines and untested; `src/channels/` and `src/dms/` are file-for-file parallel pipelines re-implementing the same XEP merge semantics (0424/0308/0444/0333/self-echo) twice; eight 1,000+-line SFCs without tests; duplicated `escapeXml`; 48 ad-hoc `.split("@")` JID parses; dead `connectionStore.api` field; no `test` script in package.json.
- **Apple** (`apps/apple/`): `AppModel.swift` is a 2,216-line `@MainActor` god model; `ChatWorkspaceView.swift` 1,508 lines; `XMPPTypes.swift` hand-rolls a JID parser duplicating the shared Rust FFI core; CI is PR-only (no main-branch build).
- **Repo-wide**: design tokens hand-maintained in 3 places (chat `tokens.css`, website `brand.css`, Swift `WaddleTheme.swift`); `bench-storage-backends/` is an unwired workspace with a stub crate and an untested compat claim; stale `server/TODO.md` (claims a XEP-0448 suite that doesn't exist); root cruft (`.impeccable.md`, `.rules.cue`, `.omx/plans/…`, `CONTEXT.md` — verify references before deleting).

**User decisions (confirmed):** delete `bench-storage-backends/`; include the RouteToConnection migration as the final server phase; unify design tokens via a simple codegen script (no style-dictionary).

**Ordering principle:** mechanical hygiene → test-debt paydown (doubles as characterization coverage) → structural decomposition → behavioral migration. Every phase is a series of small green-CI PRs (`bun test && bun run lint` for chat; `cargo clippy --all-targets -- -D warnings && cargo test` for server). Breaking changes are fine per project rules; dead code is deleted, never suppressed; no shims.

Four workstreams run in parallel; phases within a workstream are sequential.

---

## Phase 0 — Repo-wide hygiene & guardrails (1–2 small PRs)

1. Delete root cruft after `git grep`/`git log` confirms nothing references it. Outcome of those checks: `.omx/plans/issue-875-live-reaction-push.md` was a stale TDD plan for an already-landed feature — deleted. **`.impeccable.md` stays** — it carries live design direction (brand personality, aesthetic principles), not cruft. **`CONTEXT.md` stays** — the glossary is referenced by `docs/adr/010-presence-statuses.md`. **Keep `.rules.cue`** — it is the cuenv `rules.#DirectoryRules` config that generates the repo's `.gitignore`; it is live infrastructure, and the plan should use it *more*: any generated artifacts introduced by later phases (e.g. Phase T token outputs if they end up ignored, or new generated schema/test fixtures) get their ignore entries added through `.rules.cue`, never by hand-editing `.gitignore`.
2. **Delete `bench-storage-backends/`** entirely (user-confirmed). Do not wire it into CI.
3. Fix `server/TODO.md`: remove the false XEP-0448-suite claim; preferably convert remaining entries to issues and delete the file.
4. Add a `"test": "bun test"` script to `chat/package.json`.
5. Rename `server/crates/waddle-xmpp/tests/xep_0272_muji.rs` → `xep0272_*.rs` to match the convention of the other 61 files.
6. Add a default-branch build job for the Apple app. Note: unlike the chat/colony/website/server workflows, `waddle-apple-pullrequest.yml` is hand-written (no cuenv header, no `apps/apple/env.cue`), so the fix is a hand-written sibling `waddle-apple-default.yml` (push-to-main trigger, same build steps). Migrating apple CI under cuenv is a possible follow-up, not Phase 0.

Commits: `chore(repo): …`, `chore(chat): add test script`, `test(xmpp): rename muji suite`, `ci(apple): build on default branch`.

---

## Workstream S: Server (Rust)

### S1 — Mechanical hygiene (2–3 PRs, low risk)

1. **Fix the banned `format!`-XML site** `server/crates/waddle-server/src/auth/profile.rs:122-153`: replace string-built vcard-temp + hand-rolled `escape_xml` with typed xep0054 VCard builders from `waddle-xmpp` (add a builder there if only parse exists); return a typed value (`Element`/VCard struct), serialize only at the I/O boundary. Delete `escape_xml`.
2. **Eliminate all 46 `#[allow(...)]`**, grouped into 2–3 PRs by module cluster (`permissions/`, `messages/`+`vcard.rs`, `interpret/`+`auth/scram/`): delete `dead_code` items outright (29), delete unused imports (6), introduce focused parameter structs for the 4 `too_many_arguments`, fix the rest.
3. **De-duplicate `parser_utils`**: keep `waddle-xmpp-core/src/parser_utils.rs` (the shared leaf dep), migrate waddle-xmpp-only helpers into it or next to their single caller, delete the duplicate in `waddle-xmpp`.

Verify: `cargo clippy --all-targets -- -D warnings`; `git grep -n 'format!("<' server/crates` returns only test/parser hits; `cargo test --workspace`.

### S2 — XEP test-debt paydown (~5 PRs, low risk, high leverage)

Add dedicated suites under `server/crates/waddle-xmpp/tests/` (following the existing `xepNNNN_*.rs` pattern and `ws_common` harness) for the 22 inline-only XEPs: 0004, 0047, 0048, 0059, 0319, 0377, 0393, 0394, 0401, 0433, 0437, 0445, 0446, 0447, 0448, 0449, 0452, 0469, 0486, 0488, 0500, 0502. Batch by theme (~4–5 XEPs per PR): forms/paging (0004+0059), file sharing (0446–0449+0452), styling/markers (0393/0394/…), etc. Also add a ~50-line conformance test that globs `src/**/xepNNNN.rs` against `tests/xepNNNN_*.rs` and fails on missing suites — cheap recurrence prevention.

This satisfies the per-XEP-suite hard rule *and* provides characterization coverage for S3/S4.

Carried over from the deleted `server/TODO.md`: deepen the existing XEP-0292 (vCard4 PEP publish/retrieve), XEP-0402 (PEP bookmarks), and XEP-0115 (entity caps) integration suites alongside this phase; XEP-0047's missing IQ-session suite is already in the list above.

### S3 — God-module decomposition (3–4 PRs, medium risk)

De-risking pattern for each file: (a) move the 4–6k-line inline test blocks out to integration tests first (pure test-move PR, zero prod diff); (b) split prod code as move-mostly extractions; (c) extracted tests pass unchanged.

1. `waddle-server/src/notification_outbox.rs` (10,962; ~4.2k prod) → `notification_outbox/` module dir split along natural seams (enqueue/drain/dedupe/retry).
2. `waddle-server/src/push_service.rs` (6,605; ~4.2k prod) → `push_service/` dir (storage, payload building, provider dispatch, token lifecycle).
3. `notification_activity.rs` (1,710) / `notification_settings_projection.rs` (1,168): extract tests; split only if a clean seam exists — no splitting on line count alone.
4. Dissolve grab-bags: `iq/pubsub_helpers.rs` (write characterization tests first — currently untested), `iq/misc.rs`, `iq/session_misc.rs`, `interpret/room_helpers.rs`, `websocket/handlers/message.rs`. Relocate functions next to their single caller; genuinely shared remnants get named focused modules (e.g. `pubsub/node_config.rs`). No `misc.rs` survives.

Never mix a test-move and a prod-split in one PR.

### S4 — Finish the RouteToConnection migration (2–4 PRs, HIGH RISK, user-confirmed in scope)

Complete the migration documented at `waddle-xmpp/src/protocol/event/outbound.rs:37-74` (issue #229): implement the recipient-side pipeline (XEP-0191 blocking, 0280 carbons, 0313 MAM, 0359 stanza-ids, inbox projection) and delete legacy `SendDirect`.

1. Characterization first: integration tests pinning current recipient-observable behavior for both delivery paths (0191/0280/0313/0359 coverage partly arrives from S2).
2. Strangler order: build the recipient-side pipeline behind `RouteToConnection` (additive), flip producer sites one PR at a time in `interpret/`, delete the legacy path in the final PR (deletion last, no fallback kept per project rules).
3. Read issue #229 for prior design decisions before starting; close/update it in the final PR.

Depends on S2 (and S3 where interpret/ splits overlap).

---

## Workstream C: Chat (TypeScript/Vue)

### C1 — Hygiene & boundary typing (2–3 PRs, low risk)

1. Route all 48 ad-hoc `.split("@")` JID sites through `chat/src/lib/xmpp/jid.ts` helpers (`barePeerJid`, `jidDomain`, …); one hit is inside `client.ts:~3173` itself. Add a lint guard if feasible.
2. Dedupe `escapeXml`: keep `lib/xmpp/extension-commands/xml.ts`'s export, delete the private copy in `lib/xmpp/protocol-helpers.ts:49`.
3. Delete dead `connectionStore.api` (always-null) in `chat/src/lib/connection-store.ts` and its readers.
4. **Type the WASM boundary**: eliminate the 29 `any`s in `client.ts` by extending the existing wasm-types declarations — hard prerequisite for C3.

Verify: `bun test && bun run lint` (knip must stay clean — deletions only help).

### C2 — Unify channels/dms merge core (3–4 PRs, HIGH RISK, highest leverage)

`src/channels/` (4,450) and `src/dms/` (2,509) mirror each other: `messages.ts`, `chat-states.ts`, `live-merge.ts`, `mam-paging.ts`, `message-actions.ts`, `message-search.ts`, `message-timeline-state.ts`, `read-markers.ts`.

1. Characterization: shared table-driven fixtures run against BOTH pipelines to document behavior and intentional divergences before unifying (audit existing 66.5k LOC of tests for gaps).
2. Introduce a shared message shape (generic constrained interface satisfied by `LiveRoomMessage` and `LiveDmMessage`) with zero behavior change.
3. Strangler per concern into `chat/src/lib/messaging/` (small focused files mirroring the proven `extension-commands/` pattern): `retraction.ts` (0424), `correction.ts` (0308), `reactions.ts` (0444), `displayed.ts` (0333), `self-echo.ts`, `timeline-merge.ts`, then mam-paging. Each PR: extract, point both pipelines at it, delete both old copies.
4. Scope guard: unify pure merge/timeline logic ONLY — not UI, stores, or addressing (channels and DMs legitimately diverge there).

### C3 — Decompose BrowserXmppClient (4–6 PRs, HIGH RISK)

`chat/src/lib/xmpp/client.ts` (4,083 lines). Requires C1.4 (typed WASM boundary). Independent of C2.

1. First PR: replace the ~50 nullable handler fields + parallel hook arrays with one small typed event emitter (`lib/xmpp/client-events.ts` with a typed event map); migrate mechanically.
2. Strangler-fig by capability, one PR each: connection lifecycle (connect/resume/offline-queue, leaning on existing `resume-persistence.ts`/`reconnect-catchup.ts`), MAM paging, MUC admin, presence, pubsub, vCard. Client shrinks to a thin facade; decide at the end whether the facade dissolves.
3. Add direct unit tests for each extracted module as it lands.

### C4 — Shell controller & giant SFCs (medium risk, rolling backlog)

After C2/C3 first PRs land (they define the seams).

1. Split `chat/src/shell/chat-app-controller.ts` (2,944, ~40 watchers, untested) into per-feature composables under `shell/controllers/` (`useConnectionLifecycle`, `useRoomSync`, `useDmSync`, `useInboxSync`, `usePresenceSync`, notification orchestration), each with unit tests; parent becomes composition-only.
2. Thin the big SFCs one per PR, extracting logic into tested composables/child components. Priority: `MessageCard.vue` (1,536) and `MessageComposer.vue` (983) — they sit on the C2 seam — then `ContentArea.vue` (1,623), `HomeDashboard.vue` (1,358), `ChatReadyShell.vue` (1,152), the call dialogs, `ThreadPanel.vue`.
3. Split the `lib/chat-ui.ts` (735) grab-bag alongside whichever SFC PR touches it. Migrate the ~70 loose `src/lib/` root files opportunistically as touched — no big-bang move PR.

---

## Workstream A: Apple

### A1 — FFI JID + AppModel split (3–4 PRs, medium risk)

1. Highest value first: replace the hand-rolled JID parser in `apps/apple/Waddle/XMPP/XMPPTypes.swift` (454) with the UniFFI bindings from `server/crates/waddle-xmpp-client-ffi` (removes a correctness fork, not just style).
2. Decompose `apps/apple/Waddle/App/AppModel.swift` (2,216, 40+ methods) into feature models (`AuthModel`, `RoomsModel`, `DmsModel`, `InboxModel`, `PepModel`, `PushModel`, …) composed by a thin `@MainActor` shell; extract least-entangled domain first (PEP or upload), one per PR, with XCTest coverage for pure behavior.
3. Split `Chat/ChatWorkspaceView.swift` (1,508) into subviews after the model split.

Depends on Phase 0.6 (main-branch CI). Independent of other workstreams.

---

## Phase T — Design tokens (1 PR, low risk, schedule after chat churn settles; user-confirmed approach)

Single human-edited source (keep `chat/src/styles/global/tokens.css` or a small `tokens.json` beside it) + one script in `scripts/` regenerating `website/src/styles/global/brand.css` and `apps/apple/Waddle/Chat/WaddleTheme.swift` with "GENERATED — do not edit" headers, plus a CI `git diff --exit-code` regeneration check wired through cuenv (a task in `env.cue`, surfaced via `cuenv sync ci` like the other pipelines). If any generated output should be untracked, its ignore entry goes through `.rules.cue` (the cuenv `#DirectoryRules` source of the generated `.gitignore`). No style-dictionary or new dependencies.

---

## What NOT to do

- **No XEP trait/macro** for the 366-function `build_/is_/parse_` triad — it would obfuscate 74 currently grep-able modules; the S2 conformance test captures the recurrence-prevention value instead.
- **No channels/dms UI/store unification** — merge logic only.
- **No big-bang `chat/src/lib/` reorg PR** — opportunistic moves.
- **No interpreter/sans-io rewrite** — S4 finishes an existing migration; the architecture is sound.
- **No splitting on line count alone** — one responsibility per file is the target, not a size ceiling.

## Sequencing

```
Phase 0 ──┬─→ S1 → S2 → S3 → S4   (server, strict order)
          ├─→ C1 → {C2 ∥ C3} → C4 (chat)
          ├─→ A1                   (apple, independent)
          └─→ T                    (tokens, after chat churn settles)
```

Phase 0 + S1 + C1 are quick small-PR work; S2 + characterization suites are the biggest pure-test investment and best parallelization target; S3/S4/C2/C3 are the multi-week architectural core; C4/A1/T trail as rolling backlogs.

## Verification

- **Server, every PR**: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace` (or `-p` scoped). Post-S1: zero `#[allow]` outside justified exceptions, zero `format!("<` production hits. Post-S2: the glob conformance test enforces per-XEP suites. S4 flips: characterization suite passes unchanged through every producer-flip PR.
- **Chat, every PR**: `bun test && bun run lint` (knip clean, no new ignores). C2: shared fixtures pass against both pipelines before and after each extraction. C3/C4: each extracted module/composable lands with direct unit tests.
- **Apple**: xcodebuild for both targets via CI (PR + new main-branch job); XCTest for extracted models; JID round-trip tests against FFI.
- **Tokens**: regen script idempotent in CI (`git diff --exit-code`); visual spot-check chat + website; Apple builds.
- Per project convention, each phase's implementation work starts with a draft PR containing the phase plan, monitors CI to green, and finishes with adversarial-persona review before undrafting.
