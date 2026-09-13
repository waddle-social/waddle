# Per-Occupant MUC Fan-out Progress (#1757) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A partially failed room broadcast (`RouteMucGroupchat` / `RouteMucSystemBroadcast`) must resume only the undelivered occupant copies, settle its aggregate obligation from durable per-occupant progress, and be recoverable by the maintenance recovery phase, so canonical groupchat rows terminalize without depending on every copy succeeding in one execution. Closes the remainder of RFC 0018 §1 stated limitation (ii).

**Spec:** GitHub issue #1757; RFC 0018 `server/docs/rfcs/0018-ingress-authority-cutover.md` (§1 (ii), §3.3a, §3.6b, lines 113-118 relay statement); runbook `server/docs/operations/ingress-authority.md` ("Detached delivery progress", "What recovery handles").

## Facts the plan is built on (verified on main 25bec708)

- A committed groupchat row records **both** one `RouteDirect { recipient: occupant bare, fanout: [occupant full], route_identity: CaptureOrdinal }` per occupant copy (`route_to_connection.rs:89-127,393-434`, the room fan-out is one `RouteToConnection` per occupant) **and** one aggregate `RouteMucGroupchat { room, occupants, reflection, room_generation, route_identity: StanzaId(room) }` (`room_dispatch.rs:653-686`; system broadcasts: `room_system_message.rs:225-252`). Both MUC variants share storage kind 2 and semantic key `room|route_identity` (`effect_intent.rs:1808,2022`).
- `RouteProgress` (`ingress/recorded.rs:24-46`) is keyed by **one bare recipient** + route identity; `matches`/`remaining`/`execute_uow::owns` (`execute_uow.rs:33-57`) assume that key. `owns` and `route_progress_filter` (`suppression.rs:108-138`) have **no `RelayFullJid` or `Frame` arm**.
- The aggregate kind-2 receipt is generic and all-or-nothing: `receipts_routing::route_receipts` → `cover_recipients` (`receipts_routing.rs:71-89,192-232`) returns `[]` unless every recipient in `occupants ∪ {reflection}` has a matching delivery effect in *this* execution, matched by the room stanza-id stamped on the copy. The generic executor writes the receipt only when every carrying effect is `Done` (`execute.rs:598-700`); frame copies defer to `complete_frame_obligations`.
- Duplicate suppression (`suppression.rs:68-77`): a non-sender copy survives a duplicate only via `RouteProgressFilter::Keep` (per-occupant `RouteDirect` progress) or `unreceipted_repair`; the sender copy is `PlanSuppressionPolicy::Always`.
- `execute_detached::record_resource` (`execute_detached.rs:174-217`) hardcodes `IngressEffectIntent::RouteDirect` as settle evidence; `settle_recorded` requires exact `PartialEq` with the recorded intent (`ingress_uow/settlement.rs:21-43`).
- `ingress_delivery_receipts` (V1014) is keyed `(message_key, kind, semantic_identity_hash, resource)` with an FK to `ingress_effect_intents` — **generic for any kind**, so kind-2 progress rows need no schema change. `DeliveryProgressRepository::{load, load_all, record}` (`ingress_uow/delivery_progress.rs`).
- Cross-node occupant copies ride `RelayFullJid` → `deliver_full_jid_via_ordered_relay` → `OrderedRelayAck` (`deliver_ordered.v10`), one relay message **per occupant copy**; the executor sees `EffectOutcome::Delivery(FullJidDeliveryOutcome)` (`delivery_immediate.rs:68-86`). `SmIngressFrameReceipt` in the ACK has no `resource`. CLAUDE.md hard rule: any change to `RemoteStanzaEnvelope`/`OrderedRelayAck`/`Nack`/`Reply` or their contents ⇒ `deliver_ordered.v11`.
- Recovery (#1755): `RECOVERABLE_KINDS` (`recovery_rebuild.rs:29-36`) excludes kind 2; `direct_provenance` requires `Chat|Normal`, so groupchat `RouteDirect` copies are unrecoverable (`recovery_rebuild_tests.rs:166`); `freeze` builds `RouteProgress` only from `RouteDirect` (`recovery_executor.rs:249-275`). Runbook line 653 lists `route_muc` as unrecoverable.
- The canonical envelope of a groupchat row is the **post-canonicalization** room message (`from = room/nick`, `to = None`, room stanza-id + occupant-id stamped; RFC §3.3, `canonicalize.rs:143-173`), so an occupant copy is a pure `(envelope, occupant)` transform: set `to`, re-stamp XEP-0359 from `ArchiveAuthoritative` (as `delivery_message` does for direct).
- Tests asserting today's behaviour that must flip: `execute_detached_fault_tests.rs:287-366` (`detached_muc_keeps_generic_settlement`), `recovery_rebuild_tests.rs:166`, `execute_relay_detached_tests.rs:76-79`.
- Harness: `tests/ingress_support.rs::IngressFixture` (SQLite + Postgres; Postgres skips silently without `WADDLE_TEST_POSTGRES_URL`); cross-node tests need `--features clustering` (`tests/xep0045_cross_node_reflection.rs`, `tests/clustering_cluster_e2e.rs:2367`).

## Premises to prove first (Task A, red tests)

- **P1** A local partial broadcast (occupant A delivered, B's copy fails) on a duplicate resumes only B through the per-occupant `RouteDirect` progress, **but** the aggregate kind-2 receipt is never written because `cover_recipients` needs every copy in one execution → row non-terminal.
- **P2** A cross-node occupant copy (`RelayFullJid`) has no progress row; on a duplicate it is suppressed (`SenderOnly`, no `Keep` arm) → row non-terminal, copy never resumed.
- **P3** The maintenance recovery phase never selects a groupchat row (kind 2 absent) and never rebuilds occupant copies.
If a premise fails to reproduce, stop and report before implementing that part.

## Global constraints

- Clippy `-D warnings --all-targets --all-features`; no new `#[allow]`. Typed payloads only (obligation shapes are enums, never strings). No XML via `format!`.
- Settlement contract: progress + aggregate receipt commit in ONE tx under the canonical row lock; no actor/registry/socket call while it is open; the append/send happens **before** its progress tx.
- Exact ownership (`execute_uow::owns`): ownership by exact effect variant + progress match, no intent-kind heuristics.
- XEP-0045 occupant-copy semantics unchanged (from = room/nick, one copy per occupant full JID, sender self-copy).
- No wire change unless proven necessary: the per-copy ACK already proves that copy; `deliver_ordered` stays v10. If a lane finds it must change a relay type, stop and report (v11 + signing view + `relay/tests.rs:19`).
- No semantic-key change (would orphan in-flight receipts → Recreate). `RouteProgress` is in-memory; `ingress_delivery_receipts` is reused as-is. RollingUpdate deploy.
- SQLite + Postgres regressions for every behaviour; verify with `cargo nextest run` (add `--features clustering` for cross-node).

## Design

### D1. Typed progress obligation

`RouteProgress` gains `obligation: ProgressObligation` and keeps `receipt`, `route_identity`, `received_at`, `fanout`, `completed`:

```rust
pub enum ProgressObligation {
    Direct { recipient: BareJid },
    MucGroupchat { room: BareJid, occupants: Vec<FullJid>, reflection: FullJid, room_generation: EntityGeneration },
    MucSystemBroadcast { room: BareJid, occupants: Vec<FullJid>, room_generation: EntityGeneration },
}
```
`fanout` for MUC = `occupants ∪ {reflection}` (the exact set `cover_recipients` requires; `occupants` stays canonical sorted/deduped so the exact recorded intent can be rebuilt). `ProgressObligation::settle_evidence(&self, route_identity) -> IngressEffectIntent` rebuilds the exact recorded intent for `settle_recorded` (replaces the hardcoded `RouteDirect` in `execute_detached::record_resource`).

`matches(effect)`: `Direct` as today. MUC: the effect is a full-JID copy (`RouteToPeer`, `QueueDetached` single resource, `RelayFullJid`, or a sender `Frame`) whose target ∈ `fanout` and whose stamped room stanza-id equals `route_identity` (reuse `receipts_routing::full_delivery` + `message_identity`; the per-copy `CaptureOrdinal` is *not* the MUC identity). `remaining(effect)` → targets ∩ fanout \ completed.

### D2. Commit builds MUC progress; aggregate becomes arm-owned

`commit.rs:422-458`: for each unreceipted `RouteMucGroupchat`/`RouteMucSystemBroadcast` load per-recipient progress under the kind-2 key and push a MUC `RouteProgress`. `decision::assemble_receipts` then attaches the aggregate key to every covered copy effect as **arm-owned**, and `receipts_routing::route_receipts` no longer produces the generic all-or-nothing aggregate for MUC when progress exists (the generic path stays only for rows committed before this change? No: every MUC row now has progress from commit; delete the `cover_recipients` MUC arms and their tests, no compatibility branch).

### D3. Executor arms record per-occupant progress

- Local copies (`RouteToPeer`, `QueueDetached`): already owned by `execute_detached` through the per-occupant `RouteDirect` progress. Extend the arm so one copy discharges **every** matching progress (its `RouteDirect` and the MUC obligation) in the same progress tx: `record_resource` records `(key, resource)` for each matching progress, reloads, and settles each obligation whose fanout is covered (`settle_evidence`). `SmIngressAppendContext` keeps the `RouteDirect` key (ledger identity unchanged).
- Cross-node copies (`RelayFullJid`): new `owns` arm (target ∈ a matching progress's fanout) and a new `execute_relay_copy` arm: send via the existing `deliver_full_jid_via_ordered_relay`; on `Delivered` run the same progress tx; `Unavailable`/declined → leave pending (typed `EffectOutcome`), never fall back to a raw append. The per-copy ACK is the proof; no relay type changes.
- Sender `Frame` copy (relayed-owner path, `relay_plan::return_sender_reflection`): `ExecutionReport::complete_frame_obligations` records progress for `(MUC key, reflection)` instead of a generic aggregate receipt when the frame is arm-owned, and settles the aggregate if that completes the fanout. If this proves too invasive, the fallback is to record the reflection's progress when the frame obligation is written in the same code path that writes frame receipts today — either way the reflection copy must be covered by progress, never by a second receipt path.

### D4. Replay trims to (fresh ∩ frozen) \ completed and rebuilds from the envelope

- `suppression::route_progress_filter` gains `RelayFullJid` and sender `Frame` arms: `Keep` when the target ∈ fanout \ completed, `Drop` when completed, and fresh occupants ∉ frozen fanout (audience drift) are dropped for MUC-owned copies.
- `recorded::restore_delivery_payloads` gains a MUC arm: `occupant_copy_message(envelope, occupant, intents)` (pure: `to := occupant`, XEP-0359 restamp from `ArchiveAuthoritative`; `from` already the occupant JID of the sender) rebuilds every kept copy from the canonical envelope.

### D5. Recovery phase rebuilds remaining occupant copies

- Add `RouteMucGroupchat` to `RECOVERABLE_KINDS`; `freeze` builds MUC `RouteProgress` from unreceipted MUC intents + `load_all`.
- `recovery_rebuild::restore_muc_routes`: for an unreceipted MUC intent on a `Groupchat` envelope, rebuild one `QueueDetached { bare: occupant bare, resources: [occupant], route_identity: <the recorded per-occupant RouteDirect's CaptureOrdinal for that occupant>, stanza: occupant_copy_message(..) }` per occupant in `fanout \ completed`. A copy whose per-occupant `RouteDirect` is missing (never captured because the copy failed before a definitive outcome) is rebuilt with `route_identity: None` and discharges only the MUC progress. Remote-owned resources return `Unavailable` and stay pending (same stated limitation as direct recovery). Groupchat `RouteDirect` intents remain individually unrecoverable (`direct_provenance`) — the MUC restorer owns those copies.

### D6. Docs

RFC 0018 §1 (ii) rewritten (per-occupant progress now covers room broadcasts), §3.3a last paragraph replaced by a §3.3e "Per-occupant room fan-out progress (#1757)", §3.6b recovery table row for `route_muc`; runbook "Detached delivery progress" line 929 and recovery table line 653 updated; `IngressNonTerminalBacklog` runbook note.

## Tasks (lanes)

Lane A first (premise tests + D1/D2/D3-local); then B (cross-node + frame) ∥ C (replay + recovery); D docs.

### Task A — premises, typed obligation, local arm

**Files:** `ingress/recorded.rs`, `ingress/decision.rs`, `ingress/commit.rs`, `ingress/execute_detached.rs`, `ingress/execute_uow.rs`, `ingress/receipts_routing.rs`, `ingress/suppression.rs`, tests `tests/ingress_cases/muc_occupant_progress.rs` (+ register in `tests/ingress_commit.rs`), in-crate `execute_detached_fault_tests.rs`.

- [ ] Red: P1 regression (SQLite + Postgres): groupchat row with occupants A, B; A's copy delivered, B's fails (fault hook) → progress row for A under the kind-2 key, no aggregate receipt, row non-terminal; duplicate commit → only B's copy planned (A dropped), B delivered → progress for B, aggregate receipt, RouteDirect receipts, row terminal; occupant C who joined after the first commit receives nothing.
- [ ] Red: `detached_muc_keeps_generic_settlement` inverted to `detached_muc_settles_from_occupant_progress`.
- [ ] Green: D1, D2, D3-local. Delete `cover_recipients` MUC arms.
- [ ] Verify (`cargo nextest run -p waddle-server ingress`, both backends, clippy) + commit `feat(server): per-occupant progress settles MUC groupchat fan-out obligations (#1757)`.

### Task B — cross-node copies + sender frame

**Files:** `ingress/execute_uow.rs`, new `ingress/execute_relay_copy.rs`, `ingress/execute.rs` (`RelayFrames`/frame completion), `interpret/effects/delivery_immediate.rs`, `tests/xep0045_cross_node_reflection.rs` (clustering), in-crate `execute_groupchat_receipt_tests.rs`.

- [ ] Red (clustering feature, SQLite + Postgres): P2 regression — occupant B owned by another node; relay `Unavailable` on first execution → no progress for B, A recorded; duplicate → only B relayed; ACK → progress + aggregate + terminal. Relayed-owner sender frame covers the reflection through progress.
- [ ] Green: D3 relay arm + frame coverage; `execute_relay_detached_tests.rs:76-79` updated.
- [ ] Verify + commit `feat(server): cross-node occupant copies record MUC fan-out progress (#1757)`.

### Task C — replay rebuild + recovery phase

**Files:** `ingress/suppression.rs`, `ingress/recorded.rs` (`occupant_copy_message`), `ingress/recovery_rebuild.rs`, `ingress/recovery_executor.rs`, `ingress/maintenance.rs`, `recovery_rebuild_tests.rs`, `recovery_executor_tests.rs`, `tests/ingress_cases/muc_occupant_progress.rs`.

- [ ] Red: P3 regression (SQLite + Postgres) — partial broadcast, no client retransmit, maintenance recovery pass → B's copy rebuilt from the envelope as `QueueDetached`, progress + aggregate + terminal; occupant copy wire shape equals the original (from = room/nick, occupant-id, stanza-id).
- [ ] Red: audience drift — recovery never rebuilds a copy for an occupant outside the frozen set.
- [ ] Green: D4 + D5; flip `groupchat_route_direct_is_unrecoverable` into the MUC-owned statement.
- [ ] Verify + commit `feat(server): replay and recovery rebuild remaining MUC occupant copies from the canonical envelope (#1757)`.

### Task D — docs + full verification

- [ ] D6 docs; full `cargo nextest run --workspace --all-targets --all-features` on both backends; clippy; fmt.
- [ ] Commit `docs(server): per-occupant room fan-out progress in RFC 0018 and the runbook (#1757)`.

## Out of scope (state in PR)

- Keyed cross-node detached appends (#1778) — relay copies stay at-least-once at the receiver; this plan adds progress, not a receiver-side idempotency key.
- Occupants owned by a remote node during **recovery** (no relay from the maintenance pass; same limitation as #1755 direct recovery).
- Splitting `RouteMucSystemBroadcast` off storage kind 2 (semantic-key change → Recreate).
