# Exact local binding permit for SM enable

Status: implementation and verification record for #2046 and #2137. The
reviewed scope is a local pre-`start_send` fence. It does not select durable session authority or change
resume admission, SM acknowledgement, or output custody.

## Problem

A registered socket can be replaced or removed while its `<enable/>` response
is waiting to enter the transport. The node admission permit and physical
socket interruption do not by themselves establish that the same full-JID
registry binding still owns the response. `prepare_socket_bind` holds the
per-JID bind gate while asking the incumbent to stop and waiting for its
lifecycle. The incumbent must not wait for that gate to send or cancel.

The post-start boundary is different. A successful `Sink::start_send` is only
an admitted local transport attempt; a later flush error, cancellation, or
replacement does not prove whether the peer saw `<enabled/>`. This slice does
not resolve that uncertainty or make it safe to release an uncertain session.

## Intended boundary

1. Add an opaque, revocable permit for one exact local registry binding. A
   permit captures full JID, `LocalConnectionBindingId`, `BindingRef`, and
   `ConnectionRef` from the actual published entry. It cannot be constructed
   from a caller's assertion. Clones of that entry share one monotone
   active-to-revoked cell and one cancellation notification; a replacement or
   a later binding on the same physical connection receives a different cell.
2. Acquire the permit synchronously while verifying the registry's exact entry.
   Never wait for the per-JID bind gate from the incumbent enable path. Route
   removal or replacement revokes the old cell under the same route shard, so
   it is ordered against acquisition and synchronous permit use. Revoke an
   incumbent after an exact force-detach offer is accepted, before a binder
   waits for its lifecycle; a failed or stale offer does not prove detachment.
   It is not a socket-stop or cleanup-completion witness.
3. Preserve identical-entry republication without revoking its own cell.
   Review every registry mutation, including the actor/provisional paths,
   `register_entry`, `register_socket_entry`, conditional registration,
   unconditional, owner and SM-session removal, and both closed-sender
   evictions in `sending.rs`. No-op stale-owner removals must never revoke a
   successor. Remote mirrors cannot mint a local physical-binding permit.
4. Once the existing enable handler has returned a **known committed** stream
   enrollment and produced its pending enable response, obtain the exact
   permit before the response write. Do not race or cancel the existing claim
   acquisition or enrollment transaction, including its COMMIT: this change
   must not turn an uncertain database outcome into a failed or rolled-back
   one. If preserving that boundary is not possible, limit implementation to
   the registry/transport primitive and leave it unselected by SM enable.
5. Park transport readiness without holding the bind gate, route shard,
   permit mutex, or a database transaction. The binding notification may
   cancel the pre-start readiness wait. At the synchronous `start_send`
   boundary, combine the current node admission check, exact binding permit,
   and existing physical-socket stop fence. The permit mutex is held only over
   the immediate start operation; it must not re-enter registry or permit
   code. The sink's own interruption mutex is distinct. Audit the lock order
   against publication, force detach, and node revocation before integration.
6. If revocation wins before `start_send`, do not start `<enabled/>` and exit
   the old transport. The pending claim guard attempts to inventory exact claim
   release; the existing orphan-retirement sweep retries the enrolled stream. If
   `start_send` wins, retain the existing post-attempt transport and session
   behavior: this permit says neither that flush completed nor that the peer
   received the response. A later revoke cannot retroactively invalidate the
   already-admitted attempt.

No SQL migration, registered selector, epoch transition, role change, or
workflow change is part of this slice. In particular, this local permit is
not a principal/SM claim check, durable binding record, process-restart fence,
or authorization to serve after remote ownership changes. Other force-detach
producers retain their own existing authorization and cleanup contracts; any
path claimed to revoke on an accepted request must do so for the exact binding.

## Verification

- Deterministic registry tests for both winners of revocation versus synchronous
  start, exact capture and mismatched resource/owner/binding, same-entry
  republication, all replacement/removal/closed-eviction variants, stale cleanup,
  separate entries, and a fresh binding on the same physical connection.
- Accepted, full, and closed force-detach offers, including a stale captured
  binding. An accepted offer wakes the incumbent without taking the bind gate;
  a failed offer provides no detach proof and does not revoke an unrelated cell.
- Real WebSocket enable at the selected boundary with deterministic barriers:
  normal success, replacement or owner removal before start, and start before
  replacement. Verify the current binding and later binding are not clobbered.
  Do not treat a stalled or failed flush as proof the enabled frame was unseen.
- Targeted XEP-0198 and registry regressions, formatting, and strict scoped
  Clippy. Coordinate Cargo use with the other #2046 work. Report separately
  any environment or pre-existing check limitation.

## Lock order and retirement evidence

The real WebSocket start acquires node admission read guards, the socket-control
binding mutex, the binding cell, and the physical socket interruption gate in
that order. The inner
`futures-util` 0.3.32 `SplitSink::start_send` stores its local slot; it does not
acquire its `BiLock` or call the route registry. Replacement holds the route
shard, then the old binding cell, then the new socket gate; this order also
covers two bindings on the same physical socket. Refused socket publication
leaves the old binding active. Actor sealing takes endpoint admission followed
by actor custody; `ActorOutboundCustody::attempt_with` releases its custody
state before executing its start callback. The binding start callback does not
re-enter the registry or take endpoint/custody locks.

Force detach uses Tokio 1.54.1's nonblocking `try_reserve` while holding the
binding cell. Success revokes the cell; Full and Closed do not. After unlocking,
the code notifies parked writers and sends the reserved request. The actual
receiver wake can run a reentrant waker synchronously and is never called under
the binding cell. Reservation success is an accepted but unconfirmed offer;
receiver closure afterward is not evidence of completed detachment. A binder
continues waiting for observed lifecycle stop while holding its normal bind
gate, but the incumbent never waits for that gate.

The socket owner must successfully wire its live control before registration;
resumable enable capture additionally checks that same control's exact full JID
and binding attempt. A failed wiring or missing/mismatched control closes the
attempt before `<enabled/>` starts. User demotion, stale-actor retirement, and
non-fresh socket-relay offers revoke only after this socket-owned identity proof
and requester bare JID agree; otherwise they keep the legacy raw offer and ACK
behavior, including remote mirrors. The control-binding mutex is acquired before
the short permit cell and released before notification or queue wake; the
`start_send` and registry-removal paths do not take that control mutex while
holding the permit.

Only `resume='true'` uses this fence, after the existing claim and enrollment
futures have completed successfully. Neither the binding signal nor the
readiness wait cancels the enrollment COMMIT. For a known committed enrollment
whose response is refused before start, dropping the pending enable guard
attempts to inventory exact claim release. When that succeeds, the SM janitor
retries the release, scans durable ingress streams, and retires the exact
unprotected row. A failed inventory emits a warning and does not prove release.
The clustering PostgreSQL path additionally fences claim absence in its
retirement transaction. Cleanup depends on ownership reconciliation, the
janitor, and the backend; the guard does not delete the row itself.

## Later gates

A durable source binding needs the reviewed P3 session/resource CAS, exact
principal and claim authority, same-attested-pool epoch/ACL policy, cross-node
writer and old-binary fencing, and stream/source retention. Unknown COMMIT
needs the original PostgreSQL backend and authoritative-primary reconciliation;
closing a checkout or seeing an absent row is not rollback proof. Full
producer-to-SM custody still requires immutable pre-writer output intents,
stream-wide sequence, replay and authoritative ACK/transfer handling.
