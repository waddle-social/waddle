# AI Extension Completion Tasks

## Current State

- PR: https://github.com/waddle-social/waddle/pull/314
- Branch: `codex/server-ai-creep-removal`
- Status: draft until the AI chatbot is usable from the extension path.
- Existing PR work removed server-side AI provider/runtime coupling and chat-app AI special casing.
- The work is not complete until `/ai` and `@waddle` execute through the extension with real host tools and no server/chat AI creep.

## Requirements

- Keep AI orchestration inside the AI extension.
- Do not reintroduce server-side AI provider abstractions, model routing, prompt assembly, or chat-app AI rendering.
- Expose only XMPP-native host tools to extensions.
- Keep provider HTTP as a separate, explicit outbound runtime capability; it is not a host tool and extensions do not import raw `wasi:http`.
- Parse WIT string boundary values immediately into typed Rust protocol values in the host.
- Preserve XEP conformance for MAM, roster, MUC, PubSub Spaces, presence, replies, and threaded messages.
- Use official namespaces only when the exact XEP wire shape and behavior are implemented.
- Do not emit "provider unavailable" or other placeholder bot replies as the shipped behavior.
- Keep the PR draft until implementation, tests, adversarial review, CI, PR title, and PR description are all complete.

## Workstreams

### 1. Host Tool Contract

- [x] Add real WIT imports for extension host tools in `server/wit/waddle-extension.wit`.
- [x] Add typed request/response records for channels, spaces, members, presence, MAM history, roster, and message sends.
- [x] Add typed capabilities for each host tool family so manifests can request the minimum access needed.
- [x] Model first-release message targets as MUC or direct bare JID rather than free-form send strings. MUC occupant private sends are intentionally not shipped until Waddle has a conformant XEP-0045 private-message policy and pipeline.
- [x] Model roster subscriptions, MUC role/affiliation, and presence show/state as enums.
- [x] Model MAM queries with target, time bounds, optional sender filter, optional full-text search, and max result count.
- [x] Decide whether result pagination is required for the first usable release; if deferred, document the bounded first-release behavior.
- [x] Keep WIT strings as Wasm ABI values only; host-side code must convert to `Jid`, `BareJid`, `FullJid`, `DateTime<Utc>`, `MamQuery`, and typed message structs immediately.

### 2. Extension Runtime Plumbing

- [x] Add an `ExtensionHostTools` trait in `waddle-extensions` without depending on `waddle-server`.
- [x] Extend `HostState` with an `Arc<dyn ExtensionHostTools>`, invocation context, and approved capabilities.
- [x] Wire generated WIT host imports to real trait calls; no unconditional unavailable stubs.
- [x] Pass extension config into event handling so provider configuration is available outside `init`.
- [x] Enforce host/operator capability grants, bounded by the manifest, before a host tool or outbound HTTP can be used.
- [x] Return typed host-tool errors to Wasm and log operational details in the host.
- [x] Add focused runtime tests proving host imports call the trait and denied capabilities fail closed.

### 3. Server Host Adapter

- [x] Implement the `ExtensionHostTools` trait in `waddle-server`.
- [x] List channels using the existing XMPP channel path.
- [x] List Spaces using the existing PubSub Spaces storage path.
- [x] List current MUC occupants from room actors/snapshots without exposing real user JIDs.
- [x] Resolve member/presence state from live connection registry plus detached resumable sessions.
- [x] Read roster entries through existing roster storage and return typed subscription data.
- [x] Query MAM history via existing `MamStorage::query_messages` with time/search filters.
- [x] Send MUC bot messages through the canonical typed MUC dispatch/archive/inbox/fanout path.
- [x] Send direct messages through the existing typed chat-message routing/archive/carbons/offline path.
- [x] Do not ship member-targeted MUC private sends until there is a conformant MUC private-message pipeline. Direct bare-JID sends use the normal typed chat-message routing/archive/carbons/offline path.
- [x] Preserve room and user authorization checks before returning history, roster, members, or presence.
- [x] Add server tests for host tool adapter paths and denied access behavior.

