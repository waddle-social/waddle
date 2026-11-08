# Bounded indirect-edge candidates and source review

Source target: `5763923e0cb15d3bbab9cf53800e5b1bdd41e183`. The inspected
checkout is `718f47a9e08f29cf2e6157803770e2f495fe201e`; a read-only
`git diff 5763923e -- server` produced no changed server paths. The input was
[A01–A31](actor-mirror-edge-ledger.md) and the [WebSocket borrowed-write
narrative](ws-state-semantic-inventory.md#borrowed-writes-and-callers). This is
not an independent all-source discovery or a complete caller inventory.

The two TSVs contain **148 individual links**: 116 from 29 actor/relay ledger
groups and 32 from the selected WebSocket borrow and control-flow paths. The
[candidate table](indirect-edge-candidates.tsv) is the seven identity columns
projected from the [review table](indirect-edge-reviews.tsv); the matching rows
are not an independent second discovery. Each row names a concrete call site,
message ask/tell and handler, or, in A16, the shared-entry state connection.
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
- `test-actor-ask` records observed `cfg(test)` unit-test senders to otherwise
  release-compiled handlers. The physical continuation integration sender is
  itself [`test-support` gated](../../../server/crates/waddle-server/tests/physical_continuation.rs#L1).
  A31's publication asks are also `test-support` gated and the child handlers
  require `test` or `test-utils`; `test-support` enables the latter in the
  [server features](../../../server/crates/waddle-server/Cargo.toml#L176).
  A31 retirement is separately labeled dormant: the endpoint's own
  [module documentation](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1)
  says `/ws` does not select it. These facts do not establish live bind use.
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
and alternative tests and support harnesses. A test of a release-compiled
handler does not prove production reachability; a retry or enqueue result does
not prove drain or a physical socket write. These notes leave all such work
open rather than treating the candidate list as exhaustive.

Manual source inspection checked the cited message implementations, selected
senders, wrappers, borrowed contexts and relevant feature gates. The candidate
and review tables were checked for equal row count, unique IDs, canonical
operation/class labels, source-commit value, and existing endpoint paths using
ordinary text tools. No inventory validation script, Cargo command, tests,
runtime exercise, Git mutation, remote push, or CI check was performed as part
of this shard. The table checks do not independently establish discovery
completeness, semantic correctness, or every line and evidence citation; the
integrator must run the full validator and broader independent review.
