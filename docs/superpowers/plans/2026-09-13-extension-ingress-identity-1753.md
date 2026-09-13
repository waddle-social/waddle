# Typed Extension Ingress Identity (#1753) Implementation Plan — v2

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Route both extension-host dispatch paths (`ExtensionHostAdapter::dispatch_direct` and extension bot groupchat responses) through the ingress authority (plan → commit → execute) under a typed `IngressStreamIdentity::Extension` whose admission asserts a durable, revocable plugin send grant instead of a persisted authenticated principal. Extension messages then obtain canonical rows, recorded intents, receipts, `groupchat_notification_recovery` rows and replay like every other ingress producer, and the immediate (non-ingress) branches are deleted. Closes RFC 0018 §1 stated limitation (vi).

**Spec:** GitHub issue #1753; RFC 0018 `server/docs/rfcs/0018-ingress-authority-cutover.md` (§1 (vi), §2, §3.1, §3.3c); runbook `server/docs/operations/ingress-authority.md`. Plan review round 1 (gpt-6-astra, high): REVISE — findings 1–5 blocking, 6–9 should-fix; every one is addressed below and marked **[R1-n]**.

## Facts the plan is built on (verified on main 25bec708; corrected per R1)

- `IngressStreamIdentity` (`ingress/identity.rs:16-37`) has `Resumable | Ephemeral { principal } | Relayed { canonical, room, room_fence }`. It is never serialized. Consumers use non-exhaustive `if let`/`matches!`/`let-else` (`commit.rs:127-236`, `commit_stream.rs:20`, `commit_room.rs:21`, `ingress/mod.rs:443`). A new variant compiles silently; **the principal assertion stays unconditional** (`commit.rs:122`) and **local-room fencing is driven by `plan.room_execution`, not the identity** (`commit_room.rs:39`) **[R1-FALSE-1]** — so the variant must branch the principal assertion and leave room fencing as is.
- `IngressSubmission.principal: AuthenticatedPrincipalRef` (`submission.rs:14`). Uses: `assert_principal` (`commit.rs:122` → `repositories.rs:1294`: `sessions` row by bare jid + auth_context_id + version + epoch, unexpired, `FOR SHARE`); sender bare for alias resolution and digest authorities (`commit.rs:132,158,185,619`); `IngressRelayAdmission` for `RelayMucProxy` (`commit.rs:484-487`, clustering, wire type in `deliver_ordered.v10`). Identity mismatch today maps to `PrincipalAssertionFailed` — there is no `PrincipalMismatch` class **[R1-FALSE-4]**.
- The semantic digest v1 takes only `DigestInput` (`digest/v1.rs`); **alias replay exists only through a XEP-0359 origin-id** (`digest/input.rs:199`, `commit.rs:182`). Both extension builders today set only `Message.id`, no origin-id (`extension_host_adapter.rs:165`, `bot.rs:34`), so a repeated stanza id allocates a fresh canonical row **[R1-5]**.
- Extension dispatch today: `extension_host_adapter.rs:64-216` runs a throwaway `XmppStateMachine` + `interpret::interpret` on `ImmediateSink` (`interpret_deps`, `:259-283`). Requester invocations carry an unpersisted `Session::new` (`host_tools.rs:384`); provider webhooks carry `session: None` and are denied direct sends (`:138`). Direct sends are additionally gated by roster subscription (`authorize_direct_send`, `:436`) and room sends by permission tuples / provider room grants; the runtime also enforces the `HostMessageSend` manifest capability (`waddle-extensions/src/runtime/host_state.rs:251`) **[R1-1]**.
- Groupchat goes through `interpret::dispatch_extension_bot_groupchat_response` (`bot.rs:202-427`): local `GetRoom` only (remote-owned rooms dropped), outcast check, `JoinWithAffiliation`, presence, then `dispatch_bot_groupchat_response` (`bot.rs:19-145`) with `SyntheticSenderAuthority::ServerAuthored`, `project_sender_inbox: false` (`bot.rs:111` — the bot is **not** a reflected recipient, `context.rs:193`), validated+signed extension envelopes preserved (`bot.rs:66`; ordinary room dispatch rejects such envelopes at `room_dispatch.rs:478`), no enrichment, no observers. Ordinary room planning would also run the announcements-room owner check against `Deps.authenticated_principal` (`room_dispatch.rs:398`) and invoke observers with the plugin actor as requester (`room_dispatch.rs:689`) **[R1-4]**.
- `RouteMucGroupchat` records `reflection = sender_full` and receipt coverage requires the reflection copy (`room_dispatch.rs:671`, `receipts_routing.rs:71`); receipt matching recognises `ExternalEffect::Frame` (`receipts_routing.rs:253`) **[R1-3]**.
- Planning is necessary but not sufficient for a recovery row: `groupchat_notification_recovery_item` also needs a durable recipient, the room stanza id and the sender (`groupchat_inbox.rs:163`) **[R1-FALSE-2]**. `interpret/offline_delivery_immediate.rs::execute_immediate` is called from `offline_delivery.rs:113` **and** `effects/delivery_immediate.rs:126` **[R1-6]**.
- Re-entrancy: observers/commands run inside `IngressAuthority::execute`, which holds `self.admission.read()` (tokio write-preferring `RwLock<bool>`, `mod.rs:475`, `execute_observers.rs:57`); drain cancels the token before taking the writer (`mod.rs:553`) **[R1-2]**.
- Sender-facing frames are `ExecutionReport.frame_obligations`; persistence can fail partway (`execute.rs:269`); maintenance cannot rebuild frame-only obligations (`recovery_executor.rs:88`). Offline quota refusal settles its obligations and bounces through the connection registry to the exact sender resource (`execute_offline.rs:82`, `offline_delivery.rs:182`) — for `requester/extension-host` that resource is never registered **[R1-8]**.
- `ConnectionGeneration` has no `fresh()`; it is the transport fence (`waddle-xmpp/src/ingress/generation.rs:21`) **[R1-FALSE-4]**.
- Latest migration V1017. Ledger guards act at startup only; the runbook (`:257`) records that additive migrations still needed `Recreate` when old writers bypassed the new authority **[R1-9]**.
- Test harness: `tests/ingress_support.rs::IngressFixture` (SQLite + Postgres); in-crate `WebSocketState` + room-actor fixtures are feasible (`recovery_executor_tests.rs:494`); nextest postgres group covers `binary(ingress_commit)` (`nextest.toml:30`). Wire-shape test `tests/room_dispatch.rs:370` calls `dispatch_bot_groupchat_response` directly **[R1-6]**.

