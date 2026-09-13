# Typed Extension Ingress Identity (#1753) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route both extension-host dispatch paths (`ExtensionHostAdapter::dispatch_direct` and extension bot groupchat responses) through the ingress authority (plan → commit → execute) under a typed `IngressStreamIdentity::Extension` whose admission asserts a durable, revocable plugin grant instead of a persisted authenticated principal. Extension messages then obtain canonical rows, recorded intents, receipts, `groupchat_notification_recovery` rows and replay like every other ingress producer, and the immediate (non-ingress) branches are deleted. Closes RFC 0018 §1 stated limitation (vi).

**Spec:** GitHub issue #1753; RFC 0018 `server/docs/rfcs/0018-ingress-authority-cutover.md` (§1 (vi), §2, §3.1, §3.3c); runbook `server/docs/operations/ingress-authority.md`.

## Facts the plan is built on (verified on main 25bec708)

- `IngressStreamIdentity` (`ingress/identity.rs:16-37`) has `Resumable | Ephemeral { principal } | Relayed { canonical, room, room_fence }`. It is never serialized; **no consumer matches exhaustively** (`commit.rs:127-236`, `commit_stream.rs:20`, `commit_room.rs:21`, `ingress/mod.rs:443`). A new variant compiles silently and falls through every fence — each site must be revisited deliberately.
- `IngressSubmission.principal: AuthenticatedPrincipalRef` is non-optional (`submission.rs:14`); `commit.rs:122` unconditionally runs `PrincipalRepository::assert_principal` (`sessions` row by bare jid + auth_context_id + version + epoch, unexpired, `FOR SHARE` on Postgres). Other uses: `commit.rs:132,158,185,619` (sender bare only) and `commit.rs:484-487` (`IngressRelayAdmission` for `RelayMucProxy`, clustering only — **wire type**, `deliver_ordered.v10`).
- The semantic digest (`waddle-xmpp/src/ingress/digest/v1.rs`) takes only `DigestInput`; identity/principal never enter it. Golden vectors `tests/ingress_semantic_digest.rs:546` must stay unchanged.
- Extension dispatch today: `extension_host_adapter.rs:64-216` runs a throwaway `XmppStateMachine` + `interpret::interpret` on `ImmediateSink` (`interpret_deps`, `:259-283`, `ingress_effect_capture: None`). Requester invocations carry an unpersisted `Session::new` (`host_tools.rs:384`); provider webhooks carry `session: None` and are denied direct sends (`extension_host_adapter.rs:138`). Groupchat goes through `interpret::dispatch_extension_bot_groupchat_response` (`interpret/bot.rs:202-427`): registry `GetRoom` (local actor only — remote-owned rooms are dropped as `RoomNotRegistered`), outcast check, `JoinWithAffiliation`, presence sends, then `dispatch_bot_groupchat_response` (`bot.rs:19-145`) with `SyntheticSenderAuthority::ServerAuthored` and a recursive `interpret_with_depth` on the immediate sink.
- The single switch that decides whether recovery rows/intents exist is `deps.effects.is_planning()` (`groupchat_inbox.rs:163`, `offline_delivery.rs:113`). `interpret/offline_delivery_immediate.rs` (82 lines) is the immediate branch: inserts the pending row and the XEP-0357 candidate outside any receipt authority.
- There is **no durable plugin authorization table**. Provider room grants are config (`ExtensionModuleConfig.provider_room_grants`, checked in `authorize_provider_room`, `extension_host_adapter.rs:483-499`); requester authorization is the user's permission tuples. `PluginId` (`waddle-extensions/src/types/primitives.rs:125`) is already part of the ingress vocabulary (`IngressEffectIntent::RoomObserver { plugin }`), so `waddle-xmpp` can name it.
- Re-entrancy: plugin observers (`ingress/execute_observers.rs`) and command handling run inside `IngressAuthority::execute`, which holds `self.admission.read()` (tokio `RwLock<bool>`, write-preferring). A nested `commit`/`execute` taking `admission.read()` again deadlocks whenever a drain writer is queued (`ingress/mod.rs:494-500` documents the hazard).
- `IngressAuthority::execute` reports sender-facing frames as `report.frame_obligations`; the websocket path writes them and then calls `complete_frame_obligations` to settle their receipts. An extension sender has no transport; unsettled frame obligations would leave the row non-terminal forever.
- Latest migration is V1017 (`db/migrations/waddle.rs:1088`). Additive migrations roll with `RollingUpdate`; only ledger/wire breaks need `Recreate`.
- Test harness: `tests/ingress_support.rs::IngressFixture` (sqlite + postgres, `WADDLE_TEST_POSTGRES_URL`, per-test schema); aggregator `tests/ingress_commit.rs` with `ingress_cases/*.rs`; nextest group `postgres = { max-threads = 1 }` already covers `binary(ingress_commit)`. In-crate dual-backend tests exist under `src/ingress/*_tests.rs` (pattern: `xep0045_decline_recovery_tests.rs`) for cases needing `WebSocketState`.

