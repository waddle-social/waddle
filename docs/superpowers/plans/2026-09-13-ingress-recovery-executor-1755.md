# Ingress Recovery Executor (#1755) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a bounded recovery phase to the periodic ingress maintenance pass that re-executes recorded-but-unreceipted post-commit obligations purely from the canonical envelope, so an obligation that lost its Phase C execution completes without depending on a client retransmit, with receipts recorded exactly once.

**Architecture:** The maintenance pass (`ingress/maintenance.rs`) gains a `recovery` phase between terminalization and retention GC. It pages non-terminal canonical rows older than the grace period that still have at least one unreceipted intent of a *recoverable kind*, freezes `(envelope, recorded intents, unreceipted intents, route progress)` under the canonical lock, releases the lock, rebuilds a synthetic `IngressDecision` with a pure per-family builder that reuses the existing restorers (`restore_recorded_offline_deliveries`, `restore_room_observer_envelope`, `restore_recorded_dm_pin_effects`, `restore_recorded_muc_decline`, `delivery_message`), and executes it through the unchanged `execute_effects` / `execute_uow` arms and `settle_recorded` contract. Actor/registry dependencies (`Deps`) come from a late-bound `RecoveryEnvironment` trait object (implemented by `WebSocketState`) because the authority is constructed before the websocket state. Recovery never invents audience or payload: only recorded intents are rebuilt, and a direct route is rebuilt only when the recorded evidence proves the canonical message is its payload. Families that cannot be rebuilt from `(envelope, intent)` alone, or whose execution is not idempotent, are metered as unrecoverable and left pending.

**Tech Stack:** Rust (waddle-server, waddle-xmpp), tokio, sqlx-backed `Database`/`IngressUnitOfWork`, OpenTelemetry counters via `waddle_xmpp::telemetry`, SQLite + Postgres regression tests via `IngressFixture`.

**Spec:** GitHub issue #1755 (body reproduced in PR #1775 description); RFC 0018 `server/docs/rfcs/0018-ingress-authority-cutover.md` (stated limitation (i), §"Settlement contract (#1752)"). Plan review round 1 (gpt-6-astra, high): REVISE — findings 1–5 blocking, 6–9 should-fix; all addressed below and marked **[R1-n]**.

## Global Constraints

- Clippy runs with `-D warnings` (`cargo clippy --all-targets --all-features -- -D warnings` inside `server/`); no new `#[allow]`.
- Typed payloads everywhere: no `String`/`&str` carrying protocol data on events, traits, or public structs. Receipt kinds come from `EffectReceiptKind`/`IngressEffectKind`, never integer literals at call sites.
- No XML via `format!`; only typed builders.
- Settlement contract (RFC 0018): effects whose completion evidence is a DB row commit evidence + receipts in ONE transaction under the canonical row lock; no actor/registry/extension/socket call while a transaction is open.
- Recovery must never invent audience or payload: only recorded intents are executed; recorded wins over current policy/audience; a route whose payload provenance is ambiguous stays pending.
- Every implemented behaviour has SQLite + Postgres regressions (Postgres tests skip when `WADDLE_TEST_POSTGRES_URL` is unset). Local Postgres for this work: `postgres://waddle_test@127.0.0.1:55435/waddle_test`.
- Zero-registration rule: every new counter/label combination is emitted with count 0 at startup (`register_reliability_counters`).
- Verify with `cargo nextest run` (CI uses nextest; in-process `cargo test` cross-talks on metric guards).
- Commit messages: Conventional Commits with a single scope, e.g. `feat(server): ...`. Do not merge PRs.

## Family coverage decided by this plan

