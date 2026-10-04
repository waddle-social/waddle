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

## Design

**Authorization travels with the context and is resolved at the append decision.**

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

   `SmIngressAppendContext::ensure_verified(&self, stanza: &Stanza) -> Result<(), AppendAuthorityRejection>` is `Ok(())` for `Verified`, and for `Deferred` runs `check_canonical_obligation(&db, stanza, &obligation)` through the `OnceCell` (so a forwarding hop that later falls back locally pays at most one read). Failures are recorded with the existing `record_authorization_failure` and counter.

   `AppendAuthority` is **not** serialized: `IngressAppendObligationRef::from_context` / `for_message` only read the data fields (unchanged), so the wire shape and `deliver_ordered.vN` id do not change. Every existing constructor of `SmIngressAppendContext` on the origin side (ingress commit, `from_relayed`, drain, tests) sets `Verified`. Since the field has no `Default`, every constructor site is forced to decide explicitly.

2. `clustering/route_bridge/delivery/ingress_append.rs::authorize_ingress_append` becomes synchronous: it keeps `SenderClaimMismatch` and `check_stanza_binding` (both cheap, both still hard NACKs) and returns `Some(obligation.clone().into_deferred_context(db))`. The three entry points keep their shape (`requires_ingress_authority(obligation) && ctx.is_none()` → NACK) but no longer await a DB read.

3. `append_detached` (`routing.rs:953`) calls `context.ensure_verified(stanza).await` before `record_keyed_stanza_for_detached_bound_resource`. On `Err` it returns `Ok(false)` *without* an unkeyed append (preserving "rejected keyed copies cannot degrade to unkeyed appends"), and `deliver_to_detached` maps that to `Unavailable` exactly as today for a rejected key. Because `deliver_peer_to_full`/`deliver_direct_to_full` with `Some(ctx)` still go straight to the detached branch (`routing.rs:788-794`, `827-833`), the keyed-register-race invariant is untouched.

4. The live path (`deliver_ordered_local_copy`, `accept_live_delivery_inner`) and the forwarding/registered-remote paths accept a `Deferred` context unchanged, because they already authorize transactionally or hand off to a node that does. Task 2 adds a regression proving that a `Deferred` context with a canonical sender mismatch is refused by the live path (so no check is silently lost by removing the entry-point read). If the implementer finds any check in `check_canonical_obligation` that the live path does **not** repeat (compare `authorize_direct_stanza`, `authorize_resource`, and the MUC/carbon branch), the live path must call `ensure_verified` before `accept_live_delivery` rather than widening its own checks — report which case this was.

5. Measurement (acceptance): a `#[cfg(test)]` atomic read counter in `ingress/append_authority.rs` incremented inside `check_canonical_obligation`, plus assertions in the existing route-bridge tests: live-recipient relay performs **0** canonical reads at the receiver (was 1), the intermediate hop performs **0** (was 1, and the onward obligation is still forwarded), detached fallback performs exactly **1**. Tests run under `cargo nextest` (one process per test), so the static is race-free.

## Explicit non-changes

- No liveness probe anywhere. The branch that decides "detached" is the existing fallback order, and the read happens *inside* the append decision.
- No change to the receiver-side checks themselves, to the wire format, or to `deliver_ordered.vN`.
- `RemoteUserSideEffect::Carbons` receiver (`registration/side_effects.rs:299-317`) calls `into_context()` with no canonical read today; it is out of scope and reported as a follow-up in the PR.
- The socket node's second authorization of registered-remote frames stays (it is the socket's own fence).

## Tasks (TDD, seams = public interpret/route-bridge APIs and the existing fixture tests)

1. **Type + lazy authority.** Add `AppendAuthority`, `DeferredAppendAuthority`, `ensure_verified`, `IngressAppendObligationRef::into_deferred_context(db)`. Fix every constructor (`Verified`). Red test: unit test in `append_authority` that `ensure_verified` runs the canonical read once across clones and caches the rejection.
2. **Receiver entry points go sync.** Change `authorize_ingress_append`; keep NACK semantics. Red tests (extend `clustering/route_bridge/tests/ingress_append.rs`): (a) `live_recipient_delivery_writes_no_append_ledger_row` asserts 0 canonical reads; (b) `forwarded_obligation_survives_intermediate_hop` asserts 0 reads and the obligation still forwarded; (c) new `deferred_context_with_canonical_sender_mismatch_is_refused_live` for the live path; (d) existing `CanonicalAbsent` / `CanonicalSenderMismatch` / `ArchivePositionMismatch` cases in `ingress_append_authority` still produce `TargetUnavailable`/`Unavailable` with an empty queue — now via the detached branch — and assert exactly 1 read.
3. **Append decision verifies.** `append_detached` calls `ensure_verified`; keyed-register-race test (`routing_keyed_register_race.rs`) and `execute_relay_detached_tests` stay green; `keyed_detach_drain` tests unchanged.
4. **Docs.** `server/docs/operations/ingress-authority.md`: one paragraph in the cross-node keyed append section stating authorization is resolved at the append decision (live path transactional, detached path lazily, socket node for registered frames); update `TODO-ACTOR.md:65` entry.
5. **Verification.** `cargo fmt`; `cargo clippy --workspace --all-targets --all-features -- -D warnings` **and** default features; `cargo nextest run -p waddle-server` for `clustering::route_bridge::`, `ingress::`, `server::routes::interpret::`, `server::routes::websocket::tests::keyed_detach_drain` with `WADDLE_TEST_POSTGRES_URL` set so the Postgres halves run; full workspace nextest once at the end.
