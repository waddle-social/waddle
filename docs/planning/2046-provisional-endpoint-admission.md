# P3.1 provisional endpoint admission

Status: dark failure characterization and implementation inventory for
[#2137](https://github.com/waddle-social/waddle/issues/2137) under
[#2046](https://github.com/waddle-social/waddle/issues/2046). This slice does
not change production endpoint admission. The ignored regression is intended to
be enabled only after local and remote enqueue paths share the reviewed gate.

## Observed failure boundary

For XEP-0198 resume, the WebSocket registration path publishes a connection
entry and mirrors it into `UserActor` (and, when the account is remote, into a
remote-resource proxy) before `finalize_sm_after_registry_registration`
completes the resume claim. If claim completion discovers a replay gap,
`reset_resume_attempt` removes that binding, clears the resumed state and
returns `<failed/>`. The replacement task's sender has already been taken from
`pending_tx`; if the registry held its last clone, `outbound_rx.recv()` returns
`None` and the connection task takes the superseded-socket exit path. XEP-0198
§5 says a server SHOULD allow resource binding after the documented resume
failure cases, so the same authenticated WebSocket needs to remain usable for a
fresh bind.

The dark regression in `tests/xep0198_attached_resume_ws.rs` pauses after
publication and before claim completion, injects a replay gap into the claimed
session, then tries a fresh bind on the same socket and same full JID. It also
releases a delayed cleanup carrying the failed binding token and checks that a
new binding still owns the slot. The test remains ignored because the current
production behavior is expected to fail it. Its resource is deliberately
`phone` on both binds; using a different resource would not test stale cleanup
against a same-JID successor.

## Existing predecessor custody

The failed resume's new binding is different from the predecessor socket.
Before `register_entry` publishes a successor, `prepare_socket_bind` holds the
per-JID bind gate, asks a running incumbent to detach, waits for its
`SocketLifecycle` to stop, and completes retained generation retirements. In a
clustered build it also awaits remote incumbent retirement. The detach path
closes the old receiver and drains its accepted outbound queue into the
detached replay/recovery path before reporting lifecycle completion. Thus the
old socket task remains the owner of its queue and accepted predecessor work
through retirement; replacing the local registry map entry is not the step
that discards that queue.

That boundary does not cover work admitted to the *new* entry between
publication and resume-claim completion. The current path makes that entry
routable during the gap. A later failed-claim rollback unregisters it without
transferring any already-accepted, unwritten work from its queue. `OutboundStanza`
can carry a pending-row identity, a write-acceptance callback, or an ingress
append obligation, so keeping the old sender alive or moving queued envelopes
to a fresh channel is not a safe substitute for admission control.

## Enqueue and lifecycle path inventory

| Boundary | Current code | Requirement before production cutover |
|---|---|---|
| Local publication and resume completion | `server/routes/websocket/registration.rs`; `stream_management/registration.rs` | Publish the exact entry as non-admitting. Activate only after successful claim completion and final authority checks. On failure, retire that exact entry and reset for same-socket fresh bind. |
| Local direct, accepted, pending and fanout sends | `waddle-xmpp/src/registry/connection_registry/sending.rs` | Serialize admission with the actual channel enqueue. Async sends must reserve capacity first, then recheck admission while committing the permit. Preserve pending-row and write-acceptance ownership on a pre-enqueue retry. |
| Owner-gated and ordered local sends | `connection_registry/archive_dispatch.rs`, `connection_registry/sending.rs` | Apply the same check before archive-frontier advancement and before queue acceptance. A rejected provisional send must not mutate the frontier or evict the entry. |
| Peer, direct and pending actor sends | `waddle-xmpp/src/registry/user_actor/delivery.rs` | Return a typed pre-enqueue retry from the shared entry, with no alternate-resource or offline fallback at this boundary. Actor and local registry clones must observe the same admission state. |
| Remote resource proxy | `waddle-server/src/clustering/route_bridge/registration/owner.rs`, `registration/socket_forwarder.rs`, and `delivery/receiver.rs` | Add generation-fenced remote activation and retirement using the existing JID, remote registration ID, socket generation and socket-node identity. Do not treat a remote proxy's local binding ID as a physical `ConnectionRef`. Carry pre-enqueue retry semantics through relay acknowledgements without diverting the frame to another channel. |
| Failed claim and delayed cleanup | `stream_management/registration.rs`, `registration.rs`, `cleanup.rs` | Keep the pending claim token until activation. Fence detach, interruption and rollback by the exact binding/generation so delayed cleanup cannot retire its successor. |

All these routes need one typed `RetryBeforeEnqueue(binding)` outcome. It must
be produced before channel acceptance, archive-frontier mutation, or a positive
delivery acknowledgement. A route-selection check alone is too early: a
producer may wait for channel capacity and race with retirement before the
actual enqueue. Ordered relay reply changes require a `deliver_ordered.vN` bump
under `server/AGENTS.md`.

## Qualification still required

- Close every direct, peer, pending, ordered and remote enqueue bypass before
  enabling provisional entries in the production registration path.
- Preserve a pending SM claim and all accepted-but-unwritten obligations across
  successful activation, failed claim, rollback, transport interruption and
  delayed cleanup.
- Prove same-socket fresh bind after replay-gap failure, exact stale-cleanup
  fencing, remote proxy activation ordering, and queue saturation with
  deterministic barriers.
- The current dark regression covers the replay-gap/fresh-bind and delayed
  local cleanup boundary only. It does not prove accepted-but-unwritten
  obligation custody or cross-node activation.
- The ignored post-publication regression is still a characterization, not a
  passing production-path test. See the draft PR for exact executed test and
  review evidence on the current head.