| Recorded intent family | Rebuilt from `(envelope, intent)`? | Execution path |
|---|---|---|
| `RouteDirect` for a `Chat`/`Normal` envelope (headline routes use peer delivery with recipient archival for `<store/>`, `route_to_connection.rs:670/802` → deferred **[R3-1]**) whose `recipient == message.to.to_bare()`, whose identity is not DM-pin-owned (`StanzaId` identities are owned by the pin restorer whenever a `DmPinMutation` is recorded **[R3-2]**), **and** whose recipient preparation is proven to have committed in Phase B (the route is *not* a delegated live full-JID route per `commit.rs::retain_live_recipient_plan`: target is bare, or recipient == sender, or a recipient `ArchiveAuthoritative` is recorded, or the route is not `{fanout == [full], CaptureOrdinal}`) **[R1-1][R1-2][R2-2]** | Yes → `QueueDetached` with `delivery_message(envelope, recipient, intents)` (direct frame; recipient archive/inbox already committed) | `execute_uow::detached` arm (route_progress owned); SM append ledger dedupes detached appends |
| `RouteDirect` delegated live full-JID route (full target, recipient ≠ sender, no recorded recipient archive, `fanout == [full]`, `CaptureOrdinal`) | **Deferred** — recipient preparation (archive, inbox, carbons) ran at the destination connection and cannot be re-run from the envelope without a live recipient pipeline **[R2-2]**. This conservatively also defers a prepared-but-unarchived case: an originally detached full-target `<no-store/>` message (`route_to_connection.rs:454`, `archive.rs:123`) has no archive evidence and is left pending **[R3-6]** | unrecoverable (`route_direct`), stated limitation |
| `RouteDirect` on a `Headline` envelope | Deferred (see above) **[R3-1]** | unrecoverable (`route_direct`) |
| `RouteDirect` on a groupchat envelope (synthetic inbox push, `groupchat_archive.rs:974`) or any other recipient | No — payload is not the canonical message (`PushInboxUpdate` needs the Phase B inbox outcome) | unrecoverable (`route_direct`) |
| `PendingDelivery` + `NotificationActivityPreview{NotificationCandidate\|OfflineDelivery}` for direct conversations | Yes — `restore_recorded_offline_deliveries` | `execute_uow::offline` arm (re-lock, recheck, settle atomically) |
| `RoomObserver` | Yes — `restore_room_observer_envelope` (requires observer envelope; otherwise unrecoverable) | `execute_observers` (generic; receipt per plugin) |
| `GroupchatNotificationRecovery{Completed\|DeferredPolicy}` + room `NotificationCandidate` | Delegated to `reconcile_groupchat_notification_recovery` (renamed `reconcile_recovery`, groupchat_inbox.rs) | authority `prepare_/settle_notification_recovery` (re-locks and revalidates) |
| `DmPinMutation` + its `RouteDirect{StanzaId}` routes | **Route-only**: `restore_recorded_dm_pin_effects`, then drop every `DmPinMutation` effect. For a **receipted** mutation, strip its `AfterDmPinMutation` dependency from the surviving routes (the receipt is the completion evidence) and recover the routes. For an **unreceipted** mutation, drop its dependent routes too and report `DmPinMutation` unrecoverable: the mutation may already have been applied with a failed receipt write, and a later unpin must not be undone by replaying it **[R1-4][R2-3]** | detached arm for routes only |
| `MucInviteLedger{Claimed}` decline | Yes — `restore_recorded_muc_decline`, with `InviteLedgerMutation::Claim.message_key` bound to the canonical key as `commit.rs:513-520` does **[R1-5]** | generic `InviteLedger` + generic `RouteToPeer` (specialized invitation routes stay generic, `execute_uow.rs:59`) |
| `Carbons`, `DmCallThreadState` | Rebuildable (`rejection.rs`, `recorded::restore_dm_call_state`) but **deliberately deferred**: their sinks have no idempotency key and `execute.rs` only skips them when already receipted **[R1-9]** | unrecoverable (documented as deferred) |
| `RelayCarbons`, `RouteMucGroupchat`, `RouteMucSystemBroadcast`, `RouteOccupantPm`, `DispatchToRoomRemote`, `Pin` (room), `SystemMessageArchive`, `GroupDmMembershipGrant`/`GroupDmInviteLedger`, `MucInviteMembershipGrant`, `RoomSubjectMutation`, `LinkPreviewMediaRef`, `CallSignal`, `Extension`, `TombstoneReplayDeletion`, `ErrorReply` | No (needs today's actor handle, the reflected payload, the pinner nick, or a sender socket) | unrecoverable |
| `InboxProject`, `ArchiveAuthoritative`, `RetractionTombstone` | Receipted inside Phase B; unreceipted only on contradiction | unrecoverable, never re-applied |

Remote-owned resources reachable only through `RelayFullJid` owner routing are **not** recovered by this executor: `deliver_direct_to_full_with_registered_remote` covers registered remote sockets and local registries only, so such appends return `Unavailable` and the obligation stays pending **[R1-6]**. This is recorded as a stated limitation in the RFC and runbook.

**Delivery guarantee statement [R1-3][R2-6].** Receipts are exactly-once (arms re-lock the canonical row and re-check receipts before `settle_recorded`; generic receipts are idempotent inserts). Side effects are exactly-once where a durable idempotency key exists: pending rows (`PendingReceiptRepository::contains`), notification candidates (unique insert), detached SM appends (`sm_ingress_appends` keyed by obligation+resource), recovery completion (`RecoveryReceiptRepository::complete`). Side effects are **at-least-once** where no key exists: a live socket send (`TrySendDirect`, `RegistryFrame`) and a plugin observer invocation can repeat whenever an execution succeeded but its receipt/progress persistence failed (crash between send and receipt, receipt timeout), and can repeat across recovery attempts or against a concurrent client retransmission. Recovery does not widen the set of keyless sinks; it adds attempts. The runbook states this per family. A durable per-obligation send lease (concurrency control) plus keyed live sends (send-before-receipt crashes) are filed as one follow-up issue; neither is claimed here.

---

### Task 1: Substrate query for rows with unreceipted recoverable intents

**Files:**
- Modify: `server/crates/waddle-xmpp/src/ingress/effect_intent.rs` — add `impl IngressEffectKind { pub const fn storage_tag(self) -> i32 }` mirroring `StoredEffectIntent::kind` (shared codes: `ArchiveAuthoritative`=0, `RouteMucGroupchat`=2 for both MUC route variants), plus a test asserting `intent.kind().storage_tag() == intent.with_encoded_v1(|kind, _| kind)` for one intent per variant.
- Modify: `server/crates/waddle-server/src/ingress_substrate/maintenance.rs`
- Modify: `server/crates/waddle-server/src/ingress_substrate/mod.rs` (re-export)
- Test: `server/crates/waddle-server/src/ingress/maintenance_tests.rs`

**Interfaces:**
- Produces: `pub async fn unreceipted_nonterminal_keys(tx: &mut Transaction<'_>, after: Option<(DateTime<Utc>, MessageKey)>, older_than: DateTime<Utc>, kinds: &[EffectReceiptKind], limit: u32) -> Result<Vec<(DateTime<Utc>, MessageKey)>, IngressSubstrateError>` — same keyset pagination and SQLite cutoff rounding as `receipt_complete_nonterminal_keys`; selects rows where at least one intent **whose kind is in `kinds`** has no receipt. `kinds` is rendered as a bound `IN (?, ?, ...)` list (never string-formatted values); an empty list returns an empty page without querying. **[R1-7]**
- Produces: `pub(crate) const RECOVERABLE_KINDS: [IngressEffectKind; 7] = [RouteDirect, NotificationActivityPreview, DmPinMutation, MucInviteLedger, GroupchatNotificationRecovery, PendingDelivery, RoomObserver]` in `ingress/recovery_rebuild.rs` (Task 4) — Task 5 maps it through `EffectReceiptKind::from_storage(kind.storage_tag())`.

- [ ] **Step 1: Failing test** (append to `maintenance_tests.rs`)

```rust
async fn unreceipted_page_selects_only_recoverable_pending_rows(fixture: IngressFixture) {
    let complete = interrupted_delivery(&fixture, "recovery-page-complete").await;
    let mut pending = fixture.submission(Some("recovery-page-pending"), "pending");
    pending.plan.intents.push(IngressEffectIntent::RouteDirect {
        recipient: "juliet@example.com".parse().expect("recipient"),
        fanout: vec!["juliet@example.com/phone".parse().expect("resource")],
        route_identity: EffectMessageIdentity::capture_ordinal(0),
    });
    let pending_key = commit_submission(&fixture.uow, &pending, 5).await.expect("pending commit").message_key.expect("key");
    let mut carbons = fixture.submission(Some("recovery-page-carbons"), "carbons only");
    carbons.plan.intents.push(IngressEffectIntent::Carbons {
        carbon_recipients: vec!["romeo@example.com/laptop".parse().expect("carbon")],
        excluded_source: "romeo@example.com/phone".parse().expect("source"),
        kind: waddle_xmpp::ingress::CarbonKind::Sent,
    });
    let carbons_key = commit_submission(&fixture.uow, &carbons, 5).await.expect("carbons commit").message_key.expect("key");
    let route_kind = crate::ingress_substrate::EffectReceiptKind::from_storage(
        waddle_xmpp::ingress::IngressEffectKind::RouteDirect.storage_tag(),
    );
    let mut tx = fixture.db.begin().await.expect("scan transaction");
    let page = crate::ingress_substrate::unreceipted_nonterminal_keys(
        &mut tx, None, chrono::Utc::now() + chrono::Duration::seconds(1), &[route_kind], 16,
    ).await.expect("unreceipted page");
    let empty = crate::ingress_substrate::unreceipted_nonterminal_keys(
        &mut tx, None, chrono::Utc::now() + chrono::Duration::seconds(1), &[], 16,
    ).await.expect("empty kinds");
    tx.commit().await.expect("scan commit");
    let keys: Vec<_> = page.into_iter().map(|(_, key)| key).collect();
    assert_eq!(keys, vec![pending_key]);
    assert!(!keys.contains(&complete) && !keys.contains(&carbons_key));
    assert!(empty.is_empty());
    fixture.close().await;
}
// sqlite_/postgres_ wrappers as elsewhere in the file (`IngressFixture::postgres("recovery_page")`).
```

Note `CarbonKind` variant name: check `waddle_xmpp::ingress::CarbonKind` before using `Sent`.

- [ ] **Step 2: Run** `cd server && cargo nextest run -p waddle-server --all-features --lib -E 'test(ingress::maintenance::tests::sqlite_unreceipted_page)'` → compile failure.
- [ ] **Step 3: Implement** `storage_tag`, the query (factor shared cutoff/param/row-decoding into a private `page_nonterminal_keys` helper used by both public functions; the SQLite dialect binds the same `IN` list), and the re-export.
- [ ] **Step 4: Run** the test on SQLite and with `WADDLE_TEST_POSTGRES_URL` on Postgres. Expected: PASS.
- [ ] **Step 5: Commit** `feat(server): page non-terminal rows with unreceipted recoverable ingress intents`.

---

### Task 2: Telemetry — recovery phase and obligation counters

**Files:**
- Modify: `server/crates/waddle-xmpp/src/telemetry/attributes.rs`
- Modify: `server/crates/waddle-xmpp/src/telemetry/reliability.rs`
- Modify: `server/crates/waddle-xmpp/src/ingress/effect_intent.rs` (`IngressEffectKind::ALL`)
- Check: `rg -n "ingress.maintenance" infrastructure/ server/docs` for lists of metric names to extend (rules README, dashboards).

**Interfaces:**
- `IngressMaintenancePhase::Recovery` (value `"recovery"`, `ALL` → 4 entries).
- `impl MetricAttribute for IngressEffectKind` with `key() == "kind"` and values equal to `IngressEffectIntent::storage_kind_names()` names for the same storage tag (`route_direct`, `route_muc` for both MUC variants, `archive` for both archive variants, ...). Add `pub const ALL: [Self; 26]`. Add a test asserting every `ALL` member's `value()` equals the `storage_kind_names()` entry for `storage_tag()`.
- `pub fn increment_ingress_maintenance_recovered_obligations(count: u64)` → counter `ingress.maintenance.recovered_obligations`.
- `pub fn increment_ingress_maintenance_unrecoverable_obligations(count: u64, kind: IngressEffectKind)` → counter `ingress.maintenance.unrecoverable_obligations{kind}`. Both zero-registered in `register_reliability_counters`.

- [ ] **Step 1: Failing test** in `reliability.rs` tests (next to `ingress_maintenance_helpers_emit_with_typed_labels`):

```rust
#[tokio::test]
async fn ingress_recovery_helpers_emit_with_typed_labels() {
    let guard = setup().await;
    register_reliability_counters();
    increment_ingress_maintenance_run(IngressMaintenancePhase::Recovery, IngressMaintenanceOutcome::Complete);
    increment_ingress_maintenance_recovered_obligations(2);
    increment_ingress_maintenance_unrecoverable_obligations(1, crate::ingress::IngressEffectKind::Carbons);
    assert_eq!(guard.counter_sum("ingress.maintenance.runs", &[("phase", "recovery"), ("outcome", "complete")]), Some(1));
    assert_eq!(guard.counter_sum("ingress.maintenance.recovered_obligations", &[]), Some(2));
    assert_eq!(guard.counter_sum("ingress.maintenance.unrecoverable_obligations", &[("kind", "carbons")]), Some(1));
    for kind in crate::ingress::IngressEffectKind::ALL {
        assert!(guard.counter_sum("ingress.maintenance.unrecoverable_obligations", &[("kind", kind.value())]).is_some(), "{kind:?} zero-registered");
    }
}
```

- [ ] **Step 2: Run** `cargo nextest run -p waddle-xmpp --lib -E 'test(telemetry::reliability::tests::ingress_recovery_helpers)'` → compile failure.
- [ ] **Step 3: Implement.** - [ ] **Step 4: Run** `cargo nextest run -p waddle-xmpp --lib -E 'test(telemetry::)'` → PASS.
- [ ] **Step 5: Commit** `feat(server): add ingress recovery maintenance phase and obligation counters`.

---

### Task 3: `RecoveryEnvironment` trait, late binding, and coordinator plumbing

**Files:**
- Create: `server/crates/waddle-server/src/ingress/recovery_environment.rs`
- Modify: `server/crates/waddle-server/src/ingress/mod.rs` (module decl, `pub use RecoveryEnvironment`, `IngressAuthority.recovery: RecoveryBinding`, `bind_recovery_environment`)
- Modify: `server/crates/waddle-server/src/ingress/gc.rs` (`RetentionGcCoordinator::new(database, uow, binding)`; the `run` closure passes `binding.environment()` to the pass)
- Modify: `server/crates/waddle-server/src/ingress/maintenance.rs` (add `environment: Option<Arc<dyn RecoveryEnvironment>>` parameter to `run_maintenance_pass` and `run_maintenance_pass_with_cursor`; body ignores it until Task 5)
- Modify: `server/crates/waddle-server/src/server/routes/websocket/interpret_loop.rs` (`impl RecoveryEnvironment for WebSocketState`)
- Modify: `server/crates/waddle-server/src/server/http.rs` — after the `Arc<WebSocketState>` is built in `create_websocket_state` (search `ingress,` near line 1098), call `state.deps.protocol.ingress.bind_recovery_environment(Arc::downgrade(&state) as Weak<dyn RecoveryEnvironment>)`.
- Update every `run_maintenance_pass*` call in `gc.rs` tests and `maintenance_tests.rs` to pass `None`.

**Interfaces:**
```rust
pub trait RecoveryEnvironment: Send + Sync {
    fn recovery_deps(&self) -> crate::server::routes::interpret::Deps<'_>;
}
#[derive(Clone, Default)]
pub(crate) struct RecoveryBinding { environment: Arc<std::sync::Mutex<Option<Weak<dyn RecoveryEnvironment>>>> }
impl RecoveryBinding {
    pub(crate) fn bind(&self, environment: Weak<dyn RecoveryEnvironment>);
    pub(crate) fn environment(&self) -> Option<Arc<dyn RecoveryEnvironment>>; // None before binding / after drop
}
impl IngressAuthority { pub fn bind_recovery_environment(&self, environment: Weak<dyn RecoveryEnvironment>); }
impl RecoveryEnvironment for WebSocketState { fn recovery_deps(&self) -> Deps<'_> { build_interpret_deps(self, None) } }
```

- [ ] **Step 1: Failing test** in `recovery_environment.rs`: `binding_upgrades_only_while_the_environment_lives` (bind a `Fixture(ConnectionRegistry)` env, assert `environment()` is `Some`, drop, assert `None`).
- [ ] **Step 2: Run** → compile failure. - [ ] **Step 3: Implement** all files above.
- [ ] **Step 4: Run** `cargo nextest run -p waddle-server --all-features --lib -E 'test(ingress::recovery_environment) | test(ingress::maintenance) | test(ingress::gc)'` and clippy. → PASS.
- [ ] **Step 5: Commit** `feat(server): late-bind recovery execution dependencies into ingress maintenance`.

---

### Task 4: Pure recovery rebuild from `(envelope, intents)`

**Files:**
- Create: `server/crates/waddle-server/src/ingress/recovery_rebuild.rs`
- Create: `server/crates/waddle-server/src/ingress/recovery_rebuild_tests.rs`
- Modify: `server/crates/waddle-server/src/ingress/mod.rs` (`mod recovery_rebuild;`)
- Modify: `server/crates/waddle-server/src/ingress/recorded.rs` (`pub(super) fn delivery_message`)
- Modify: `server/crates/waddle-server/src/ingress/decision.rs` — add `pub(super) fn assemble_receipts(external: &[ExternalEffect], intents: &[IngressEffectIntent], route_progress: &[RouteProgress]) -> Result<(Vec<Vec<EffectReceiptKey>>, Vec<EffectReceiptKey>), IngressUowError>` extracted verbatim from `commit.rs:521-546`, and `pub(super) fn bind_claim_keys(external: &mut [ExternalEffect], key: MessageKey)` extracted from `commit.rs:513-520`; `commit.rs` calls both (behaviour-preserving refactor). **[R1-5][R1-10]**
- Modify: `server/crates/waddle-server/src/server/routes/interpret/groupchat_inbox.rs` — rename private `reconcile_recovery` to `pub(crate) async fn reconcile_groupchat_notification_recovery(state: &WebSocketState, recovery: &GroupchatNotificationRecovery) -> Result<RecoverySweepOutcome, IngressUowError>`; re-export via `server/routes/interpret.rs`.

**Interfaces:**
```rust
pub(crate) const RECOVERABLE_KINDS: [IngressEffectKind; 7] = [ /* see Task 1 */ ];

pub(super) struct RecoveryInput<'a> {
    pub key: MessageKey,
    pub envelope: &'a MessageEnvelope,
    pub created_at: DateTime<Utc>,
    pub recorded: &'a [IngressEffectIntent],
    pub unreceipted: &'a [IngressEffectIntent],
    pub route_progress: Vec<RouteProgress>,
}
pub(super) struct RebuiltRecovery {
    pub decision: IngressDecision,                 // external may be empty
    pub delegated: Vec<GroupchatNotificationRecovery>,
    pub unrecoverable: Vec<IngressEffectKind>,     // deduplicated
}
pub(super) fn rebuild(input: RecoveryInput<'_>) -> Result<RebuiltRecovery, IngressUowError>;
```

Algorithm:
1. `plan = IngressPlan { failure: None, rejection: None, plan: Vec::new(), intents: recorded.to_vec(), sanitized_message: envelope.message().clone(), error_reply: None, room_execution: RoomExecutionPath::None }`.
2. `dm_pin::restore_recorded_dm_pin_effects(&mut plan, recorded, envelope)?`. Then **[R1-4][R2-3]**: for every `ExternalEffect::DmPinMutation(m)` remove the effect. If its recorded `DmPinMutation` intent is *not* in `unreceipted`, remove every `PlanEffectDependency::AfterDmPinMutation { pair, target }` with `pair == m.pair && target == m.target_stanza_id` from the remaining planned effects (the receipt is the completion evidence). If it *is* unreceipted, also remove every planned effect carrying that dependency and push `IngressEffectKind::DmPinMutation` to `unrecoverable`.
3. `muc_direct::restore_recorded_muc_decline(&mut plan, recorded, unreceipted, envelope)?`.
4. If any recorded `PendingDelivery`: `restore_offline::restore_recorded_offline_deliveries(&mut plan, recorded, unreceipted, envelope, created_at)`.
5. If any recorded `RoomObserver`: `recorded::restore_room_observer_envelope(&mut plan, recorded, envelope)`; on `Err(IngressUowError::EffectIntentMessageMissing)` push `IngressEffectKind::RoomObserver` to `unrecoverable` instead of failing.
6. Direct routes **[R1-1][R1-2][R2-2][R3-1][R3-2]**: for each unreceipted `RouteDirect { recipient, fanout, route_identity }` with non-empty `fanout`, **not DM-pin-owned** (skip every `RouteDirect { route_identity: EffectMessageIdentity::StanzaId(_) }` when `recorded` contains any `DmPinMutation`; the pin restorer owns those, including the ones §2 removed), not already covered by an existing plan effect (`recorded::recorded_route_obligation(&plan.intents, effect)` for some effect with the same recipient and identity), and **provenance-proven**: `matches!(envelope.message().type_, MessageType::Chat | MessageType::Normal)` and `envelope.message().to.as_ref().map(Jid::to_bare) == Some(recipient.clone())` and no recorded invitation/grant intent (`restore_offline::specialized_invitation`) and `recipient` not in a recorded `PendingDelivery` audience (`in_recorded_offline_audience` semantics) and **not a delegated live route**: extract the predicate at `commit.rs:621-633` into `pub(super) fn live_recipient_delegated(target: &NormalizedTarget-or-&Jid, sender: &BareJid, recorded: &[IngressEffectIntent]) -> bool` (true when the target is a full JID, `recipient != sender`, no recorded `ArchiveAuthoritative { archive == recipient }`, and a recorded `RouteDirect { recipient, fanout == [full], route_identity: CaptureOrdinal(_) }` exists) and call it with `envelope.message().to` and `envelope.message().from.to_bare()`; `commit.rs` keeps calling the same function.
   - Proven → push `ExternalDeliveryEffect::QueueDetached { route_identity: Some(id), call_setup: None, bare: recipient, resources: fanout, stanza: Box::new(Stanza::Message(delivery_message(envelope, recipient, recorded))) }`.
   - Failing provenance or delegated → `unrecoverable.push(IngressEffectKind::RouteDirect)`.
7. Groupchat notification recovery: for each unreceipted `GroupchatNotificationRecovery { mutation }` with `action ∈ {Completed, DeferredPolicy}` push `GroupchatNotificationRecovery { message_key: key, key: GroupchatNotificationRecoveryKey { recipient, room, thread_id: mutation.thread_id.as_ref().map(|t| t.as_str().to_owned()), archive_stanza_id }, sender_jid: mutation.sender.clone(), is_live_occupant, room_members_only, sender_can_broadcast_channel_mention, created_at_ms }` into `delegated` (dedupe by `key`). Room `NotificationActivityPreview::NotificationCandidate` intents (owner != conversation) are covered when a delegated row with the same `(recipient, room, archive_stanza_id)` exists; otherwise unrecoverable.
8. `external = suppression::filter_external_effects(&plan, &ReconcileVerdict::Consistent, &[], unreceipted, &route_progress)`; `external_dependencies` from `suppression::external_effect_indices(...)` mapped to `plan.plan[index].dependencies.clone()` (as `commit.rs:489-498`).
9. `decision::bind_claim_keys(&mut external, key)`; `(external_receipts, arm_owned_receipts) = decision::assemble_receipts(&external, recorded, &route_progress)?`.
10. Any unreceipted intent whose receipt key appears in no `external_receipts[i]` and is not delegated → `unrecoverable.push(intent.kind())` (dedupe).
11. `decision = IngressDecision { class: IngressDecisionClass::ExistingRepaired, message_key: Some(key), ordinal: None, alias: AliasOutcomeClass::Existing, verdict: None, archive_ids: <recorded ArchiveAuthoritative/SystemMessageArchive>, applied_durable: Arc::default(), external_dependencies, external, external_receipts, arm_owned_receipts, route_progress, receipts_pending: unreceipted.iter().map(receipt_key).collect::<Result<_, _>>()? }`.

- [ ] **Step 1: Failing tests** in `recovery_rebuild_tests.rs` (pure; helpers `envelope(body)`, `route_intent(resources)`, `progress_for(&intent)`):
  - `unreceipted_bare_target_route_rebuilds_a_detached_fanout_owned_by_the_arm` (asserts `QueueDetached`, recipient archive stanza-id present in stanza, `external_receipts[0] == [receipt]`, `arm_owned_receipts == [receipt]`, `receipts_pending == [receipt]`, `unrecoverable` empty).
  - `delegated_live_full_target_route_is_deferred` (envelope `to = juliet@example.com/phone`, no recipient `ArchiveAuthoritative`, recorded `RouteDirect { fanout: [phone], CaptureOrdinal }` → external empty, `unrecoverable == [RouteDirect]`). **[R2-2]**
  - `full_target_with_recorded_recipient_archive_rebuilds_direct_frame` (→ `QueueDetached`).
  - `full_target_multi_resource_route_is_not_delegated` (recorded fanout has two resources → `QueueDetached`, matching `retain_live_recipient_plan`'s `fanout == [full]` guard).
  - `groupchat_route_direct_is_unrecoverable` (envelope type groupchat, `RouteDirect` for a member → external empty, `unrecoverable == [RouteDirect]`). **[R1-1]**
  - `headline_route_is_deferred` (envelope type headline with `<store/>`, bare target → external empty, `unrecoverable == [RouteDirect]`). **[R3-1]**
  - `pin_owned_stanza_id_routes_are_never_generically_rebuilt` (recorded unreceipted `DmPinMutation` + two `RouteDirect{StanzaId}` fanouts, one per participant → external empty, `unrecoverable == [DmPinMutation]`, no `QueueDetached`). **[R3-2]**
  - `route_to_a_recipient_other_than_the_target_is_unrecoverable`.
  - `receipted_route_is_not_rebuilt`; `partially_completed_fanout_keeps_only_remaining_resources`.
  - `pending_delivery_with_candidate_rebuilds_offline_row` (one `QueueOfflineDelivery` with `Prepared`, three receipts mapped).
  - `room_observer_without_observer_envelope_is_unrecoverable`; `room_observer_with_envelope_rebuilds_observe_effect`.
  - `groupchat_notification_recovery_is_delegated_not_executed`.
  - `carbons_are_reported_unrecoverable`.
  - `unreceipted_dm_pin_mutation_and_its_routes_are_deferred` (all unreceipted → external empty, `unrecoverable == [DmPinMutation]`). **[R2-3]**
  - `receipted_dm_pin_mutation_is_not_replayed_but_its_routes_run` (mutation receipted, one route unreceipted → no `DmPinMutation` effect, `RouteToPeer` per resource present with **no** `AfterDmPinMutation` dependency, receipts owned by the arm via route_progress). **[R1-4]**
  - `muc_decline_claim_is_bound_to_the_canonical_key` (`InviteLedgerMutation::Claim { message_key: Some(key) }`). **[R1-5]**
- [ ] **Step 2: Run** `cargo nextest run -p waddle-server --all-features --lib -E 'test(ingress::recovery_rebuild)'` → compile failure.
- [ ] **Step 3: Implement** `recovery_rebuild.rs`, the two `decision.rs` extractions, the `groupchat_inbox.rs` rename.
- [ ] **Step 4: Run** rebuild tests + `-E 'test(ingress::commit)'` (refactor safety) → PASS.
- [ ] **Step 5: Commit** `feat(server): rebuild unresolved ingress obligations from the canonical envelope`.

---

### Task 5: Recovery phase driver in the maintenance pass

**Files:**
- Create: `server/crates/waddle-server/src/ingress/recovery_executor.rs`
- Modify: `server/crates/waddle-server/src/ingress/maintenance.rs`
- Modify: `server/crates/waddle-server/src/ingress/mod.rs` (`mod recovery_executor;`)
- Modify: `server/crates/waddle-server/src/ingress_uow/repositories.rs` — add `CanonicalMessageRepository::is_terminal(tx, key) -> Result<bool, IngressUowError>` if no accessor exists.

**Interfaces:**
```rust
// maintenance.rs
pub(crate) struct MaintenanceBudget {
    pub(crate) terminalization: Duration,       // 2s
    pub(crate) recovery: Duration,              // NEW 4s phase budget
    pub(crate) recovery_row: Duration,          // NEW 1s absolute per-row deadline (freeze + execute + delegate + recount)  [R1-7]
    pub(crate) recovery_page_size: u32,         // NEW 64 keys per scan page
    pub(crate) recovery_max_attempts: u32,      // NEW 64 attempted rows per pass (skipped unsupported rows do not count)
    pub(crate) retention: RetentionGcBudget,
    pub(crate) hard_deadline: Duration,         // 9s → 13s
    pub(crate) page_size: u32, pub(crate) max_pages: u32, pub(crate) grace: chrono::Duration,
}
pub(super) struct MaintenanceCursor { after: ..., recovery_after: Arc<Mutex<Option<MaintenancePosition>>> }

// recovery_executor.rs
pub(super) enum RowRecovery {
    Vanished,                                                   // row gone/terminal when locked
    NothingPending,                                             // all receipts present; terminalization owns it
    Executed { recovered: u64, unrecoverable: Vec<IngressEffectKind>, terminal: bool },
}
pub(super) async fn recover_row(database: &Database, uow: &IngressUnitOfWork, deps: &Deps<'_>, key: MessageKey, deadline: tokio::time::Instant) -> Result<RowRecovery, IngressUowError>;
```

`recover_row` (everything below runs inside `tokio::time::timeout_at(deadline, ..)` owned by the caller; the remaining time is passed to `execute_effects` as its budget):
1. `tx = uow.begin_with_timeouts(100ms, 250ms)`; `if !CanonicalMessageRepository::lock(&mut tx, key)` → commit, `Vanished`. Load `envelope` (`EffectIntentMessageMissing` if `None`), `created_at`, `recorded = EffectIntentRepository::load`, `unreceipted` (loop as `commit.rs:261-274`), `route_progress` for unreceipted `RouteDirect` (as `commit.rs:420-450`). If `unreceipted.is_empty()` → commit, `NothingPending`. `tx.commit()` — **the lock is released before any execution or actor call**. `#[cfg(test)] test_hooks::after_recovery_freeze(key).await` runs here (gate pattern of `pause_before_terminalization`) so tests can interpose between the freeze and execution **[R3-5]**.
2. `rebuilt = recovery_rebuild::rebuild(...)?`.
3. If `!rebuilt.decision.external.is_empty()`: `execute::execute_effects(uow, database, &rebuilt.decision, &ImmediateSink, deps, deadline.saturating_duration_since(Instant::now()))` (frame obligations, if any, stay unresolved: recovery has no writer; `ExecutionReport::drop` meters them).
4. For each `delegated` row: `if let Some(state) = deps.web_socket_state { reconcile_groupchat_notification_recovery(state, &row).await }` (debug log otherwise).
5. Recount in one plain transaction (no lock): `recovered = receipts_pending.iter().filter(contains).count()`, `terminal = CanonicalMessageRepository::is_terminal`.
6. `Executed { recovered, unrecoverable: rebuilt.unrecoverable, terminal }`.

`maintenance.rs::recover_candidates(database, uow, budget, cursor, environment: &dyn RecoveryEnvironment)` mirrors `terminalize_candidates`: `deps = environment.recovery_deps()` once per phase; pages with `unreceipted_nonterminal_keys(.., &recoverable_receipt_kinds(), budget.recovery_page_size)` where `recoverable_receipt_kinds()` maps `RECOVERABLE_KINDS` through `storage_tag`; persists `recovery_after` before each row; runs each row under `timeout_at(now + budget.recovery_row)`; adds `recovered` to the recovered counter; meters each `unrecoverable` kind once per row.

**Unsupported-row exclusion [R1-7][R2-5][R3-3][R3-4].** The SQL kind filter cannot express provenance, so the scan also returns per-row evidence counts and the cursor remembers unsupported evaluations keyed by that evidence:
- Task 5 extends the Task 1 query to `unreceipted_nonterminal_candidates(..) -> Vec<RecoveryCandidate { created_at, key, evidence: RecoveryEvidence { intents: u32, receipts: u32 } }>` where the counts are `(SELECT count(*) FROM ingress_effect_intents i WHERE i.message_key = m.message_key)` and the same for `ingress_effect_receipts` (both indexed by `message_key`). Any change to the recorded intents or receipts of a row changes at least one count.
- `MaintenanceCursor.recovery_unsupported: Arc<Mutex<UnsupportedRows>>` is a bounded (4096 entries, FIFO eviction) process-local map `MessageKey -> RecoveryEvidence` of rows whose last attempt produced **no executable effect and no delegation** (`rebuild` returned empty `external` and empty `delegated`). A candidate is skipped only when its current evidence equals the cached evidence; different evidence (a foreground duplicate receipted a pin mutation, reconciliation added intents) re-evaluates the row and replaces the entry. Eviction or restart only costs a re-evaluation; the cache can never suppress work whose evidence changed, so it is an efficiency device, not a correctness one.
- Paging runs until the SQL scan reaches its tail, `budget.recovery_max_attempts` (64) rows have been *attempted* (skipped rows do not count), or the phase deadline fires; `recovery_page_size` (64) is the scan page.

**Outcome semantics:** `Err` from the scan → `failure_outcome(&error)`; `Err`/timeout from `recover_row` → `Partial` (row deferred, like a contended terminalization); a row that executed but did not become terminal is **not** partial (retried next tick); attempt budget exhausted before the tail → `Partial`; a resumed cursor reaching the tail → `Partial` once for wraparound (same as terminalization). `Complete` means **the evaluation sweep reached the tail within this pass's attempt budget**, not that no unsupported work remains: unsupported rows stay visible through `ingress.maintenance.unrecoverable_obligations{kind}` and the CNPG backlog gauge, and the runbook says so. A backlog of N unsupported rows therefore costs ⌈N/64⌉ `Partial` passes (with the coordinator's 1→30 s backoff) once, then `Complete` passes with zero attempts until evidence changes. `run_maintenance_pass_with_cursor` runs recovery after terminalization under `tokio::time::timeout(budget.recovery, ..)`, records `IngressMaintenancePhase::Recovery`, and folds it into `combine`. When `environment` is `None` the phase is skipped and unrecorded (pre-binding startup pass; existing tests unchanged).

- [ ] **Step 1: Failing test** (append to `maintenance_tests.rs`) `recovery_phase_completes_a_lost_detached_route`: real Phase B commit with a `RouteDirect` intent and a `QueueDetached` effect for one resource stored as a detached SM session (copy `store_detached` + `DatabaseSmPersistence` + `InMemorySmSessionRegistry` from `execute_detached_fault_tests.rs:20-70`); **no** Phase C; `backdate_created(.., 120)`; environment = `StateEnvironment(Arc<WebSocketState>)` built with `create_test_websocket_state_with_db_pool_and_ingress(pool_from_fixture, Arc::new(fixture.authority().await))` plus `TestStateOverrides { sm_session_registry: Some(sm) }` (see `groupchat_recovery_tests.rs::state_for`). Assertions: unbound pass → `Complete`, `append_count == 0`, non-terminal; bound pass → `Complete`, `append_count == 1`, one receipt, terminal; third pass → `append_count` still 1. **[R1-8]**
- [ ] **Step 2: Run** → compile failure. - [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** `-E 'test(ingress::maintenance)'` on SQLite + Postgres, and clippy → PASS.
- [ ] **Step 5: Commit** `feat(server): execute unresolved ingress obligations in the maintenance recovery phase`.

---

### Task 6: Regression suite per effect family (SQLite + Postgres)

**Files:**
- Create: `server/crates/waddle-server/src/ingress/recovery_executor_tests.rs` (`#[cfg(test)] #[path] mod tests;` from `recovery_executor.rs`)
- Reuse: `execute_detached_fault_tests.rs` (detached SM + actor registration via `dual_registration::mirror_register`), `offline_settlement_tests.rs`, `execute_observer_tests.rs` (`room_observer_test_manager`), `server/routes/interpret/groupchat_recovery_tests.rs` (`state_for`, `recovery_plan`), `dm_pin_authority_tests.rs`, `execute_test_hooks.rs`.

Shared helpers: `StateEnvironment(Arc<WebSocketState>)` implementing `RecoveryEnvironment` via `build_interpret_deps(self.0.as_ref(), None)`; `state_for(&fixture, overrides)` as in `groupchat_recovery_tests.rs` but accepting `TestStateOverrides`; `immediate_recovery_budget()` (`grace: zero`); `assert_recovered(fixture, key, receipts)`.

Every test: real plan → `commit_submission` → Phase C **not run** (or aborted by a fault hook) → `run_maintenance_pass(.., Some(env))` → assert the side effect happened exactly once (actual frame/row/invocation, never receipt count alone **[R1-8]**), receipts complete, row terminal → run the pass again → side-effect count unchanged. Each with `sqlite_`/`postgres_` wrappers:

1. `detached_route_recovers_each_resource_once` — two detached resources; `append_count == 1` each after two passes.
2. `stalled_execution_is_recovered_without_double_append` — run the original `execute_effects` under `STALL_DELIVERY_RESOURCE` for the second resource, abort with `tokio::time::timeout`, then recover: first resource still appended once (delivery progress), second appended once.
3. `delegated_live_full_target_route_is_left_pending` — resource registered live via `register_with_carbons` + `mirror_register`; envelope `to` = full JID, no recipient archive recorded, recorded `RouteDirect { fanout: [full], CaptureOrdinal }`; two passes sharing one `MaintenanceCursor` → no frame received, row non-terminal, `unrecoverable_obligations{kind="route_direct"}` incremented once, and the second pass attempts nothing (`#[cfg(test)]` attempt counter). Companions: `bare_target_live_route_recovers_once` (bare target → exactly one frame received, terminal) and `detached_no_store_full_target_route_is_deferred` (`<no-store/>` full target, detached resource → pending, documented deferred class **[R3-6]**). **[R1-2][R2-2]**
4. `groupchat_inbox_push_route_is_left_pending` — production-planned groupchat message with `PushInboxUpdate` + `RouteDirect` (plan through `PlanSink` as `execute_detached_fault_tests` does, or the groupchat archive planner); no Phase C; pass → no frame to the member, row non-terminal, `unrecoverable_obligations{kind="route_direct"}` incremented. **[R1-1]**
5. `offline_pending_row_and_candidate_recover_once` — `pending_delivery == 1`, `notification_candidates == 1`, three receipts.
6. `observer_plugin_recovers_once` — assert the test manager's invocation count (expose one if `room_observer_test_manager` lacks it) is exactly 1 after two passes.
7. `groupchat_notification_recovery_completes_via_maintenance` — `recovery_plan` fixture; no Phase C; hold the canonical lock in a blocker tx, spawn the bound pass **and** `reconcile_groupchat_notification_candidates_for_sweep` concurrently, release, join; assert one candidate, recovery row completed once, receipts complete, terminal. **[R1-11][R2-7]**
8. `receipted_pin_with_lost_routes_recovers_each_route_once` — commit + execute the pin with all routes stalled (`STALL_DELIVERY_RESOURCE`) so the mutation receipt commits but no route does; abort; recover → one event frame per participant resource, terminal.
9. `completed_pin_with_lost_route_is_not_re_pinned_after_unpin` — as 8 for one route, then apply an unpin through the normal path, then recover: the lost route's frame is delivered once and the pin state remains **unpinned**. **[R1-4]**
9b. `unreceipted_pin_mutation_is_deferred_after_a_later_unpin` — execute the pin with the mutation receipt write failing (force `record_receipt_pooled` to fail via a `#[cfg(test)]` hook in `execute_test_hooks.rs`, e.g. `fail_receipt_once(key, receipt)`), apply an unpin normally, recover → pin state stays unpinned, no frames, `unrecoverable_obligations{kind="dm_pin_mutation"}` incremented. **[R2-3]**
10. `muc_decline_recovers_claim_and_inviter_route` — follow the decline fixture in `invitation_replay_tests.rs`; assert the ledger claim happened and the inviter received one frame. **[R1-5]**
11. `live_duplicate_and_recovery_serialize_on_the_canonical_lock` — offline family: hold `CanonicalMessageRepository::lock` in a blocker tx, spawn the bound pass and `execute_effects` of a re-committed duplicate decision, release, join; assert one pending row, one candidate, receipts complete, terminal. Repeat for the detached SM family (two racers, `append_count == 1`). **[R1-3]**
12. `unrecoverable_only_rows_do_not_enter_the_recovery_scan` — `Carbons`-only row: pass `Complete`, row pending, `recover_row` attempt counter unchanged (kind filter). Companion `unsupported_backlog_is_evaluated_once_then_skipped` — all passes share one `MaintenanceCursor` via `run_maintenance_pass_with_cursor` **[R3-4]**: 70 provenance-rejected `RouteDirect` rows (groupchat envelope) plus one recoverable row at the tail, `recovery_max_attempts = 64`: pass 1 `Partial` (64 attempts, all unsupported); pass 2 `Partial` (wraparound: 6 unsupported + the tail row, which becomes terminal); pass 3 `Complete` with zero attempts. Companion `changed_evidence_re_evaluates_a_cached_unsupported_row` **[R3-3]**: an unreceipted-pin row is evaluated once (unsupported, cached); then a foreground duplicate (`commit_submission` of the same submission + `execute_effects` with all routes stalled) receipts the mutation; the next pass re-evaluates (evidence counts changed), recovers the routes and terminalizes.
13. `row_deadline_bounds_a_stalled_delegate_and_later_rows_still_run` — variant A: `STALL_DELIVERY_RESOURCE` on row A's only resource, row B recoverable; `recovery_row = 200ms`; pass `Partial`, B terminal, A pending; second pass without the stall recovers A. Variant B **[R3-5]**: row A is a groupchat-notification delegation; the test waits on `after_recovery_freeze(A)`, then takes the canonical lock in a blocker tx and releases the gate, so the delegate's own 100 ms lock timeout fails → A deferred (`Partial`), blocker released, B terminal in the same pass.
14. `forced_stop_during_bound_recovery_is_prompt` — extend `cancellation_during_real_pass` with a bound environment and a stalled row; `force_stop.cancel()` exits within 100 ms. **[R1-12]**
15. Dedicated XEP suite (`server/crates/waddle-server/tests/xep0198_ingress_authority.rs`): `maintenance_recovery_appends_lost_detached_delivery_once` — through the public API only: `IngressAuthority::new` + `bind_recovery_environment` + a new `pub fn trigger_maintenance(&self)` (wraps `gc.trigger()`), with `created_at` backdated past the default grace; poll until the row is terminal; assert the detached XEP-0198 session's lost append exists exactly once and survives a resume replay without duplication (`sm_ingress_appends` keyed ledger). **[R2-7][R3 note, AGENTS.md XEP suite rule]**

- [ ] **Step 1: Write tests 1–14.** - [ ] **Step 2: Run** `-E 'test(ingress::recovery_executor)'` SQLite + Postgres; keep the Postgres log as evidence in the PR description.
- [ ] **Step 3: Fix implementation gaps.** - [ ] **Step 4: Run** `-E 'test(ingress::)'` + clippy. - [ ] **Step 5: Commit** `test(server): recovery executor regressions per ingress effect family`.

---

### Task 7: Documentation and alert text

**Files:**
- Modify: `server/docs/operations/ingress-authority.md` — metrics table (`ingress.maintenance.runs` phases incl. `recovery`; `ingress.maintenance.recovered_obligations`; `ingress.maintenance.unrecoverable_obligations{kind}`), `## Periodic maintenance` (phase order and budgets: terminalization 2 s → recovery 4 s, 1 s per row, 32 rows × 2 pages, recoverable-kind scan filter → retention GC; hard deadline 13 s; recovery skipped until the websocket state binds), `## Retention and unresolved effects` (replace "#1658 adds the recovery executor"), `## Non-terminal backlog triage` (which `kind_family` values recover automatically, which stay pending and why, provenance rule for `route_direct`, remote-owned resources, the live-send race caveat), and the V1015/V1016 paragraph stating "there is no recovery executor".
- Modify: `server/docs/rfcs/0018-ingress-authority-cutover.md` — limitation (i) becomes scoped (recoverable families listed; deferred/unrecoverable families and the live-send race remain stated limitations), and the §3.7 sentence referencing #1755.
- Modify: `infrastructure/waddle.cloud/rules/mimir/waddle-reliability.yaml` — `IngressUnresolvedEffectsGrowing` / `IngressNonTerminalBacklog` summaries: replace "Issue #1658 adds the recovery executor" with the actual behaviour and the unrecoverable counter.

- [ ] Edit; `rg -n "no recovery executor|adds the recovery executor" server/docs infrastructure` must return nothing; commit `docs(server): describe the ingress maintenance recovery phase`.

---

### Task 8: Verification, adversarial review, PR

- [ ] `cd server && cargo fmt --all --check && cargo clippy --all-targets --all-features -- -D warnings && cargo nextest run -p waddle-xmpp --lib -E 'test(telemetry::)' && WADDLE_TEST_POSTGRES_URL=postgres://waddle_test@127.0.0.1:55435/waddle_test cargo nextest run -p waddle-server --all-features --lib -E 'test(ingress::)'`.
- [ ] Full `cargo nextest run -p waddle-server --all-features` once at the end (rerun known metric-guard flakes serially).
- [ ] Adversarial review by gpt-6-astra (high) under the CLAUDE.md high-value review rule; repeat until `CLEAN`.
- [ ] Update PR #1775 title/description with the completed plan, Postgres evidence, and the stated limitations; undraft; monitor CI; after landing, `@codex review` loop until no significant findings.

## Self-review notes

- Spec coverage: bounded phase after terminalization/before GC (Task 5); rebuild via existing restorers (Task 4); same arms/settlement contract, lock only for the frozen read, no actor call in-tx (Task 5); recorded-wins with a payload-provenance gate (Task 4 step 6); typed outcomes + metrics (Tasks 2, 5); runbook/RFC/alerts (Task 7); per-family SQLite+Postgres regressions incl. fault-hook stall, races, lifecycle and budget tests (Task 6).
- Review round 3 disposition (gpt-6-astra high, REVISE): R3-1 generic routes restricted to `Chat|Normal`, headline deferred (+test); R3-2 DM-pin-owned `StanzaId` routes excluded from generic rebuild (+test); R3-3 unsupported cache keyed by `(intents, receipts)` evidence counts returned by the scan, invalidated on change (+transition test); R3-4 `Complete` = sweep reached tail; corrected 12b expectations, shared cursor in tests; R3-5 `after_recovery_freeze` test gate for 13B; R3-6 `<no-store/>` deferred class documented + tested; test 15 through public API (`trigger_maintenance`).
- Review round 2 disposition (gpt-6-astra high, REVISE): R2-2 delegated live full-JID routes are deferred via the shared `live_recipient_delegated` predicate (Task 4 §6, tests 3/3b); R2-3 unreceipted pin mutations and their routes deferred, route-only recovery for receipted mutations (Task 4 §2, tests 8/9/9b); R2-5 bounded in-memory unsupported-row set + attempt budget (Task 5, test 12b); R2-6 at-least-once wording for keyless sinks + follow-up issue; R2-7 concurrent janitor test, exclusion assertions, delegate-stall variant, XEP-0198 suite test (tests 7/12/13/15). R2-1/R2-4 confirmed resolved.
- Review round 1 disposition: R1-1 provenance gate (Task 4 §6, tests 4); R1-2 recipient pass for live full-JID (Task 4 §6, test 3); R1-3 exactly-once statement + races (family table note, test 11, follow-up issue for a send lease); R1-4 receipted pin not replayed (Task 4 §2, test 9); R1-5 claim binding + generic path (Task 4 §9, test 10); R1-6 remote-owned scope note; R1-7 absolute row deadline, kind-filtered scan, outcome semantics (Tasks 1, 5, test 13); R1-8 real fixtures/assertions (Tasks 5–6); R1-9 deferred vs unavailable wording; R1-10/11/12/13 preserved as tests and doc scope.