## Global constraints

- Clippy `-D warnings --all-targets --all-features`, no new `#[allow]`. Typed payloads only. No XML via `format!`.
- XEP shapes: extension messages keep today's wire shape **plus** a typed XEP-0359 `<origin-id/>` (§ D5) **[R1-5]**; XEP-0045 occupant copies, XEP-0201 thread, XEP-0461 reply, XEP-0394 markup unchanged.
- Settlement contract (RFC 0018): no actor/registry/extension/socket call inside an ingress transaction. Bot lifecycle work (join, presence) is separate pre-plan work, authorised before mutation, and is **not** covered by message receipts **[R1-4]**.
- No wire change to `deliver_ordered.v10` / `IngressRelayAdmission`.
- Every behaviour has SQLite + Postgres regressions; verify with `cargo nextest run` from `server/`.
- Conventional Commits `feat(server): ...`; do not merge PRs.

## Design

### D1. Typed principal + identity **[R1-5: grantee ≠ sender]**

- `waddle-xmpp/src/auth/extension_grant.rs` (new): `ExtensionGrantId(Uuid)`; `ExtensionGrantScope { Send, ProviderRoom(BareJid) }`; `ExtensionGrantRef { grant_id, plugin: PluginId, scope }`.
- `ingress/principal.rs` (new):
  ```rust
  pub struct ExtensionPrincipal { pub grant: ExtensionGrantRef, pub requester: Option<BareJid>, pub sender: BareJid }
  pub enum IngressPrincipal { Authenticated(AuthenticatedPrincipalRef), Extension(ExtensionPrincipal) }
  impl IngressPrincipal { pub fn bare_jid(&self) -> &BareJid }  // Authenticated → principal bare; Extension → `sender`
  ```
  `sender` is the **effective sender bare**: the requester for direct sends, the plugin actor bare (`plugin@extensions-domain`) for groupchat. Alias resolution and digest authorities use `bare_jid()`, so a requester's own client, plugin X and plugin Y never share `(sender, room, origin)`.
