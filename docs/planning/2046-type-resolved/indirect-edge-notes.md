# Bounded indirect-edge candidates and source review

Source target: `4219bdd71af9fb0116c82a5c33ab1ed98eecc35d`. The original
review used `718f47a9e08f29cf2e6157803770e2f495fe201e`, whose server tree
matched `5763923e`; the changed owner-update ordering paths were re-inspected
for this refresh. The inputs were [A01–A31](actor-mirror-edge-ledger.md), the
[WebSocket borrowed-write narrative](ws-state-semantic-inventory.md#borrowed-writes-and-callers),
selected paths from the [independent discovery](independent-edge-discovery-4219.md),
and selected paths from the [supervised-I/O source review](supervised-io-review-4219.md).
This is not an independent all-source discovery or a complete caller inventory.

The two TSVs contain **241 individual links**: 116 from 29 actor/relay ledger
groups, five owner-update queue links, 32 selected WebSocket borrow and
control-flow links, and 26 production links reviewed from the independent
discovery and its immediate remote-presence helper chain. The latter comprise
three rollback, 19 room-presence/routing/outbox, and four carbons links. The
remaining 62 links cover selected test-only, test-support and dormant paths:
20 from the independent discovery and 42 supervised-I/O fixture and supporting
owner/custody links. The
[candidate table](indirect-edge-candidates.tsv) is the seven identity columns
projected from the [review table](indirect-edge-reviews.tsv); the matching rows
are not an independent second discovery. Each row names a concrete call site,
message ask/tell and handler, explicit turn drop and destructor, or, in A16,
the shared-entry state connection.
`kind` records the observed source gate and edge mechanism, while `operation`
and `classification` use the validator's vocabulary. A call or message edge
can exist without the preceding branch being selected or the operation
succeeding. The tables do not establish runtime reachability, socket delivery,
distributed ownership, or completed custody.

A18 and A30 are deliberately absent as edges. The [registry retirement
note](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1661)
identifies removed forwarding messages; the [reserved identity codecs](../../../server/crates/waddle-server/src/clustering/relay/resource_identity_wire.rs#L1)
explicitly have no Kameo handlers. Neither supports inventing a current
sender-to-handler link, and the latter does not make older registered IDs
obsolete.

## Gates and interpretation

- Cluster rows require the [`clustering` feature](../../../server/crates/waddle-server/src/clustering/mod.rs#L71)
  and the particular source branch, service, claim, registration, and runtime
  conditions. Remote asks cite the matching registered handler, including both
  live and baseline compatibility variants where present. Compatibility can
  retry only under its explicit conditions; it is not unconditional fanout.
- The five added A23 queue links cover the explicit admission, initial grant,
  readiness wait, turn destructor and successor grant. Interior cancellation
  removes only that turn. Poison drains ungranted senders when a later admission
  or turn drop observes the lock; granted heads may finish, and poison alone
  need not immediately wake waiters. These selected links are not exhaustive
  discovery of queue internals or other remote update paths.
- The rollback links distinguish permit or shutdown failure before and after
  the registration mirror. The helper attempts owner-qualified mirror removal
  through a bounded tell; enqueue admission is not completed removal.
- Room presence tries the registered-remote wrapper first. Its `Retryable`
  result is treated as handled, while `Absent` reaches the local helper. The
  local helper retries `DroppedFull` once, logs and counts a second drop, and
  then returns `true`, suppressing the later clustering fallback. Its preceding
  [comment](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L425)
  says persistent full is not reported as success, but the
  [return](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L456)
  is `true`; this inventory records the code's branch behavior. The registry's
  `Delivered` result maps from `AcceptedOrUnknown`, not proof of a socket write.
- The room outbox waits for local writer-acceptance receivers on its non-inline
  completion path. Only a `NotConnected` local result tries the remote
  write-accepted helper; without clustering that helper returns `Absent`.
  Enqueue alone does not establish writer acceptance or row completion.
- Carbon planning selects from the recipient inventory; registry-only planners
  without a user actor registry use a separate registry fallback. The registry
  fanout helper reads enabled entries independently. Selection is not evidence
  that a carbon was sent.
- `test-actor-ask` records observed `cfg(test)` unit-test senders to otherwise
  release-compiled handlers; `test-integration-actor-ask` names source in
  integration-test targets. The selected ingress integration modules have no
  `test-support` source gate; the server's
  [development dependency](../../../server/crates/waddle-server/Cargo.toml#L159)
  enables XMPP `test-utils` for those tests. The physical continuation integration sender is
  itself [`test-support` gated](../../../server/crates/waddle-server/tests/physical_continuation.rs#L1).
  A31's publication asks are also `test-support` gated and the child handlers
  require `test` or `test-utils`; `test-support` enables the latter in the
  [server features](../../../server/crates/waddle-server/Cargo.toml#L176).
  A31 retirement is separately labeled dormant: the endpoint's own
  [module documentation](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1)
  says `/ws` does not select it. These facts do not establish live bind use.
- The additional supervised-I/O cases use a `test-support`-gated `/actor-owner`
  fixture over an in-memory duplex WebSocket. Publication and confirmation
  operate on a provisional candidate; error or a vanished waiter causes a
  separately owned retirement attempt. The failure result can reach the waiter
  before retirement finishes, and actor timeout or stop alone is not evidence
  of compensation. The fixture source separately waits for registry/actor
  cleanup and joined pumps in its publication-loss scenarios.
- The direct-custody handler can return `AcceptedOrSettled` after local
  acceptance or existing settlement; neither proves a physical write. The
  supervised fixture source distinguishes a peer frame and `Flushed` stage
  from its still-unresolved acceptance receiver. Other fixture assertions
  distinguish `NotAttempted` from `AttemptedUncertain` and retrieve retained
  envelopes only after joined cleanup. These describe source assertions, not
  executed tests, safe replay of an uncertain write, or completed settlement.
- The actor driver and generic inbound core are release compiled but dormant
  for `/ws`; their physical-driver test fixture has a separate `test-support`
  gate. Calls that create the core or lend its mutable state to the frame
  handler do not establish that a live producer selects that path.
- A16 is a **shared-state** connection, not a message from the presence
  handler to the routing handler: registered entries share atomics as described
  by the [registration contract](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1077).
  Conditional owner-gated presence writes can therefore be observed by actor
  selection reads. Actor-local full presence and registry presence are not
  assumed to be the same store.
- The IQ paths retain the conditions at their eventual mutations: carbons
  writes the borrowed flag [after owner validation](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/sans_io.rs#L170),
  roster get does so [after snapshot and owner validation](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/roster/implementation.rs#L81),
  and blocking get [after its owner-gated mark](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/iq/blocking.rs#L56).
  The presence context does not lend the separate socket presence carriers.
  Resume restores fields only on its successful path; terminal recovery's
  socket snapshot is explicitly non-resumable. These are conditional source
  effects, not unconditional writes on every invocation.

## Omitted work and evidence limits

The WebSocket rows trace the narrative's selected borrow-to-callee chains, not
all 60 mutable-borrow compiler spans or all aliases of every shared handle.
Further discovery is required for every source caller and each relevant build
profile, including other registration and teardown callers, all selection
consumers and their distinct fallback behavior, actor lifecycle/claim rotation,
subscriptions and carbons operations, remote update and side-effect downstream
fanout, ordered relay and room delivery, receipts and detached/physical custody,
and alternative tests and support harnesses. The added test and dormant paths
are a selected subset, not a complete review of fixture calls, marker rows or
all test configurations. A test of a release-compiled handler does not prove
production reachability; a retry or enqueue result does not prove drain or a
physical socket write. These notes leave all such work open rather than
treating the candidate list as exhaustive.

Manual source inspection checked the cited message implementations, selected
senders, wrappers, borrowed contexts and relevant feature gates. The inventory
edge-validation helper reconciled candidate and review keys, unique IDs,
operation/class labels, source-commit values and cited source locations for all
241 supplied links. The full validator also reparsed both retained compiler
streams and reconciled the current 241 supplied links. Independent discovery
and semantic review remain integration work. No Cargo command, runtime
exercise, Git mutation, remote push or CI check was performed for this
reconciliation; the checks do not prove discovery completeness or semantic
correctness.
