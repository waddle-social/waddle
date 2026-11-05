# P3.1 dark actor-owned local endpoint

Status: candidate implementation contract for #2137 under #2046, based on
merged main
`7610378cd98c87bfad34f02e931ef2ae2067543d` (2026-11-05). This plan
authorizes no `/ws` lane switch, remote-proxy change, endpoint DDL, or feature
advertisement. The proposed PR is independent of #2116's database-role work.

## Source boundary and decision

**NO-GO for implementation until independent review accepts the exact local
publication, custody and extraction contracts below; NO-GO for production
cutover.** A dark local slice may proceed after that review without #2116.

- Production `/ws` still creates the outbound channel and holds the socket
  halves, `WsConnState`, timers, force-detach receiver and outbound receiver in
  `server/routes/websocket/connection.rs`. `batch_write.rs` borrows the sink,
  reader and mutable SM state together, including acknowledgements during a
  paced batch. Moving only the sink or registry entry would leave two writers
  of output order and SM state.
- `connection_actor_core.rs` consumes a `WsConnState`, but its Kameo actor is
  exported only under `test-support` from `connection_actor.rs`. The actor's
  real-socket fixture proves reserved pumps, bounded continuations, pre-bind
  transitions and handled-prefix scheduling; it does not route general stanzas
  or own a published production endpoint.
- `waddle-xmpp` has exact `ConnectionRef`/`BindingRef` identities and a
  provisional/active/sealed endpoint with queue-and-admission-bound permits.
  Production `ConnectionEntry` construction remains active by default. Local
  retry-before-enqueue returns the original `OutboundStanza` on a proven
  refusal; queue acceptance remains accepted-or-unknown. Registry replacement
  does not seal an old active endpoint. `ProvisionalConnectionEntry` cannot be
  published to `ConnectionRegistry` or `UserActor` without activating it first;
  `register_entry` and `RegisterConnection` overwrite their slots without an
  exact provisional CAS.
- Registration in `registration.rs` publishes the entry and UserActor mirror
  before `finalize_sm_after_registry_registration` completes a resume claim.
  Its failed-claim reset can drop the registry's last sender, making the live
  loop treat `outbound_rx.recv() == None` as physical supersession. The
  post-publication same-socket regression in
  `tests/xep0198_attached_resume_ws.rs` remains ignored. The issue's local
  repro is not hosted or production evidence.
- `supervised_io.rs` is an inactive, independently joined real WebSocket pump
  boundary. `LocalSocketIo` is the current production half-drop witness;
  `SocketLifecycle::Detached` records binding cleanup and does not prove
  physical termination or pump joins. Its `send_queued` takes the payload from
  the request immediately before `start_send`; `WriteReceipt` tracks a local
  milestone but cannot recover a full `OutboundStanza` after cancellation.

## One PR: actor-owned local endpoint and physical supervisor

Extract a **small** production-compiled owner from the 3,136-line,
`test-support`-only `connection_actor.rs`: physical socket reservation/start,
exact endpoint state, accepted-envelope custody and retained cleanup. Keep
synthetic auth/effect protocols, scripted outputs, barriers and the rest of
the actor fixture behind `test-support`; do not make the whole fixture a
release module or suppress dead-code warnings across it. Narrowly justify
any unavoidable dormant-item lint at the item, not module, level. The owner
must compile in the release feature set, but `/ws` must not select it. Extend
the **existing** `tests/supervised_io/connection_actor.rs` harness to invoke
this owner with a local registry and UserActor; do not build a second lab or
shadow owner. Its physical-split proof must use the production
`SocketAdmission::start_socket` path on an actual upgraded Axum WebSocket,
not only a generic factory or the test-only split hook. Test-only hooks may
still pause later boundaries. Leave `connection::router` and production
registration callers untouched.

1. Give the extracted owner one `ConnectionRef`, its current actor-issued
   `BindingRef`, the `SocketOutboundEndpoint` and sole receiver, and the
   reserved `IoCustodianHandle`. `SocketAdmission::start_socket` must split
   the real socket once under its source/lifecycle guards. The owner retains
   cleanup custody and both pump join results; no raw sender or independently
   mutable `WsConnState` escapes to a registry, pump or callback.