- `IngressStreamIdentity::Extension { plugin: PluginId, requester: Option<BareJid> }`; cross-check fence: must equal the principal's `(grant.plugin, requester)` else `IngressUowError::PrincipalAssertionFailed` (existing class).
- Transport generation: `IngressSubmission.connection_generation` becomes a typed `TransportGeneration { Connection(ConnectionGeneration), Host }`; every consumer is matched explicitly (`Host` has no registry recheck) **[R1-FALSE-4]**.

### D2. Durable plugin send grant (V1018, both dialects) **[R1-1]**

No per-invocation or lazy delegation. One durable, revocable **send grant per plugin** plus one **provider room grant per (plugin, room)**:

```sql
CREATE TABLE extension_grants (
  grant_id TEXT PRIMARY KEY,
  plugin_id TEXT NOT NULL,
  scope SMALLINT NOT NULL,        -- 0 = send, 1 = provider room
  room_jid TEXT,                  -- scope 1
  granted_at TIMESTAMPTZ NOT NULL,   -- TEXT rfc3339 on SQLite
  revoked_at TIMESTAMPTZ
);
CREATE UNIQUE INDEX extension_grants_active_send ON extension_grants (plugin_id) WHERE scope = 0 AND revoked_at IS NULL;
CREATE UNIQUE INDEX extension_grants_active_room ON extension_grants (plugin_id, room_jid) WHERE scope = 1 AND revoked_at IS NULL;
```

`ingress_uow/repositories.rs::ExtensionGrantRepository`:
- `sync_configured(tx, desired: &[ConfiguredPluginGrants]) -> GrantSync` — called once at startup with the **complete** configured plugin set: for every configured plugin with the `HostMessageSend` capability an active send grant exists (insert if missing); provider room grants exist exactly for the configured rooms; active grants for plugins **not** in the set, plugins that lost the capability, or rooms no longer configured get `revoked_at` set. Configuration is the source of truth: a restart with unchanged config re-grants by design; a durable revocation is expressed by removing the plugin/room/capability from config (documented in the runbook).
- `revoke_plugin(tx, plugin) -> u64` — runtime revocation (plugin unload/hot reload) sets `revoked_at` on all active grants; the running process also drops its in-memory grant refs.
- `active_send_grant(tx, plugin) -> Option<ExtensionGrantRef>`, `active_room_grant(tx, plugin, room) -> Option<ExtensionGrantRef>` — resolved by the adapter per dispatch, before Phase A, and carried in the submission. A revoked grant can never be re-minted by sending.
- `assert_grant(tx, grant) -> GrantAssertion` — `SELECT ... WHERE grant_id = ? FOR SHARE`; `Asserted` only if present, `revoked_at IS NULL`, `plugin`+`scope`(+room) equal the reference. For a requester invocation the requester's `users` row is also asserted (`FOR SHARE`) so a deleted account fails closed. Failures → `IngressUowError::ExtensionGrantAssertionFailed(GrantAssertionFailure::{Missing, Revoked, Mismatch, RequesterGone})`, classified as `PrincipalAssertionFailed`.

Existing pre-plan authorisation stays as it is (roster subscription for direct, permission tuples / provider room + `source_room` for rooms, envelope validation/signing, manifest capability): the grant is the **durable Phase B fence** that those in-memory checks lacked, not a replacement for them.

### D3. Admission in `commit_attempt`

