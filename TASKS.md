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
- Parse WIT string boundary values immediately into typed Rust protocol values in the host.
- Preserve XEP conformance for MAM, roster, MUC, PubSub Spaces, presence, replies, and threaded messages.
- Use official namespaces only when the exact XEP wire shape and behavior are implemented.
- Do not emit "provider unavailable" or other placeholder bot replies as the shipped behavior.
- Keep the PR draft until implementation, tests, adversarial review, CI, PR title, and PR description are all complete.

## Workstreams

### 1. Host Tool Contract

- [ ] Add real WIT imports for extension host tools in `server/wit/waddle-extension.wit`.
- [ ] Add typed request/response records for channels, spaces, members, presence, MAM history, roster, and message sends.
- [ ] Add typed capabilities for each host tool family so manifests can request the minimum access needed.
- [ ] Model message targets as MUC, DM JID, or member JID rather than free-form send strings.
- [ ] Model roster subscriptions, MUC role/affiliation, and presence show/state as enums.
- [ ] Model MAM queries with target, time bounds, optional sender filter, optional full-text search, and max result count.
- [ ] Decide whether result pagination is required for the first usable release; if deferred, document the bounded first-release behavior.
- [ ] Keep WIT strings as Wasm ABI values only; host-side code must convert to `Jid`, `BareJid`, `FullJid`, `DateTime<Utc>`, `MamQuery`, and typed message structs immediately.

### 2. Extension Runtime Plumbing

- [ ] Add an `ExtensionHostTools` trait in `waddle-extensions` without depending on `waddle-server`.
- [ ] Extend `HostState` with an `Arc<dyn ExtensionHostTools>`, invocation context, and approved capabilities.
- [ ] Wire generated WIT host imports to real trait calls; no unconditional unavailable stubs.
- [ ] Pass extension config into event handling so provider configuration is available outside `init`.
- [ ] Enforce manifest/capability grants before a host tool can be used.
- [ ] Return typed host-tool errors to Wasm and log operational details in the host.
- [ ] Add focused runtime tests proving host imports call the trait and denied capabilities fail closed.

### 3. Server Host Adapter

- [ ] Implement the `ExtensionHostTools` trait in `waddle-server`.
- [ ] List channels using the existing XMPP channel path.
- [ ] List Spaces using the existing PubSub Spaces storage path.
- [ ] List MUC occupants and persistent affiliations from room actors/snapshots.
- [ ] Resolve member/presence state from live connection registry plus detached resumable sessions.
- [ ] Read roster entries through existing roster storage and return typed subscription data.
- [ ] Query MAM history via existing `MamStorage::query_messages` with time/search filters.
- [ ] Send MUC bot messages through the canonical typed MUC dispatch/archive/inbox/fanout path.
- [ ] Send DM/member messages through the existing typed chat-message routing/archive/carbons/offline path.
- [ ] Preserve room and user authorization checks before returning history, roster, members, or presence.
- [ ] Add server tests for each host tool adapter path and denied access behavior.

### 4. AI Chatbot Extension

- [ ] Register `/ai` as an extension command instead of relying only on raw message observation.
- [ ] Register bot member/presence behavior needed for `@waddle` in MUCs.
- [ ] Use host tools to gather relevant room/thread/DM context with MAM time/search bounds.
- [ ] Use host tools for roster, channel/space, member, and presence context where useful to answer the prompt.
- [ ] Implement provider execution inside the extension, not in the server or chat app.
- [ ] Define the provider configuration contract for the extension.
- [ ] Ensure missing provider configuration fails with a clear command error path, not a broken bot reply in room history.
- [ ] Preserve threaded replies and reply metadata when sending answers.
- [ ] Add unit tests for prompt cleaning, trigger handling, tool selection, provider request assembly, and response sending.
- [ ] Add integration coverage proving `/ai` produces a usable answer through extension-owned execution.

### 5. XEP Review And Tests

- [ ] Review the final design against `./xeps` before implementation is considered ready.
- [ ] Add or update dedicated Rust custom test suites for any expanded XEP behavior.
- [ ] Cover XEP-0313 MAM query semantics used by the extension.
- [ ] Cover roster semantics used by the extension.
- [ ] Cover MUC occupant, affiliation, and bot-message behavior used by the extension.
- [ ] Cover direct chat routing behavior used by DM/member sends.
- [ ] Cover PubSub Spaces lookup behavior if Spaces are exposed as a host tool.

### 6. Chat App Boundary

- [ ] Keep chat UI rendering generic for extension payloads.
- [ ] Do not add AI-specific branches, icons, labels, or provider state to the chat app.
- [ ] Keep or update generic extension rendering tests as needed.

### 7. Verification

- [ ] `cargo test -p ai-chatbot`
- [ ] `cargo test -p waddle-extensions`
- [ ] `cargo test -p waddle-server`
- [ ] `cargo check --workspace --all-targets`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `bun test chat-ui-render.test.ts`
- [ ] `bun run lint`
- [ ] `helm template waddle ./charts/waddle-server --set spicedb.enabled=false`
- [ ] `git diff --check`
- [ ] Search for forbidden AI creep: `ai_provider`, `AiProviderFallback`, `WADDLE_AI`, `OPENROUTER`, `OPENAI_API`, `ai.invoke`, and chat-app AI special cases.

### 8. Review, PR, And CI

- [ ] Run multiple adversarial subagent reviews after implementation.
- [ ] Action all real findings and rerun relevant verification.
- [ ] Push the completed branch.
- [ ] Update the PR title to accurately describe the final implemented work.
- [ ] Update the PR description with the actual design, behavior, test coverage, and any justified lint/config exceptions.
- [ ] Verify the rendered PR body after editing.
- [ ] Monitor CI until all checks are green.
- [ ] Undraft the PR only after the extension is usable and CI is green.

## Subagent Ownership Plan

- Runtime/WIT agent owns `server/wit/waddle-extension.wit` and `server/crates/waddle-extensions/**`.
- Server adapter agent owns `server/crates/waddle-server/src/**` host adapter and route integration changes.
- AI extension agent owns `server/extensions/ai-chatbot/**` and AI extension tests.
- XEP/test review agents own review findings first; implementation fixes remain with the relevant owner to avoid conflicting edits.
- Final adversarial reviewers inspect the whole diff for server/chat AI creep, XEP violations, missing tests, and unusable shipped behavior.