2. Replace the activation-before-publication limitation with a distinct,
   publishable **provisional** entry wrapper accepted only by new dedicated
   local registry and UserActor methods. Neither `register_entry` nor
   unconditional `RegisterConnection` is an acceptable shortcut. The
   candidate carries one tuple: `ConnectionRef`, actor `BindingRef`, registry
   `LocalConnectionBindingId`, current `NodeLeaseIdentity`/admission permit,
   full JID and expected predecessor owner (or vacant slot). Revalidate the
   current permit before publication and before the mirror request. Publish
   under the local bind gate and registry shard CAS; the actor mirror performs an
   exact expected-owner CAS and acknowledges the same candidate. Both maps
   must refer to the same admission object, initially non-admitting.
3. Activation uses an opaque success capability tied to that tuple. After
   bounded mirror confirmation, check current lease/admission and exact
   registry and UserActor ownership again; linearize provisional→active under
   the registry publication lock and endpoint admission gate. The dedicated
   replacement/removal paths seal the old endpoint **before** making a new
   entry visible, including captured clones. Existing unconditional registry
   and UserActor replacement APIs must also seal or refuse an actor-owned
   candidate; otherwise they bypass this fence. Legacy active entries keep
   their present behavior. Because the two maps cannot be
   updated atomically, any timeout, cancellation, failed CAS or ambiguous
   actor ask leaves the candidate provisional/sealed and an independent
   cleanup owner retains exact unregister/retry work until both sides confirm
   retirement. Never activate based only on an earlier mirror reply. No
   production caller can mint the activation capability in this PR; the
   existing harness has a test-only success capability. A failed attempt may
   reuse the physical socket with a fresh `BindingRef` only after the old
   candidate is sealed, its queue is empty, and exact control identity is
   retired. Preserve distinct occupancy and SM identities.
4. Once an envelope is accepted, hold the **entire** `OutboundStanza` with
   its exact tuple, callback, pending row, receipt time and ingress obligation
   in a bounded owner-held custody table. Endpoint queue, actor mailbox and
   writer queue carry a stable local ID, never sole ownership of the envelope.
   Count and byte permits cover queued, in-flight and terminal-retained work.
   The writer may serialize a derived frame, but the original envelope stays
   in custody through the `start_send` boundary: `send_queued` currently
   consumes the write value before that call, so `WriteReceipt` alone is
   insufficient. On cancellation before attempt, the cleanup owner retains
   and classifies the exact envelope as proven not attempted, with a typed
   retrieval/compensation path even if the caller future has vanished. After
   attempt, retain it as uncertain until an explicit terminal owner settles
   it. Local flush completion is not client acknowledgement or an SM/durable
   custody transfer. Activation remains
   test-only; the harness exercises full envelope metadata through the entire
   path but cannot acknowledge or settle external obligations as durable.
5. Seal before stop, prevent further commits of reserved permits, interrupt
   stalled writes and atomically transfer all accepted IDs/envelopes into a
   bounded terminal inventory. Join both pumps before reporting local
   termination. Control must remain responsive while a data mailbox, effect
   or socket write stalls. Cancellation or panic must leave discoverable
   cleanup custody. Do not infer termination from abort, channel close,
   `SocketLifecycle::Detached` or the half-drop witness. This is process-local
   custody only; no crash-recovery or durable-delivery claim follows.
6. Delete only fixture startup/endpoint glue replaced by this small owner;
   the existing tests call the extracted path. Keep `/ws`, `batch_write`,
   active endpoint constructors, `dual_registration` and remote-resource
   relay until production replacements are authoritative.

The local publication lock order is **per-JID bind gate → registry shard →
endpoint admission**; release shard and admission locks before any UserActor
ask, storage operation or socket await. The UserActor handler may lock its
entry's endpoint admission while executing its mailbox turn, but must not
call back into the bind gate or registry. A bounded mirror ask occurs while
the independent startup owner retains the bind gate and cleanup ticket;
its reply is evidence to recheck, not the activation linearization point.
Activation revalidates the current lease and exact tuple before entering the
registry shard/admission critical section. A replacement that races after
the mirror reply must seal the candidate before its own publication. Timeout
or cancellation hands the ticket to an independent cleanup owner before the
caller can drop the last reference; retry outcomes distinguish confirmed
retirement, stale successor, and completion unknown.