### 4. AI Chatbot Extension

- [x] Register `/ai` as an extension command instead of relying only on raw message observation.
- [x] Register bot member/presence behavior needed for `@waddle` in MUCs.
- [x] Use host tools to gather relevant room/thread/DM context with MAM time/search bounds.
- [x] Use host tools for roster, channel/space, member, and presence context only when prompt intent selects that context source.
- [x] Implement provider execution inside the extension, not in the server or chat app.
- [x] Define the provider configuration contract for the extension.
- [x] Ensure missing provider configuration fails with a clear command error path, not a broken bot reply in room history.
- [x] Preserve threaded replies and reply metadata when sending answers without leaking real MUC sender JIDs into groupchat reply payloads.
- [x] Add unit tests for prompt cleaning, trigger handling, tool selection, provider request assembly, and response sending.
- [x] Add extension coverage proving `/ai` produces a visible answer through extension-owned execution.

### 5. XEP Review And Tests

- [x] Review the final design against `./xeps` before implementation is considered ready.
- [x] Add or update dedicated Rust custom test suites for any expanded XEP behavior.
- [x] Cover XEP-0313 MAM query semantics used by the extension.
- [x] Cover roster semantics used by the extension.
- [x] Cover MUC occupant, affiliation, and bot-message behavior used by the extension.
- [x] Cover direct chat routing behavior used by DM sends. Member-targeted MUC private sends are not shipped in this PR.
- [x] Cover PubSub Spaces lookup behavior if Spaces are exposed as a host tool.

### 6. Chat App Boundary

- [x] Keep chat UI rendering generic for extension payloads.
- [x] Do not add AI-specific branches, icons, labels, or provider state to the chat app.
- [x] Keep or update generic extension rendering tests as needed.

### 7. Verification

- [x] `cargo test -p ai-chatbot` after the latest review fixes.
- [x] `cargo test -p waddle-extensions` after the latest review fixes.
- [x] `cargo test -p waddle-server` after the member-target routing adjustment.
- [x] `cargo check --workspace --all-targets` after the latest review fixes.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` after the latest review fixes.
- [x] `cargo build -p ai-chatbot --target wasm32-wasip2` after the WIT ABI fix.
- [x] `bun test` after the latest review fixes.
- [x] `bun run lint` from `chat/` after the latest review fixes.
- [x] `helm template waddle ./server/charts/waddle-server --set spicedb.enabled=false` after the latest review fixes.
- [x] `git diff --check` after the latest review fixes.
- [x] Search for forbidden AI creep: `ai_provider`, `AiProviderFallback`, `WADDLE_AI`, `OPENROUTER`, `OPENAI_API`, `ai.invoke`, and chat-app AI special cases after the latest review fixes.

### 8. Review, PR, And CI

- [x] Run multiple adversarial subagent reviews after implementation. Boundary, XEP/privacy, and shipped usability reviews completed and produced actionable findings.
- [x] Action all real findings and rerun relevant verification.
- [x] Push the completed branch.
- [x] Update the PR title to accurately describe the final implemented work.
- [x] Update the PR description with the actual design, behavior, test coverage, next steps, and any justified lint/config exceptions.
- [x] Verify the rendered PR body after editing.
- [ ] Monitor CI until all checks are green.
- [ ] Undraft the PR only after the extension is usable and CI is green.

## Subagent Ownership Plan

- Runtime/WIT agent owns `server/wit/waddle-extension.wit` and `server/crates/waddle-extensions/**`.
- Server adapter agent owns `server/crates/waddle-server/src/**` host adapter and route integration changes.
- AI extension agent owns `server/extensions/ai-chatbot/**` and AI extension tests.
- XEP/test review agents own review findings first; implementation fixes remain with the relevant owner to avoid conflicting edits.
- Final adversarial reviewers inspect the whole diff for server/chat AI creep, XEP violations, missing tests, and unusable shipped behavior.