## Global constraints

- Clippy `-D warnings`, `--all-targets --all-features`, no new `#[allow]`.
- Typed payloads: no new `String`/`&str` carrying protocol data on events, traits, structs. Grant identities, grantee kinds, denial reasons are enums/newtypes.
- No XML via `format!`. XEP shapes unchanged: extension direct and groupchat messages must keep today's wire shape (XEP-0045 occupant copy, XEP-0359 stanza-id, XEP-0201 thread, XEP-0461 reply, XEP-0394 markup).
- Settlement contract (RFC 0018): no actor/registry/extension/socket call while an ingress transaction is open. Pre-plan actor steps (room join) stay before Phase A.
- Every behaviour has SQLite + Postgres regressions. Verify with `cargo nextest run` from `server/`.
- No wire change: `deliver_ordered` stays v10; `IngressCanonicalRef`/`IngressRelayAdmission` shapes untouched.
- Conventional Commits `feat(server): ...`; do not merge PRs.

## Design

### D1. Typed principal + identity

- `waddle-xmpp/src/auth/extension_grant.rs` (new):
  - `ExtensionGrantId(Uuid)` newtype.
  - `ExtensionGrantee { Requester(BareJid), Provider { room: BareJid } }`.
  - `ExtensionGrantRef { grant_id: ExtensionGrantId, plugin: PluginId, grantee: ExtensionGrantee }` with `sender_bare(&self, extensions_domain) -> BareJid` semantics owned by the adapter (requester bare for `Requester`, plugin actor bare for `Provider`).
