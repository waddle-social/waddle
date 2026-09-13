# Per-Occupant MUC Fan-out Progress (#1757) Implementation Plan — v2

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A partially failed room broadcast (`RouteMucGroupchat` / `RouteMucSystemBroadcast`) must resume only the undelivered non-sender occupant copies, settle its aggregate obligation from durable per-occupant progress, and be rebuildable by the maintenance recovery phase, so canonical groupchat rows terminalize without depending on every copy succeeding in one execution. Closes the remainder of RFC 0018 §1 stated limitation (ii).

**Spec:** GitHub issue #1757; RFC 0018 (§1 (ii), §3.3a, §3.6b, relay statement lines 113-118); runbook `server/docs/operations/ingress-authority.md`. Plan review round 1 (gpt-6-astra, high): REVISE — findings 1–5 blocking, 6–8 should-fix; all addressed below and marked **[R1-n]**.

## Facts the plan is built on (verified on main 25bec708; corrected per R1)

- **Occupant copies do not record `RouteDirect` intents.** Only `Chat|Normal` messages take `route_dm_to_full_jid` (`route_to_connection.rs:278`) where `capture_route_direct_intent` lives; groupchat copies take `route_to_connection.rs:308` → `route_to_connection_plan.rs:161`, which records delivery effects but no intent. The only `RouteDirect` intents on a groupchat row are **inbox updates** (`PushInboxUpdate`, `recovery_executor_tests.rs:2111,2154`), which must never be discharged by occupant delivery **[R1-1]**.
- A committed groupchat row records one aggregate `RouteMucGroupchat { room, occupants (canonical sorted/deduped), reflection, room_generation, route_identity: StanzaId(room) }` (`room_dispatch.rs:653-686`); system broadcasts record `RouteMucSystemBroadcast` after `SystemMessageArchive` (`room_system_message.rs:171,218,237`). Both share storage kind 2 and semantic key `room|route_identity` (`effect_intent.rs:1808,2022`); persisted variants stay distinct.
- `RouteProgress` (`recorded.rs:24-46`) is keyed by one bare recipient + route identity; `execute_uow::owns` (`:33-57`) and `suppression::route_progress_filter` (`:108-138`) have no `RelayFullJid`/`Frame` arms. Commit builds progress only from `RouteDirect` (`commit.rs:424`).
- The aggregate kind-2 receipt is generic and all-or-nothing (`receipts_routing.rs:71-89,192-232`, `execute.rs:673`); a non-sender copy survives a duplicate only via `Keep`, `unreceipted_repair` (compares the copy's `CaptureOrdinal` with the room `StanzaId` → never matches, `recorded.rs:532`) **or the subject-rebroadcast exception** (`suppression.rs:66`, `policy_metadata.rs`) **[R1-FALSE-4]**. Sender copy is `PlanSuppressionPolicy::Always`.
- `execute_detached::record_resource` hardcodes `RouteDirect` settle evidence (`execute_detached.rs:203`); `settle_recorded` needs exact `PartialEq` (`settlement.rs:47`).
- `ingress_delivery_receipts` (V1014) is generic over `(message_key, kind, semantic_identity_hash, resource)` (`waddle.rs:864,895`; `delivery_progress.rs`). No schema change needed for kind-2 rows.
- Cross-node copies: one `RelayFullJid` per occupant copy over the exact full-JID channel (`ordered.rs:64`); receiver delivers (`receiver.rs:30`); ACK maps to `Delivered` (`ordered_send.rs:176`). The per-copy ACK is the proof; **no `deliver_ordered.v11` for ordinary copies** **[R1-F7 TRUE]**. Direct-route relay decline falls back locally through `finish_full_jid_relay` (`delivery_immediate.rs:69`); `execute_relay_detached_tests.rs:49` is a **DM** regression, not MUC evidence **[R1-6]**.
- **The stored envelope is not always room-canonical.** A relayed owner reuses the origin's canonical key and replaces the envelope only when a `RoomObserver` is recorded (`commit.rs:180,208,358`, `recorded.rs:349`, `ingress_substrate/authority.rs:133`); with zero observer plugins the row keeps the origin's real-sender envelope **[R1-2]**. System broadcasts never replace the envelope with the synthetic system message (`room_pin.rs:173`, `pin.rs:137`) **[R1-3]**. The stored envelope codec rejects unknown fields (`authority.rs:96`); intent payloads use `#[serde(default)]` for additive fields.
- Sender frame on the relayed-owner path: `frame_receipts()` exports receipt identities without occupant identity, `replay_frame_completions()` rebuilds reports without frames, and completion writes those identities as aggregate receipts (`execute.rs:177,204,270`); owner receipts travel in the ACK (`relay.rs:375`); retained-frame completion after report/token loss is a tested path (`frame_receipts_tests.rs:283`) **[R1-4]**.
- Subject/pin copies depend on `AfterRoomSubject`/`AfterRoomPin` (`policy_metadata.rs:97`, `room_pin_scope.rs:9`, `subject_receipt_tests.rs:377`) **[R1-5]**.
- Recovery (#1755): kind 2 is absent from `RECOVERABLE_KINDS` (`recovery_rebuild.rs:29`); rows are paged when **any** pending kind is supported (`ingress_substrate/maintenance.rs:78`, `maintenance.rs:397`), so a groupchat row with supported siblings is attempted but its broadcast is never rebuilt **[R1-7]**. Budgets: 1 s per row, 4 s recovery, bounded cursor (`maintenance.rs:48`).
- Fault hooks are `#[cfg(test)] pub(crate)` task-locals (`execute_detached.rs:23`) → real-path tests live in-crate; `tests/xep0045_cross_node_reflection.rs` exercises receiver state, and `clustering_cluster_e2e.rs:2367` needs Postgres **[R1-8]**. Bodyless/chat-state copies have no `ArchiveAuthoritative` stamp; `restamp.rs:57` restores frozen stamps.

## Premises (corrected) **[R1-1][R1-7]**

- **P1'** An ordinary local partial broadcast (A delivered, B failed) leaves B **suppressed** on a duplicate (no progress, no `Keep`, no repair match) and the aggregate pending.
- **P2** A cross-node occupant copy has no progress and is suppressed on a duplicate (sender and subject-rebroadcast exceptions aside).
- **P3'** Kind-2-only pending rows are paged but not attempted; groupchat rows with supported pending siblings are attempted but the broadcast is never rebuilt. Both persisted MUC variants share kind 2.
Task A proves P1' and P2 with red tests before any implementation; if either fails to reproduce, stop and report.

## Global constraints

- Clippy `-D warnings --all-targets --all-features`; no new `#[allow]`; typed payloads only; no XML via `format!`.
- Settlement contract: append/send **before** its progress tx; progress + aggregate receipt in ONE tx under the canonical row lock; no actor/registry/socket call inside.
- Exact ownership (`execute_uow::owns`): by exact effect variant + exact progress match; **the new relay arm is limited to MUC obligations** — direct routes keep today's relay/fallback contract **[R1-6]**.
- XEP-0045 §7.4 live `from` rewrite to `room/nick` and XEP-0421 occupant-id preserved on every rebuilt copy **[R1-2]**.
- No relay type change (`deliver_ordered` stays v10); if a lane finds one necessary, stop and report (v11 + signing view + `relay/tests.rs:19`).
- No semantic-key change; `RouteProgress` is in-memory; the only persisted change is an additive `#[serde(default)]` intent field (D3) → no Recreate; RollingUpdate. Old rows without the field are typed-unrecoverable, never misinterpreted.
- Recovery never re-runs room MAM/inbox projections: rebuilt copies are `QueueDetached` only **[R1-5]**.
- SQLite + Postgres regressions for every behaviour, with evidence that Postgres cases ran; `cargo nextest run` (+ `--features clustering` where applicable).

## Design

### D1. Typed progress obligation (evidence fields separate from derived fanout)

```rust
pub enum ProgressObligation {
    Direct { recipient: BareJid },
    MucGroupchat { room: BareJid, occupants: Vec<FullJid>, reflection: FullJid, room_generation: EntityGeneration },
    MucSystemBroadcast { room: BareJid, occupants: Vec<FullJid>, room_generation: EntityGeneration, system_message: Option<Box<Message>> },
}
pub struct RouteProgress { receipt, obligation, route_identity, received_at, fanout: Vec<FullJid>, completed: Vec<FullJid> }
```
- `fanout` (derived): `Direct` → recorded fanout; `MucGroupchat` → `occupants \ {reflection}` (**non-sender copies only**, see D4); `MucSystemBroadcast` → `occupants`.
- `ProgressObligation::settle_evidence(&self, route_identity) -> IngressEffectIntent` rebuilds the exact recorded intent (all evidence fields retained verbatim) for `settle_recorded`; replaces the hardcoded `RouteDirect` in `record_resource`.
- `matches(effect)`: `Direct` as today. MUC: effect is a single full-JID copy (`RouteToPeer`, `QueueDetached` with one resource, `RelayFullJid`) whose target ∈ `fanout` and whose stamped room stanza-id equals `route_identity` (via `receipts_routing::full_delivery` + `message_identity`); for bodyless copies the frozen stamp is restored (`restamp.rs:57`) **before** matching **[R1-8]**. A `RouteDirect` inbox update never matches a MUC obligation (different variant, `PushInboxUpdate` is not a full-JID copy) **[R1-1]**.
- `SmIngressAppendContext` for an occupant copy uses the **MUC receipt key + occupant resource** (stable append key; the `sm_ingress_appends` gate applies) **[R1-1]**.

### D2. Commit builds MUC progress; aggregate becomes arm-owned

`commit.rs:422-458`: for each unreceipted MUC intent load per-recipient progress under the kind-2 key (`load_all`) and push a MUC `RouteProgress`. `decision::assemble_receipts` attaches the aggregate key to every covered copy as **arm-owned**; the `cover_recipients` MUC arms in `receipts_routing` are deleted (no compatibility branch: every MUC row has progress from commit).

### D3. Frozen payload sources **[R1-2][R1-3]**

- **Room-canonical envelope always persisted.** On owner acceptance of a relayed groupchat (and on every local room dispatch) Phase B stores the room-canonical message (`from = room/nick`, room stanza-id, occupant-id, enrichment) as the canonical envelope **independently of observer eligibility**: `commit.rs:358` replaces `room_observer_envelope` with a `room_canonical_envelope(plan)` derived from the planned fan-out copy (the reflector's `working` message), frozen at first owner acceptance and never replaced afterwards. Test: relayed owner with zero plugins → stored envelope is room-canonical.
- **System broadcasts carry their payload.** `RouteMucSystemBroadcast` gains `system_message: Option<Box<Message>>` (`#[serde(default)]`, room-bare sender, no reflection), recorded at `room_system_message.rs:237` from the exact archived system message; multiple system identities on one canonical row are distinct obligations by `route_identity`. Old rows (`None`) are typed-unrecoverable (`unrecoverable{kind=route_muc, reason=missing_payload}`), never rebuilt from the triggering command.
- `occupant_copy_message(source, occupant, intents) -> Message` (pure): `source` = room-canonical envelope (groupchat) or `system_message` (broadcast); sets `to = occupant`, restores XEP-0359/0421 stamps from `ArchiveAuthoritative`/`SystemMessageArchive`; never exposes a real sender JID.

### D4. Executor arms record per-occupant progress; reflection excluded from the obligation **[R1-4]**

- Local copies (`RouteToPeer`, `QueueDetached`): `owns` matches a MUC `RouteProgress`; `execute_detached` appends, then in the progress tx records `(MUC key, resource)`, reloads, and settles with `settle_evidence` when `fanout ⊆ completed`.
- Cross-node copies (`RelayFullJid`): new `owns` arm **only** for an exact MUC obligation match; new `execute_relay_copy` arm: send via `deliver_full_jid_via_ordered_relay`; `Delivered` → same progress tx; decline/`Unavailable` → typed pending outcome (no raw-append fallback; direct routes untouched). Covers a destination that moved from remote to local ownership between planning and execution (exact-channel result decides, then progress records) **[R1-6]**.
- **Sender reflection**: the reflection copy (local `RouteToPeer`/`QueueDetached` to the sender, or the relayed-owner `Frame`) is **not part of the MUC obligation**: it keeps `PlanSuppressionPolicy::Always` (re-sent on every duplicate, as today), carries **no kind-2 receipt key**, and is proven by nothing beyond the sender's own stream (XEP-0198). Consequences: `frame_receipts()` never exports a kind-2 identity, retained-frame replay and the ACK `owner_receipts` carry nothing new, and an old origin can never misread kind 2 as aggregate proof → no mixed-version hazard and no relay type change. The aggregate obligation is exactly "every non-sender occupant copy delivered", matching the issue wording. RFC 0018 records this as the reflection semantics.

### D5. Replay: trim to (fresh ∩ frozen) \ completed, rebuild from frozen sources **[R1-5]**

- `route_progress_filter` gains `RelayFullJid`: `Keep{remaining}` for targets ∈ fanout \ completed, `Drop` for completed, drop fresh occupants ∉ frozen fanout (audience drift). Ordering: the subject-rebroadcast exception (`suppression.rs:66`) is evaluated **before** `Drop` so subject re-application keeps today's semantics (state reapplication, no historical fan-out proof widened).
- `restore_delivery_payloads` gains a MUC arm using `occupant_copy_message` for every kept copy.
- Dependent copies (`AfterRoomSubject`/`AfterRoomPin`) keep their dependencies; a duplicate never delivers a subject/pin copy whose mutation is not receipted.

### D6. Recovery phase rebuilds remaining non-sender copies **[R1-5][R1-7]**

- Add `RouteMucGroupchat` to `RECOVERABLE_KINDS`; `freeze` builds MUC `RouteProgress` from unreceipted MUC intents + `load_all`.
- `recovery_rebuild::restore_muc_routes`: for an unreceipted MUC intent whose frozen source exists (room-canonical envelope with `MessageType::Groupchat`, or `system_message`), and whose prerequisite mutations (`RoomSubjectMutation`/room `Pin`) are **receipted** (else typed-unrecoverable `prerequisite_pending`), rebuild one `QueueDetached { bare, resources: [occupant], route_identity: None, stanza: occupant_copy_message(..) }` per occupant in `fanout \ completed`; discharges only the MUC progress. Remote-owned resources → `Unavailable`, stay pending (same limitation as direct recovery). Groupchat `RouteDirect` (inbox) intents remain unrecoverable and untouched (`groupchat_route_direct_is_unrecoverable` stays) **[R1-1]**.
- Budgets unchanged (1 s row deadline, 4 s phase, bounded cursor). Row terminality is claimed only when every sibling obligation is settled.

### D7. Docs

RFC 0018 §1 (ii) rewritten; §3.3a closing paragraph replaced by §3.3e "Per-occupant room fan-out progress (#1757)" (obligation = non-sender copies; reflection semantics; frozen payload sources; relay ACK as proof); §3.6b table row for `route_muc`; runbook lines 653/929 + `IngressNonTerminalBacklog` doc keep unsupported inbox/remote-owner/prerequisite-pending cases listed.

## Tasks (lanes)

Lane A first; B ∥ C after A; D last.

### Task A — premises, typed obligation, frozen sources, local arm

**Files:** `ingress/recorded.rs`, `ingress/decision.rs`, `ingress/commit.rs`, `ingress/execute_detached.rs`, `ingress/execute_uow.rs`, `ingress/receipts_routing.rs`, `ingress/suppression.rs`, `waddle-xmpp/src/ingress/effect_intent.rs` (system_message field + codec tests), `interpret/room_system_message.rs`, in-crate tests (new `ingress/muc_occupant_progress_tests.rs` using real planner → commit → executor with cfg(test) fault hooks, SQLite + Postgres via `test_support`), `execute_detached_fault_tests.rs` (rewrite the synthetic MUC fixture with genuine room-stanza identity — do not merely invert assertions; keep the direct-route negatives).

- [ ] Red: P1' (both backends): occupants A, B (+ sender S); A delivered, B's append fails via fault hook → progress for A under the kind-2 key, no aggregate receipt, non-terminal; duplicate commit → only B planned, delivered → progress for B, aggregate receipt, terminal; occupant C joined later receives nothing; S's copy re-sent (Always) without progress.
- [ ] Red: mixed MUC + inbox `RouteDirect` on one row → settling MUC leaves the inbox update pending **[R1-1]**.
- [ ] Red: relayed owner with zero plugins → stored envelope room-canonical **[R1-2]**; partial pin broadcast → `system_message` recorded, rebuilt copy has room-bare sender **[R1-3]**; old-row `None` → typed unrecoverable.
- [ ] Red: append-before-progress rollback + concurrent retry under the MUC append key (no duplicate queue allocation); bodyless chat-state copy matches after stamp restoration **[R1-8]**.
- [ ] Green: D1–D4 (local arm), delete `cover_recipients` MUC arms.
- [ ] Verify + commit `feat(server): per-occupant progress settles MUC fan-out obligations from frozen room payloads (#1757)`.

### Task B — cross-node copies

**Files:** `ingress/execute_uow.rs`, new `ingress/execute_relay_copy.rs`, `interpret/effects/delivery_immediate.rs`, in-crate controlled-relay tests (both backends), `tests/clustering_cluster_e2e.rs` (Postgres two-process variant), `execute_groupchat_receipt_tests.rs`.

- [ ] Red (in-crate, controlled relay stub, both backends): P2 — B remote; relay `Unavailable` first → no progress for B; duplicate → only B relayed; ACK → progress + aggregate + terminal; remote→local ownership change between plan and execute handled; DM relay fallback test (`execute_relay_detached_tests.rs`) unchanged and green **[R1-6]**.
- [ ] Red (clustering E2E, Postgres): two-node partial broadcast then recovery → foreign occupant receives exactly one copy, row terminal.
- [ ] Green + verify + commit `feat(server): cross-node occupant copies record MUC fan-out progress (#1757)`.

### Task C — replay rebuild + recovery phase

**Files:** `ingress/suppression.rs`, `ingress/recorded.rs`, `ingress/recovery_rebuild.rs`, `ingress/recovery_executor.rs`, `ingress/maintenance.rs`, tests.

- [ ] Red (both backends): P3' both halves; partial broadcast + no retransmit → maintenance rebuilds B's copy (wire shape: `from = room/nick`, occupant-id, stanza-id), progress + aggregate + terminal; timeout after one occupant then recovery within budget; audience drift never rebuilt; subject/pin prerequisite unreceipted → `prerequisite_pending`, nothing delivered; duplicate subject keeps rebroadcast semantics; `RouteOccupantPm` and headline direct routes unaffected **[R1-5][R1-8]**.
- [ ] Green: D5 + D6.
- [ ] Verify + commit `feat(server): replay and recovery rebuild remaining MUC occupant copies from frozen room payloads (#1757)`.

### Task D — docs + full verification

- [ ] D7; full `cargo nextest run --workspace --all-targets --all-features` both backends (record Postgres evidence); clippy; fmt.
- [ ] Commit `docs(server): per-occupant room fan-out progress in RFC 0018 and the runbook (#1757)`.

## Out of scope (state in PR)

- Keyed cross-node detached appends at the receiver (#1778).
- Remote-owned occupants during recovery (no relay from the maintenance pass; same limitation as #1755).
- Splitting `RouteMucSystemBroadcast` off kind 2 (semantic-key change → Recreate).
- Proof of the sender's reflection copy beyond XEP-0198 (excluded from the obligation by design, D4).
