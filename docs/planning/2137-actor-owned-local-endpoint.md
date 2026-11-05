# P3.1 dark actor-owned local endpoint

Status: implementation plan for #2137 under #2046, based on merged main
`7610378cd98c87bfad34f02e931ef2ae2067543d` (2026-11-05). This plan
authorizes no `/ws` lane switch, remote-proxy change, endpoint DDL, or feature
advertisement. The proposed PR is independent of #2116's database-role work.

## Source boundary and decision

**GO for a dark, local vertical slice; NO-GO for production cutover.**

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
  does not seal an old active endpoint.
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
  physical termination or pump joins.

## One PR: actor-owned local endpoint and physical supervisor

Promote the **concrete** actor startup, physical cleanup and local endpoint
state machine already present in `connection_actor.rs` into a private,
production-compiled ownership component. Keep scripted authentication, fake
effects, barriers and synthetic outputs behind `test-support`. The new code
must compile in the release feature set and own real WebSocket halves; it is
deliberately unselected by `/ws` until its protocol and custody contract is
complete. Extend the existing `tests/supervised_io/connection_actor.rs` harness
to exercise the promoted component with a local `ConnectionRegistry` and
`UserRegistryActor`; do not build a second lab or shadow owner. Leave
`connection::router` and its production registration callers untouched.

1. Give the production-compiled actor-owned connection a typed
   `ConnectionRef`, its current `BindingRef`, a provisional
   `SocketOutboundEndpoint` and the sole receiver,
   plus the reserved `IoCustodianHandle`. Move the real split socket halves to
   the supervised pumps exactly once. The controller retains cleanup custody
   and both join results; no raw sender or independent mutable `WsConnState`
   escapes to a registry, pump, callback or test adapter.
2. Add a private local endpoint lifecycle API that prepares a candidate,
   publishes only that exact provisional entry into the isolated
   registry/mirror, and seals it before retirement. The actor decides each
   transition and the registry validates and applies it under its publication
   lock; stale owner, stale binding, lease-loss and cancelled activation fail
   closed. Activation requires an opaque exact-binding success capability;
   no production caller can mint one in this PR. A test-only capability proves
   the activation race through the existing harness. The independent startup
   owner, not a blocked actor turn, awaits bounded registry and UserActor
   confirmation and retains compensation on ambiguous outcomes. A failed
   attempt may reuse the physical socket for a fresh `BindingRef` only after the old
   candidate is sealed, empty and its exact control identity retired. Keep
   occupancy and SM identity separate from physical connection identity.
3. Route the existing harness's local direct-frame output through the promoted
   endpoint owner to the actor and its supervised writer, with a closed, typed
   disposition for
   proven pre-enqueue refusal, accepted/unknown, not attempted, attempted
   uncertain and completed local write. Preserve the complete
   `OutboundStanza` envelope on refusal and in any local terminal inventory.
   The test-only capability may activate only a documented
   no-durable-obligation profile;
   any pending-row, callback or ingress obligation requires explicit retained
   custody before accepting it. No result means client acknowledgement or
   durable responsibility.
4. Seal before stop, prevent further commits of reserved permits, interrupt
   stalled writes, retain accepted-but-unwritten envelopes with their exact
   binding, and join both pumps before reporting local termination. A bounded
   control path must work while a data mailbox, external effect or socket
   write is stalled. Cancellation or actor panic must leave a discoverable
   cleanup owner. Do not infer termination from abort, channel close,
   `SocketLifecycle::Detached` or the half-drop witness alone.
5. Delete the superseded fixture-only startup/endpoint glue in the same PR;
   the existing test harness must call the promoted component. Keep the current
   `/ws` loop, `batch_write`, active
   endpoint constructors, `dual_registration`, and remote-resource relay paths
   until their respective production replacements are authoritative.

Primary code ownership: `server/crates/waddle-server/src/server/routes/websocket/`
(`connection_actor.rs`, `connection_actor_core.rs`, `supervised_io.rs`, a small
private runtime module if needed),
`server/crates/waddle-server/src/test_support/`, and
`server/crates/waddle-xmpp/src/registry/connection_registry/`
(`endpoint.rs`, `connections.rs`, `binding.rs`). Use a small private component
rather
than a second actor runtime or a generic routing framework. Touch
`user_actor/delivery.rs` only for a proven local typed-result gap; do not change
`clustering/route_bridge`, SQL schemas, `connection.rs` or #2116 files in this
PR. Document any unavoidable overlap before implementation.

## Acceptance for this PR

- Extend `tests/supervised_io/connection_actor.rs` with real native WebSocket
  schedules: provisional refusal returns the exact envelope and leaves the
  queue empty; exact activation yields ordered direct-frame output; stale
  activation/retirement cannot affect a same-JID successor; failed binding
  then fresh binding on the same physical socket uses a new `BindingRef`.
- Barrier tests cover a held capacity permit racing seal, cancellation before
  and after split, actor panic, reader/writer panic, stalled `poll_ready` and
  post-`start_send` flush, lease loss, shutdown deadline and both task joins.
  Check count and byte budgets separately and verify control responsiveness
  with all 256 actor data slots occupied.
- Exercise local registry and UserActor mirrors with the same candidate
  object. Assert no provisional stanza reaches the receiver, no archive
  frontier advances on refusal, and an old cleanup cannot remove a successor.
  Preserve pending-row, callback, receipt and ingress metadata in refused or
  retained envelopes. Any activated no-obligation test frame must be explicit.
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