`match &submission.principal { Authenticated(p) => assert_principal(p), Extension(x) => assert_grant(&x.grant) + requester assertion + identity cross-check + (ProviderRoom(room) ⇒ target bare == room) }`. Fences: no SM stream (`commit_stream.rs` `match` made explicit); no `Relayed` canonical fence; room fencing unchanged (driven by `plan.room_execution`). `RelayMucProxy` with an `Extension` principal is refused in Phase A by a typed `IngressPlanFailure::ExtensionRemoteRoomUnsupported` (today's reach: local rooms only); `commit.rs:484` becomes a `match` that returns `IngressUowError::ExtensionRemoteRoomUnsupported` for defence in depth.

### D4. Nested authority operation with one permit **[R1-2]**

Recursive `admission.read()` is unsound with a queued writer. Instead the adapter obtains one permit **before Phase A** and holds it across commit → execute → frame settlement:

```rust
pub struct NestedIngressOperation<'a> { authority: &'a IngressAuthority, _permit: RwLockReadGuard<'a, bool> }
impl IngressAuthority { pub fn try_begin_nested(&self) -> Result<NestedIngressOperation<'_>, AuthorityUnavailable> }  // try_read(); cancelled or draining → typed refusal, no wait
impl NestedIngressOperation<'_> { async fn commit(&self, &IngressSubmission) -> IngressDecision; async fn execute(&self, &IngressDecision, &dyn EffectSink, &Deps<'_>) -> ExecutionReport; async fn complete_frame_obligations(&self, &mut ExecutionReport) -> Result<bool, ExecutionPersistenceFailure> }
```
- Refusal happens only **before commit** (`try_begin_nested` fails, or `commit` sees the cancellation token). Once committed, execution and settlement proceed under the same permit; a drain writer waits for the permit like it waits for the outer execute today.
- Frame settlement failure: bounded retry of `complete_frame_obligations` (same 5 s budget as websocket); on final failure the decision stays committed and the row pending; the adapter returns a typed `ExtensionHostAdapterError::Committed(CommittedButUnsettled)` so the plugin learns the message was accepted but its response proof is pending. This is the same at-least-once frame limitation RFC 0018 already states for socket senders; documented for the host transport.
- Tests: nested adapter dispatch (a) with drain requested before `try_begin_nested` → refused, no row; (b) drain requested between commit and execute → execute completes, row terminal, drain completes after; (c) receipt-persistence failure during frame settlement → `Committed(..)`, row pending with committed canonical row; (d) invoked from inside an observer hook while a writer is queued → no deadlock (timeout-guarded).

### D5. Direct path through ingress **[R1-5][R1-6][R1-8]**

`dispatch_direct`: resolve `active_send_grant` (own short tx) → `try_begin_nested` → build the `Message` with `Message.id = stanza_id` **and** typed `xep0359::OriginId(stanza_id)` → throwaway state machine → `plan_message_dispatch` → `IngressSubmission { identity: Extension{plugin, requester: Some(req)}, principal: Extension{grant, requester, sender: requester}, sender: requester/extension-host, target, digest_input, plan, connection_generation: Host }` → `commit` → `execute(&ImmediateSink, &deps)` → D6. An adapter-level retry after a non-advancing `Storage` decision reuses the same origin-id and aliases to the existing row (`AliasOutcome::Existing`).

Delete `interpret/offline_delivery_immediate.rs`, the non-planning branch of `apply_offline_delivery_row`, and the `execute_immediate` arm in `effects/delivery_immediate.rs:126`; ingress-owned offline effects must never reach generic immediate execution (the `QueueOfflineDelivery` variant is arm-owned in `execute_uow`). Port the immediate-path tests to the ingress path.

### D6. Extension host as the sender's transport **[R1-3][R1-8]**

- `ExecutionReport.frame_obligations` for an `Extension` submission are consumed by the adapter: the first frame carrying a `StanzaError` maps to `ExtensionHostAdapterError::Rejected(StanzaError)`; all frames are then marked written and settled via `complete_frame_obligations` (D4).
- **Quota**: for an `Extension` principal the offline quota bounce is emitted as a sender **frame obligation** (typed `resource-constraint` error reply) instead of a registry send, so the host sees `Rejected(resource-constraint)`, no pending row/candidate is inserted, obligations are settled, and replay of the same origin-id stays settled. Websocket senders keep the registry bounce.
- Non-advancing decisions map typed: `PrincipalAssertionFailed`/grant failures → `NotAuthorized`; `Storage`/authority unavailable → `Storage`; plan failures → `Unsupported`.

### D7. Groupchat path: a dedicated typed trusted-bot planning entry **[R1-3][R1-4]**

Instead of threading one flag through ordinary room planning, `interpret::bot` gains `plan_extension_bot_groupchat(deps, room, room_sender, ExtensionRoomMessage) -> Result<IngressPlan, ExtensionBotDispatchError>` — `dispatch_bot_groupchat_response` executed in plan mode (`PlanSink` + `IngressEffectCapture`), preserving today's semantics exactly: `SyntheticSenderAuthority::ServerAuthored`, `project_sender_inbox: false`, validated+signed extension envelopes accepted (the `room_dispatch.rs:478` rejection is not on this path), managed-room authorisation already done by the adapter (no `Deps.authenticated_principal` owner check), no enrichment, no observers. It captures the canonical message (room-canonical `from = room/nick`, room stanza id), the local room fence, `ArchiveGroupchat` with its authoritative stanza id and commit ordinal (`groupchat_archive.rs:303`, `archive_authority.rs:109`), occupant fan-out, `RouteMucGroupchat`, inbox projections and `PlannedGroupchatNotificationRecovery`.
- **Bot reflection**: because the bot is not a reflected recipient, the plan emits a typed host-consumed reflection `ExternalEffect::Frame` for the bot sender (the same copy a connected sender would receive), so the `RouteMucGroupchat` receipt coverage that requires the reflection is satisfied and the adapter settles it through D6. Wire shape to occupants unchanged; the bot's frame is consumed by the host (logged, never sent anywhere).
- Pre-plan bot lifecycle steps (registry lookup, outcast check, `JoinWithAffiliation`, presence) stay where they are, before Phase A, authorised as today; they are separate from message receipts and documented as such.
- Grant: requester invocations use the plugin's send grant with `requester = Some(req)`; provider webhooks use the `ProviderRoom(room)` grant with `requester = None`. `principal.sender = plugin actor bare`.
- With planning on and the durable recipient/stanza id/sender present, `groupchat_notification_recovery_item` yields the recovery row with its canonical `message_key`.
- Delete the immediate `dispatch_bot_groupchat_response` execution path; port `tests/room_dispatch.rs:370,306,511` to the planning entry keeping their behavioural assertions (nick base, threaded MUC message, managed-room scope).

### D8. Docs, rollout **[R1-9]**

- RFC 0018: strike (vi); §3.1 documents the `Extension` identity, grant fence, host transport (frames consumed by the host; at-least-once frame settlement), local-room-only reach; §6: V1018 is additive and rolls with `RollingUpdate`, with an explicit **enforcement window**: until every pod runs the new binary, extension sends from old pods still bypass ingress (today's behaviour, which does not corrupt new-writer state because old binaries never read `extension_grants`); guarantees become global when the rollout completes; pre-V1018 binaries cannot restart against the advanced ledger (existing startup guard).
- Runbook: `extension_grants` inspection/revocation SQL, config-driven revocation semantics, "extension" (kind 10) classification unchanged, host-transport frame limitation.

## Tasks (lanes)

Lane A first; B ∥ C after A; D last.

### Task A — foundation: types, table, repository, admission, nested operation

**Files:** new `waddle-xmpp/src/auth/extension_grant.rs`; `waddle-xmpp/src/auth/mod.rs`; new `waddle-server/src/ingress/principal.rs`; `ingress/{identity,submission,commit,commit_stream,commit_room,mod}.rs`; `ingress_uow/repositories.rs` + error enum; `waddle-xmpp/src/ingress/generation.rs` (`TransportGeneration`); `db/migrations/waddle.rs` (V1018 both dialects + `db/migrations/tests.rs` catalog); all `IngressSubmission` constructors (`websocket/frame.rs:1070`, `route_bridge/delivery/muc/reserved.rs`, `tests/ingress_support.rs:118`, in-crate fixtures).

- [ ] Red: repository tests (SQLite + Postgres): `sync_configured` inserts/revokes for added, removed, capability-lost plugins and room changes; `revoke_plugin`; `assert_grant` Asserted/Missing/Revoked/Mismatch/RequesterGone.
- [ ] Red: `tests/ingress_cases/extension_identity.rs` (register in `tests/ingress_commit.rs`): (a) Extension submission with an active send grant commits; recorded sender bare == principal `sender`; (b) revoked grant → `PrincipalAssertionFailed`, no row; (c) identity/grant mismatch → `PrincipalAssertionFailed`; (d) `ProviderRoom` grant with another target → refused; (e) same origin-id twice → `Existing`, one row; (f) plugin X and plugin Y with the same origin-id → two rows (sender separation).
- [ ] Red: D4 nested-operation tests (a)–(d).
- [ ] Green: D1–D4 + V1018.
- [ ] Verify (`cargo nextest run -p waddle-server ingress`, both backends; clippy; digest golden vectors) + commit `feat(server): typed extension ingress principal, durable plugin grant admission and nested authority operation (#1753)`.

### Task B — direct path

**Files:** `server/extension_host_adapter.rs`, `extension_host_adapter/{types,host_tools}.rs`, `interpret/offline_delivery.rs`, delete `interpret/offline_delivery_immediate.rs`, `interpret/effects/delivery_immediate.rs`, `ingress/execute_offline.rs` (quota frame for Extension), `ingress/mod.rs`.

- [ ] Red (in-crate, SQLite + Postgres, real adapter → plan → commit → execute): direct send to an offline recipient → pending row + XEP-0357 candidate + `PendingDelivery`/`NotificationActivityPreview`/`RouteDirect` receipts + terminal; canonical envelope `from == requester/extension-host` with origin-id; adapter retry with the same origin-id → one pending row, one candidate; recipient/sender archive ids stable on replay **[R1-7]**.
- [ ] Red: lost Phase C after a frozen `Inserted` candidate obligation → maintenance recovery completes without re-evaluating policy, no duplicate candidate, terminal **[R1-7]** (also added to the dedicated XEP-0357/XEP-0160 suites).
- [ ] Red: recipient blocks requester (XEP-0191) → `Rejected(service-unavailable)`, frames settled, terminal.
- [ ] Red: offline quota exceeded → `Rejected(resource-constraint)`, no pending row/candidate, obligations settled, replay stays settled **[R1-8]**.
- [ ] Red: `revoke_plugin` between two sends → second `NotAuthorized`, no row; grant revoked concurrently with an in-flight commit → either committed-before-revoke or refused, never half-written.
- [ ] Green: D5 + D6; delete immediate module/branches/arm; port immediate-path tests.
- [ ] Verify + commit `feat(server): route extension direct dispatch through ingress and drop immediate offline delivery (#1753)`.

### Task C — groupchat planning entry + provider grant sync

**Files:** `interpret/bot.rs`, `interpret/room_dispatch.rs` (only if a shared helper is extracted), `interpret/deps.rs`, `server/extension_host_adapter.rs`, startup wiring for `sync_configured` (where the extension manager is constructed in waddle-server), `interpret/tests/room_dispatch.rs`.

- [ ] Red (in-crate, SQLite + Postgres, real adapter → plan → commit → execute with a room actor): extension groupchat to a local room with a live recipient and an offline durable recipient → occupant wire shape identical to today (`from = room/nick`, hat, thread, reply, markup, stanza-id), `groupchat_notification_recovery` row with the canonical `message_key`, groupchat archive id/ordinal recorded, bot reflection frame settled, full route receipt set, terminal.
- [ ] Red: T0 push-policy failure → `DeferredPolicy` recorded → `reconcile_groupchat_notification_recovery` completes → terminal; candidate-insertion failure after a frozen `Inserted` obligation → recovered without T0 re-evaluation, no duplicate **[R1-7]**.
- [ ] Red: provider webhook groupchat with a synced room grant succeeds; after `sync_configured` without that room → `NotAuthorized`; validated+signed extension envelope preserved on the wire.
- [ ] Red (clustering): extension groupchat to a remote-owned room → typed plan failure, no row.
- [ ] Green: D7 + startup sync; port the bot tests; delete the immediate bot execution.
- [ ] Verify + commit `feat(server): plan extension groupchat responses through ingress with provider grant sync (#1753)`.

### Task D — docs, final sweep

- [ ] D8 docs; remove stale comments; full `cargo nextest run --workspace --all-targets --all-features` both backends; clippy; fmt.
- [ ] Commit `docs(server): record the typed extension ingress identity in RFC 0018 and the runbook (#1753)`.

## Out of scope (state in PR)

- Extension groupchat to remote-owned rooms (typed refusal; relaying would change `IngressRelayAdmission` → `deliver_ordered.v11`).
- Operator UI for grants; only config-driven sync, runtime `revoke_plugin` and admission semantics ship.
- Ad-hoc IQ commands (not message ingress). #1660 opaque delivery keys for extension effects (kind 10). Live-order limitation (#1770) unchanged.
