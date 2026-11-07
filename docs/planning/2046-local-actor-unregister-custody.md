# Local actor-unregister admission custody

Status: production implementation plan; no replacement actor or P2 serving selector.

The active WebSocket lifecycle removes an exact local binding from `ConnectionRegistry` and then sends `EnqueueUserResourceUnregister` to `UserRegistryActor`. Ten production call expressions use this mirror. Nine discard a failed mailbox admission; the SM expiry janitor reports a failed sweep but its next pass can no longer find the removed registry entry. The actor's pending-unregister inventory begins only after mailbox admission. A stale actor resource can therefore remain without a retry owner.

## Ownership contract

1. Reserve bounded process-owned capacity for each local binding attempt before bind preparation publishes occupancy or retires an incumbent. Exhaustion fails bind before those side effects. Bind the reservation to the exact full JID and `LocalConnectionBindingId` before submitting the actor register ask. Same-socket retries and same-JID successors use distinct reservations.
2. An admitted register and its exact retirement share one custody record. The admission future keeps a drop guard through its final register attempt, even if connection state already owns the slot. Before any route removal or cancellable teardown await, arm retirement synchronously. It becomes ready for worker delivery only after the exact route is withdrawn and the register producer cannot issue another ask. Cancellation and panic retain that obligation; the worker never removes a live actor entry.
3. A supervised, bounded worker retries exact-token `UnregisterAndReleaseIfEmptyWithoutPendingRecord` through mailbox congestion, child BUSY, ambiguous replies, and cancellation. Keep custody until a terminal exact-token result; do not treat tell admission, timeout, absence of a reply, or process-local scheduling as completion. Do not await the worker while holding the per-JID bind or force-detach lifecycle locks.
4. Wire ordinary disconnect, successful and failed SM detach, terminal promotion, failed and closed resume, bind rollback, and SM expiry through the same contract. Preserve the existing force-detach acknowledgment and remote-owner semantics. Replacements must not be removed by a stale token.

## Verification and limits

Use deterministic barriers for capacity exhaustion before occupancy mutation; late register and cancellation on both sides of mailbox admission; route withdrawal versus worker readiness; same-JID successor; repeated same-socket attempts; full mailbox/child BUSY fairness; worker stop/restart; and each listed teardown path. Run native WebSocket/registry tests, formatting and strict scoped Clippy, then independent final-diff review.

This is bounded process-local actor-unregister custody. It does not prove durability across process restart, total boundedness of the pre-existing actor retry queue, remote-mirror retirement, or the joint P3 authority cutover. Those remain #2046 gates. GitHub Actions and workflow files remain disabled until the whole issue is ready for release qualification.