Primary code ownership: `server/crates/waddle-server/src/server/routes/websocket/`
(`connection_actor.rs`, `connection_actor_core.rs`, `supervised_io.rs`, a small
private runtime module if needed),
`server/crates/waddle-server/src/websocket_transport.rs`,
`server/crates/waddle-server/src/test_support/`, and
`server/crates/waddle-xmpp/src/registry/connection_registry/`
(`endpoint.rs`, `connections.rs`, `binding.rs`) plus the exact
`user_actor/implementation.rs` provisional mirror methods. Use a small private
component rather than a second actor runtime or a generic routing framework.
Touch `user_actor/delivery.rs` only for a proven local typed-result gap; do not
change
`clustering/route_bridge`, SQL schemas, `connection.rs` or #2116 files in this
PR. Document any unavoidable overlap before implementation.

## Acceptance for this PR

- Extend `tests/supervised_io/connection_actor.rs` with real native WebSocket
  schedules using `SocketAdmission::start_socket`: provisional refusal returns
  the exact envelope and leaves the queue empty; exact activation yields
  ordered direct-frame output; stale activation/retirement cannot affect a
  same-JID successor; failed binding then fresh binding on the same physical
  socket uses a new `BindingRef`. Prove both physical pump joins and no second
  holder of either half.
- A dedicated registry/UserActor test must publish a still-provisional entry
  without calling `ProvisionalConnectionEntry::activate_binding`, then confirm
  both maps have the same admission object and exact tuple. Race activation
  with predecessor replacement, stale mirror reply, current-lease revocation,
  actor ask timeout and caller cancellation. Each loss leaves no active stale
  endpoint; an ambiguous mirror write retains exact compensation until its
  final state is observed. Existing unconditional replacement APIs cannot
  bypass sealing for an actor-owned entry.
- Barrier tests cover a held capacity permit racing seal, cancellation before
  and after split, actor panic, reader/writer panic, stalled `poll_ready` and
  post-`start_send` flush, lease loss, shutdown deadline and both task joins.
  Check count and byte budgets separately and verify control responsiveness
  with all 256 actor data slots occupied.
- Exercise local registry and UserActor mirrors with the same candidate
  object. Assert no provisional stanza reaches the receiver, no archive
  frontier advances on refusal, and an old cleanup cannot remove a successor.
  Inject full `OutboundStanza` metadata and assert exact retention at each
  endpoint→mailbox→writer transition and after cancellation before `start_send`
  or after an attempted stalled flush. Verify callback is never acknowledged
  by queue acceptance, `WriteReceipt` alone, or local terminal inventory.
  A full terminal inventory rejects new work at its count or byte bound.
- Keep `tests/xep0198_attached_resume_ws.rs`'s ignored production regression
  ignored until local **and remote** enqueue paths, failed-claim rollback and
  accepted-work custody are complete. Run its existing non-ignored XEP-0198
  cases and focused `characterization_bind.rs`/`characterization_resume_muc.rs`
  cases to detect accidental active-path changes. Run default and clustering
  feature compilation, formatting and strict scoped Clippy. Native WebSocket
  tests establish local behavior only; no cross-node or deployment claim.

## Production cutover gate

Before selecting this owner from `/ws`, complete full inbound stanza routing,
pre-bind/auth, bind/rebind, SM replay and contiguous handled-prefix parity,
including paced-batch acknowledgement handling and replay-before-live output.
Every mutable protocol field must have one owner; remove the corresponding
`LiveSocketState`/`WsConnState` loop writers in the same cutover. Close direct,
peer, pending, ordered and remote-proxy enqueue bypasses with exact-binding
retry and generation-fenced activation. Specify accepted-but-unwritten custody
and uncertain completion across crash, not just an in-memory inventory. The
remote route uses its own registration ID, socket generation and node identity;
its local binding ID is not a physical `ConnectionRef`. Any ordered-relay reply
shape change requires a `deliver_ordered.vN` bump.

#2116/#1986/#2118/#1948 are prerequisites for affected durable and remote-room
custody, while #2138/#2139 must agree on account-resource and durable SM
identity at the coupled activation boundary. Resolve endpoint publication/CAS,
rollback, mixed versions, supported SQLite behavior, workload budgets and
exact-release multi-process qualification separately. A stopped pump is not a
delivered stanza; an SM unacked append is a different recovery milestone.
Review the relevant XEP-0198 and XEP-0045 sources and dedicated suites before
changing wire behavior or feature advertisements. This PR changes neither.
