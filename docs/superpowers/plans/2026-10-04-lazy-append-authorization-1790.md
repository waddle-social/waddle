# Lazy cross-node append authorization (#1790) Implementation Plan

**Goal:** Stop running the canonical-row authorization read (`check_canonical_obligation`: full envelope decode + every intent decode + archive positions) at the three ordered-relay receiver entry points for relayed full-JID messages whose consumer never uses that result, without introducing any point-in-time liveness decision and without weakening the keyed-append guarantees.

**Spec:** GitHub issue #1790. The issue's premise is partly stale (see `TODO-ACTOR.md:65`): today a keyed `SmIngressAppendContext` is required for *every* keyed delivery (live or detached), and authorization failure is a hard NACK (`TargetUnavailable` / `Unavailable`), never an unkeyed fallback. This plan therefore targets the genuinely redundant reads only.

## Where the entry-point read is redundant today

The three receiver entry points (`clustering/route_bridge/delivery/{local.rs:29-40, remote.rs:312-318 & 357-363, remote_socket.rs:137-143 & 176-182}`) call `authorize_ingress_append` → `check_authority` → `check_canonical_obligation` (`ingress/append_authority.rs:134-181`) before the delivery branch is known. The consumers of the resulting context are:

| Consumer | Re-authorizes on its own? | Entry-point read needed? |
|---|---|---|
| Live local keyed send: `interpret::deliver_ordered_local_copy` (`route_to_connection.rs:1366`) → `live_delivery_status` (`authorize_resource`) → `accept_live_delivery_inner` (`authorize_resource`, `authorize_direct_stanza`; `check_canonical_obligation` for MUC/carbon kinds) | **Yes**, transactionally | No |
| Forwarding hop: `try_deliver_full_jid_remote` / `try_deliver_processed_full_jid_remote` → `prepare_remote_delivery` re-serializes `IngressAppendObligationRef::for_message(ctx)` onward (`ordered_send.rs:48-72`) | Next receiver authorizes | No |
| Registered remote socket: `remote_resource_frame(... ctx)` → socket node runs `check_canonical_obligation` again (`remote_socket.rs:389-418`) | **Yes** | No |
| Detached fallback: `deliver_peer_to_full` / `deliver_direct_to_full` (`routing.rs:781-850`) → `deliver_to_detached` → `append_detached` (`routing.rs:953-986`) → `record_keyed_stanza_for_detached_bound_resource`; its transaction (`sm_persistence/ingress_append.rs:22 authorize_new_delivery`) proves canonical **existence** only, not sender/receipt/positions | **No** | **Yes** — this is the only consumer that depends on the entry-point check |

The socket detach drain already authorizes lazily "at the append decision, and nowhere earlier" (`server/routes/websocket/replay.rs:134-142`, `drain_append.rs`). This plan brings the relay receivers to the same model.

## Design (revised after architecture review round 1, gpt-6-astra: findings R1-1..R1-3, all blocking, all addressed below)

**Authorization travels with the context and is resolved by the first consumer that trusts it.**

1. `SmIngressAppendContext` (`server/routes/interpret/deps.rs:135-158`) gains a field `authority: AppendAuthority` where

   ```rust
   #[derive(Clone)]
   pub(crate) enum AppendAuthority {
       /// Minted by this node's own ingress commit, or already verified against the canonical row.
       Verified,
       /// A relayed claim that passed the synchronous checks only
       /// (`SenderClaimMismatch`, `check_stanza_binding`). The canonical read runs
       /// on first `ensure_verified`, once per context clone-tree.
       Deferred(Arc<DeferredAppendAuthority>),
   }
   pub(crate) struct DeferredAppendAuthority {
       db: Database,                       // clone of `state.deps.app_state.db_pool.global()`
       obligation: IngressAppendObligationRef,
       result: tokio::sync::OnceCell<Result<(), AppendAuthorityRejection>>,
   }
   ```

   `SmIngressAppendContext::ensure_verified(&self, stanza: &Stanza) -> Result<(), AppendAuthorityRejection>` is `Ok(())` for `Verified`, and for `Deferred` runs `check_canonical_obligation(&db, stanza, &obligation)` through the `OnceCell` (a forwarding hop that later falls back locally pays at most one read). Failures are recorded with the existing `record_authorization_failure` and counter, once.

   `AppendAuthority` is **not** serialized: `IngressAppendObligationRef::from_context` / `for_message` only read the data fields (unchanged), so the wire shape and `deliver_ordered.vN` id do not change. Every existing constructor of `SmIngressAppendContext` on the origin side (ingress commit, `from_relayed`, drain, tests) sets `Verified`. The field has no `Default`, so every constructor site must decide explicitly.

2. `clustering/route_bridge/delivery/ingress_append.rs::authorize_ingress_append` becomes synchronous: it keeps `SenderClaimMismatch` and `check_stanza_binding` (cheap, still hard NACKs) and returns `Some(obligation.clone().into_deferred_context(db))`. The three entry points keep their shape (`requires_ingress_authority(obligation) && ctx.is_none()` → NACK) but no longer await a DB read.