- `ingress/principal.rs` (new): `pub enum IngressPrincipal { Authenticated(AuthenticatedPrincipalRef), Extension(ExtensionGrantRef) }` with `bare_jid(&self) -> &BareJid` (the sender bare used at `commit.rs:132,158,185,619`). `IngressSubmission.principal: IngressPrincipal`. Every existing constructor wraps in `Authenticated`.
- `IngressStreamIdentity::Extension { plugin: PluginId, requester: Option<BareJid> }` (mirrors the issue's shape; `requester == None` for provider webhooks). Cross-check fence (like the `Ephemeral` principal fence at `commit.rs:127`): the identity's `(plugin, requester)` must equal the principal grant's `(plugin, grantee.requester())`, else `IngressUowError::PrincipalMismatch` (existing non-advancing class).

### D2. Durable grant table + repository (V1018, both dialects)

```sql
CREATE TABLE extension_grants (
  grant_id TEXT PRIMARY KEY,               -- uuid
  plugin_id TEXT NOT NULL,
  grantee_kind SMALLINT NOT NULL,          -- 0 = provider room grant, 1 = requester delegation
  requester_bare_jid TEXT,                 -- kind 1
  room_jid TEXT,                           -- kind 0
  granted_at TIMESTAMPTZ NOT NULL,         -- TEXT rfc3339 on SQLite
  expires_at TIMESTAMPTZ,
  revoked_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX extension_grants_active_requester ON extension_grants (plugin_id, requester_bare_jid) WHERE grantee_kind = 1 AND revoked_at IS NULL;
CREATE UNIQUE INDEX extension_grants_active_provider  ON extension_grants (plugin_id, room_jid)           WHERE grantee_kind = 0 AND revoked_at IS NULL;
```

`ingress_uow/repositories.rs::ExtensionGrantRepository`:
- `ensure_requester_delegation(tx, plugin, requester) -> ExtensionGrantRef` — returns the active row or inserts one (idempotent under the partial unique index; on conflict re-select).
- `sync_provider_grants(tx, plugin, rooms: &[BareJid]) -> ProviderGrantSync { inserted, revoked }` — inserts missing active `(plugin, room)` rows, sets `revoked_at` on active provider rows for that plugin not in `rooms`. Called once at server startup from the extension manager construction path for every plugin with `provider_room_grants` (and with an empty list for plugins that lost theirs), so the config remains the source of truth and the table is the durable fence.
- `active_provider_grant(tx, plugin, room) -> Option<ExtensionGrantRef>`.
- `revoke(tx, grant_id) -> bool` and `revoke_plugin(tx, plugin) -> u64` (plugin unloaded/removed).
- `assert_grant(tx, grant: &ExtensionGrantRef) -> GrantAssertion` — `SELECT plugin_id, grantee_kind, requester_bare_jid, room_jid, expires_at, revoked_at FROM extension_grants WHERE grant_id = ? FOR SHARE` (no `FOR SHARE` on SQLite); `Asserted` only when the row exists, `revoked_at IS NULL`, unexpired, and `(plugin, grantee)` equal the reference. For `Requester` grants the `users` row for the requester must still exist (join), so a deleted account fails closed. Any other result → `IngressUowError::ExtensionGrantAssertionFailed(GrantAssertionFailure::{Missing, Revoked, Expired, Mismatch, RequesterGone})` → classified with `PrincipalAssertionFailed` (non-advancing, principal loss).

### D3. Admission in `commit_attempt`

Replace the unconditional principal assertion with a `match &submission.principal`:
- `Authenticated(p)` → today's `assert_principal`.
- `Extension(grant)` → `assert_grant`, plus the D1 identity cross-check. For a `Provider { room }` grant the submission target must be that room (`NormalizedTarget` bare == room), else `Mismatch`.

Fences for the `Extension` identity: no SM stream (`lock_stream` returns `None` — make the `match` in `commit_stream.rs` explicit, not `let-else`), no `Relayed` canonical fence, room fence exactly as for `Ephemeral` (guarded local room effect → durable room claim `FOR SHARE` via `RoomExecutionPath::Local`). `RelayMucProxy` (remote-owned room) with an `Extension` principal is refused **in Phase A** with a typed plan failure (`IngressPlanFailure::ExtensionRemoteRoomUnsupported`, non-advancing) so `commit.rs:484-487` never needs an `AuthenticatedPrincipalRef` it does not have; that site becomes a `match` returning `IngressUowError::ExtensionRemoteRoomUnsupported` for defence in depth. Extension groupchat therefore stays local-owner only, exactly today's reach.

Revisit every non-exhaustive identity site listed in Facts and convert to explicit `match` arms where the variant matters (`ingress/mod.rs:443`, `commit_stream.rs:20`, `commit_room.rs:21`); document the decision per arm in a one-line comment.

### D4. Authority entry for nested submissions

`IngressAuthority` gains `commit_nested`/`execute_nested` (or a `Reentrancy::{Outer, Nested}` parameter) that acquire the admission lock with `try_read()`. `TryLockError` → `non_advancing(IngressDecisionClass::Storage)` (authority draining), never a wait. Extension submissions always use the nested entry because host tools may be invoked from inside an outer `execute`. Same for `complete_frame_obligations`. Regression: a test holds `admission.read()`, queues a writer, and proves a nested extension commit returns non-advancing within a bounded timeout instead of hanging.

### D5. Direct path through ingress

`dispatch_direct` becomes: `ensure_requester_delegation` (its own short uow tx, before Phase A) → build the `Message` and throwaway state machine as today → `interpret::plan_message_dispatch(&mut sm, message, &deps)` (plan mode) → `IngressSubmission { identity: Extension{..}, principal: Extension(grant), sender: actor_jid, target, digest_input (via `submission::digest_input` + `digest_authorities`), plan, connection_generation: ConnectionGeneration::fresh() }` → `ingress.commit_nested` → registry recheck not applicable → `ingress.execute_nested(&decision, &ImmediateSink, &deps)` → frame obligations (D6). Delete `interpret/offline_delivery_immediate.rs` and the non-planning branch of `apply_offline_delivery_row`: with no immediate caller left, `apply_offline_delivery_row` takes the planning capture unconditionally (no dead `else`).

### D6. Extension host as the sender's transport

The sender-facing frames in `ExecutionReport.frame_obligations` are the standard error reply and sender receipts. The adapter consumes them: the first `Message` frame carrying a `StanzaError` is mapped to a new typed `ExtensionHostAdapterError::Rejected(xmpp_parsers::stanza_error::StanzaError)` (surfaced to the plugin via `host_tool_error`); all frames are then marked written and `complete_frame_obligations_nested` settles their receipts so the canonical row terminalizes. A non-advancing decision maps to `ExtensionHostAdapterError::NotAuthorized` for principal loss / grant failures and to `Storage` otherwise, via a `From<IngressDecisionClass>`-style typed mapping (no strings).

### D7. Groupchat path through ingress

`dispatch_extension_bot_groupchat_response` keeps its pre-plan actor steps (registry lookup, outcast check, `JoinWithAffiliation`, occupant presence) and then, instead of `dispatch_bot_groupchat_response` + immediate interpret, builds the groupchat `Message` from the bot occupant (same body/thread/reply/markup/extensions/stanza-id/hat inputs) and runs plan → commit → execute as in D5, with `sender = plugin actor JID`, `principal = Extension(grant)` where the grant is the requester delegation (requester invocations) or the active provider grant (webhooks). Planning must reproduce today's dispatch semantics: `Deps` gains a typed `synthetic_sender_authority: Option<SyntheticSenderAuthority>` (and the bot nick/hat inputs already carried by `ExtensionRoomMessage`) that `room_dispatch` threads into the room chain context exactly where `bot.rs:121-123` set it. Existing wire-shape tests (`tests/room_dispatch.rs:306,511`) must pass unchanged. With planning on, `groupchat_notification_recovery_item` produces the `PlannedGroupchatNotificationRecovery`, so the recovery row gets its canonical `message_key` in Phase B. Delete `dispatch_bot_groupchat_response` (immediate) and any helper left unused.

### D8. Docs + ops

RFC 0018: strike (vi) from §1 (record it as resolved by #1753), extend §3.1 with the `Extension` identity, its grant fence and the local-owner-only groupchat reach; §6 notes V1018 as additive/RollingUpdate. Runbook: `extension_grants` inspection + revocation SQL, and the "extension" (kind 10) classification unchanged. `TODO-ACTOR.md`: no entry needed.

## Tasks (lanes)

Lane A must land before B and C; B ∥ C; D after B+C.

### Task A — foundation: types, table, repository, admission, nested authority entry

**Files:** new `waddle-xmpp/src/auth/extension_grant.rs`; `waddle-xmpp/src/auth/mod.rs`; new `waddle-server/src/ingress/principal.rs`; `ingress/identity.rs`, `ingress/submission.rs`, `ingress/commit.rs`, `ingress/commit_stream.rs`, `ingress/commit_room.rs`, `ingress/mod.rs`, `ingress_uow/repositories.rs`, `ingress_uow/error.rs` (or wherever `IngressUowError` lives), `db/migrations/waddle.rs` (V1018 both dialects), all `IngressSubmission` constructors (`websocket/frame.rs:1070`, `route_bridge/delivery/muc/reserved.rs`, test fixtures `tests/ingress_support.rs:118`, in-crate fixtures).

- [ ] Red: repository tests (SQLite + Postgres) — ensure/idempotent, sync inserts+revokes, assert Asserted/Missing/Revoked/Expired/Mismatch/RequesterGone.
- [ ] Red: `tests/ingress_cases/extension_identity.rs` (register in `tests/ingress_commit.rs`): (a) Extension submission with an active requester delegation commits an accepted canonical row whose recorded principal/sender bare is the requester; (b) revoked grant → non-advancing `PrincipalAssertionFailed` class, no canonical row; (c) identity/grant mismatch → `PrincipalMismatch`; (d) provider grant with a target other than the granted room → refused.
- [ ] Red: nested-entry regression (D4) in `src/ingress/` tests.
- [ ] Green: implement D1–D4; migration V1018.
- [ ] Verify: `cargo nextest run -p waddle-server ingress` on SQLite and with `WADDLE_TEST_POSTGRES_URL`; clippy all-targets all-features; digest golden vectors unchanged.
- [ ] Commit `feat(server): typed extension ingress principal, durable grant admission and nested authority entry (#1753)`.

### Task B — direct path

**Files:** `server/extension_host_adapter.rs`, `extension_host_adapter/{types,host_tools}.rs`, `interpret/offline_delivery.rs`, delete `interpret/offline_delivery_immediate.rs`, `interpret/mod.rs`/`interpret.rs` re-exports, `ingress/mod.rs` (frame-completion nested entry).

- [ ] Red (SQLite + Postgres, in-crate where `WebSocketState` is needed): extension direct send to an offline recipient → pending row + XEP-0357 candidate + `PendingDelivery`/`NotificationActivityPreview`/`RouteDirect` receipts + row terminal; canonical envelope `from == requester/extension-host`; second send with the same stanza-id → alias `Existing`, exactly one pending row.
- [ ] Red: send to a recipient that blocks the requester (XEP-0191) → `ExtensionHostAdapterError::Rejected(service-unavailable)`, frame receipts settled, row terminal.
- [ ] Red: revoked delegation between two sends → second send `NotAuthorized`, no row.
- [ ] Green: D5 + D6; delete the immediate module and branch.
- [ ] Verify + commit `feat(server): route extension direct dispatch through ingress and drop immediate offline delivery (#1753)`.

### Task C — groupchat path + provider grant sync

**Files:** `interpret/bot.rs`, `interpret/room_dispatch.rs` (sender authority threading), `interpret/deps.rs`, `server/extension_host_adapter.rs`, extension manager construction (`waddle-extensions/src/manager/construction.rs` call site in waddle-server startup for `sync_provider_grants`), `interpret/tests/room_dispatch.rs`.

- [ ] Red (SQLite + Postgres, in-crate): extension groupchat to a local room with an offline durable recipient → `groupchat_notification_recovery` row with the canonical `message_key`; inject a T0 push-policy failure → `DeferredPolicy` intent recorded → `reconcile_groupchat_notification_recovery` sweep completes → receipts → terminal.
- [ ] Red: provider webhook groupchat with a synced provider grant succeeds; after `sync_provider_grants(plugin, [])` the same send is `NotAuthorized`.
- [ ] Red: extension groupchat to a remote-owned room (clustering feature) → typed plan failure, no canonical row (today's drop, now typed).
- [ ] Green: D7 + startup sync; existing wire-shape tests unchanged.
- [ ] Verify + commit `feat(server): route extension groupchat dispatch through ingress with provider grant sync (#1753)`.

### Task D — docs, final sweep

- [ ] RFC 0018 + runbook edits (D8); remove stale comments referencing the immediate path; `cargo doc` links.
- [ ] Full `cargo nextest run --workspace --all-targets` on SQLite + Postgres; clippy; fmt.
- [ ] Commit `docs(server): record the typed extension ingress identity in RFC 0018 and the runbook (#1753)`.

## Out of scope (state in PR)

- Extension groupchat to remote-owned rooms (today it is dropped; now a typed refusal). Relaying would change `IngressRelayAdmission` → `deliver_ordered.v11`; separate issue.
- Operator UI for grant revocation; only the repository API + admission semantics ship.
- #1660 opaque delivery keys for extension effects (kind 10) — unchanged.