3. **Consumers that trust the context call `ensure_verified` before their first trusted action** **[R1-1]**:
   - `interpret::deliver_ordered_local_copy` (`route_to_connection.rs:1366`) calls `ensure_verified(stanza)` **first**, before `live_delivery_status` and its durable-status short-circuit at `:1398`. That short-circuit returns success on existing receipts/custody/completed attempts through `authorize_resource` (`live_delivery.rs:277`, `:421`), which checks receipt, target and archive positions but **not** the canonical sender; without the pre-check a relay carrying a valid sender claim and *another* sender's already-completed message key would be ACKed. The live local path therefore pays the same single read it pays today, just at the consumer instead of the entry point. A rejection returns `None` (keyed detached fallback), which then fails definitively in step 4 **[R1-2]**: no `MaybeCommitted`, no change to the existing `TargetUnavailable`/`Unavailable` mapping, and still exactly one read because the `OnceCell` caches the rejection.
   - `append_detached` (`routing.rs:953`) calls `ensure_verified(stanza)` before `record_keyed_stanza_for_detached_bound_resource`. On `Err` it returns `Ok(false)` *without* an unkeyed append (preserving "rejected keyed copies cannot degrade to unkeyed appends"); `deliver_to_detached` maps that to `Unavailable` exactly as today. `deliver_peer_to_full`/`deliver_direct_to_full` with `Some(ctx)` still go straight to the detached branch (`routing.rs:788-794`, `827-833`), so the keyed-register-race invariant is untouched.
   - Consumers that **do not** trust the context and therefore never read: the forwarding hop (`try_deliver_full_jid_remote` / `try_deliver_processed_full_jid_remote` → `prepare_remote_delivery`, which re-serializes the data fields onward; the next receiver authorizes) and the registered-remote socket frame (`remote_resource_frame`; the socket node runs `check_canonical_obligation` itself at `remote_socket.rs:389-418`). This is where the issue's saving lands: an intermediate hop and an owner→socket hop no longer pay the read at all, so a three-hop path pays once instead of twice and a registered-remote route pays once instead of twice.

4. Rejection semantics are unchanged and definitive: `CanonicalAbsent`, `CanonicalSenderMismatch`, `ArchivePositionMismatch`, `CarbonObligationMismatch` all surface as `TargetUnavailable` (ordered receiver) / `Unavailable` (remote-resource paths) with an empty queue and the `authorization_failed` counter incremented once. The ordered receiver's existing `MaybeCommitted` mapping (`receiver.rs:125`) is reserved for genuinely uncertain outcomes and is never reached for an authorization rejection, because the rejection is resolved by `ensure_verified` before `live_delivery_status` runs.

5. Measurement (acceptance): a `#[cfg(test)]` atomic read counter in `ingress/append_authority.rs` incremented inside `check_canonical_obligation`, with assertions in the existing route-bridge tests: live-recipient relay = **1** read at the receiver (unchanged; was 1 at the entry point), intermediate hop = **0** (was 1; the onward obligation is still forwarded), registered-remote owner hop = **0** (was 1; the socket node still performs its own), detached fallback = **1**, rejected cases = **1**. Tests run under `cargo nextest` (one process per test), so the static is race-free.

## Explicit non-changes

- No liveness probe anywhere. The branch that decides "detached" is the existing fallback order, and the read happens *inside* the append decision.
- No change to the receiver-side checks themselves, to the wire format, or to `deliver_ordered.vN`.
- `RemoteUserSideEffect::Carbons` receiver (`registration/side_effects.rs:299-317`) calls `into_context()` with no canonical read today; it is out of scope and reported as a follow-up in the PR.
- The socket node's second authorization of registered-remote frames stays (it is the socket's own fence).

## Tasks (TDD, seams = public interpret/route-bridge APIs and the existing fixture tests)

1. **Type + lazy authority.** Add `AppendAuthority`, `DeferredAppendAuthority`, `ensure_verified`, `IngressAppendObligationRef::into_deferred_context(db)`. Fix every constructor (`Verified`). Red test: unit test in `append_authority` that `ensure_verified` runs the canonical read once across clones and caches the rejection.
2. **Receiver entry points go sync.** Change `authorize_ingress_append`; keep NACK semantics. Red tests (extend `clustering/route_bridge/tests/ingress_append.rs`): (a) `live_recipient_delivery_writes_no_append_ledger_row` asserts exactly 1 canonical read (at the consumer); (b) `forwarded_obligation_survives_intermediate_hop` asserts 0 reads and the obligation still forwarded; (c) `registered_remote_frame_carries_the_executors_ingress_obligation` (`tests/delivery.rs:545`) asserts 0 reads on the owner hop; (d) existing `CanonicalAbsent` / `CanonicalSenderMismatch` / `ArchivePositionMismatch` cases in `ingress_append_authority` still produce `TargetUnavailable`/`Unavailable` with an empty queue and exactly 1 read; (e) new `completed_obligation_with_mismatched_canonical_sender_is_refused_live` **[R1-1]**: seed receipts so `live_delivery_status` would short-circuit to success, relay with a mismatched canonical sender, assert `TargetUnavailable`, no ACK, no ledger row.
3. **Consumers verify.** `deliver_ordered_local_copy` and `append_detached` call `ensure_verified`; keyed-register-race test (`routing_keyed_register_race.rs`) and `execute_relay_detached_tests` stay green; `keyed_detach_drain` tests unchanged.
4. **Docs.** `server/docs/operations/ingress-authority.md`: one paragraph in the cross-node keyed append section stating authorization is resolved at the append decision (live path transactional, detached path lazily, socket node for registered frames); update `TODO-ACTOR.md:65` entry.
5. **Verification** **[R1-3]**. `cargo fmt`; `cargo clippy --workspace --all-targets --all-features -- -D warnings` **and** default features; `cargo nextest run -p waddle-server --features clustering` for `clustering::route_bridge::`, `ingress::`, `server::routes::interpret::`, `server::routes::websocket::tests::keyed_detach_drain` with `WADDLE_TEST_POSTGRES_URL` set so the Postgres halves run (the route-bridge module is entirely behind `feature = "clustering"`, `clustering/mod.rs:90`, which is off by default); a separate default-feature `cargo nextest run -p waddle-server` pass for the interpret/drain tests; full workspace nextest `--all-features` once at the end, with the test listing showing the forwarding, authorization, live-recipient measurement and detach-race cases ran.
