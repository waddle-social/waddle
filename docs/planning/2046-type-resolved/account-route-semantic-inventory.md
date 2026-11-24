# Account routing and mirror edges: semantic review

Source target: `5763923e0cb15d3bbab9cf53800e5b1bdd41e183`. This shard
records reviewed account-registration and remote-resource edges. It is not an
exhaustive caller inventory: the compiler-site ledger, room dispatch,
additional relay handlers, and test callers still require reconciliation.
`Authority` describes the current local decision at an edge; it does not prove
the complete distributed ownership or custody contract required by #2046.

Paths below are relative to the repository root. Production means compiled in
the indicated configuration; a source reference alone does not prove a live
route selected it.

| Source / edge | Operation | Classification and current role |
| --- | --- | --- |
| `server/crates/waddle-server/src/server/dual_registration.rs:120` → `server/crates/waddle-xmpp/src/registry/user_registry.rs:1090` | Call / transfer `ConnectionEntry` | Authority at registration: `mirror_register_outcome` waits for the registry actor's result and maps failures to outcomes for the binding caller. The shared entry carries the socket endpoint and atomic state. |
| `server/crates/waddle-server/src/server/routes/websocket/registration.rs:762`, `:770`, `:920` → `mirror_register_outcome` | Call | Authority in the live registration path: a successful actor registration participates in the bind decision. The exact failure, rollback, and accepted-frame custody obligations remain separate open gates. |
| `server/crates/waddle-xmpp/src/registry/user_registry.rs:1120` → `server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs:194` | Call / write | Authority for the actor's local resource map: the registry asks the child to install the entry, then writes `UserEntry.resources` after the child replies. The latter is a derived index for exact demotion, not a durable account claim. |
| `server/crates/waddle-xmpp/src/registry/user_registry.rs:1186` → `server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs:243` | Call / conditional write | Authority for an owner-gated actor slot: the child checks the requested local binding identity and the registry updates its mirror only on success. A local binding identity is not itself a durable cross-node claim. |
| `server/crates/waddle-server/src/server/dual_registration.rs:187` → `server/crates/waddle-xmpp/src/registry/user_registry.rs:1248` | Call / enqueue | Migration glue and derived-index maintenance: the unregister mirror reports mailbox admission, not confirmed child removal. Its owner token constrains later removal; retry custody belongs to the registry. |
| `server/crates/waddle-server/src/server/routes/websocket/registration.rs:250`, `:808`; `server/crates/waddle-server/src/server/routes/websocket/cleanup.rs:1216`, `:1476`, `:1557`, `:2767`, `:3866`; `server/crates/waddle-server/src/server/routes/websocket/stream_management/registration.rs:436`, `:526` | Call | Teardown and registration migration glue invoking the mirror. The source sites do not alone prove an exact durable teardown, completed socket drain, or successful mirror handling. |
| `server/crates/waddle-xmpp/src/registry/user_registry.rs:629`, `:659`, `:682` → `server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs:499` | Call / conditional removal | Authority for this local removal decision and claim-release sequencing: child reports removed, absent, or retained; the registry removes its derived resource entry and may release the captured claim only when empty. Failure may retain a retry item. |
| `server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs:23`, `:105`, `:145` | Read / write | `connections` is the actor's current resource routing map; `presence_states` and `pending_subscriptions` have distinct per-user purposes. Removal seals an actor binding if present and clears resource presence. The map and parent `UserEntry.resources` coexist with the DashMap registry, so this is not evidence of a completed single-owner cutover. |
| `server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs:539`, `:562`, `:582`, `:603`, `:626`; `server/crates/waddle-xmpp/src/registry/selection.rs:167` | Read / write | Routing queries derive snapshots from actor entries; `UpdatePresence` writes shared availability and priority atomics. `GetAvailableResources` has a current selection-helper caller even though the handler comment describes an earlier caller's retirement. |
| `server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs:196`, `:233`, `:307`, `:333`, `:355` | Read / transfer | Routing selection reads actor entries; direct, peer and pending-flush handlers construct and pass outbound envelopes; `GetConnectionEntry` returns an entry clone. These are transport/delivery edges, not proof of terminal socket write or durable custody. |
| `server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs:264`, `:287` | Call / transfer | Dark candidate: the source describes these custody entry points as having no production callers. Their retained-envelope behavior does not establish live `/ws` selection. |
| `server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs:174`, `:382`, `:556`, `:685` → `RegisterUserResourceIfOwnerOrAbsent` / `UnregisterAndReleaseIfEmptyWithoutPendingRecord` | Call / conditional mirror write or removal | Clustered remote-resource migration glue: owner-side registration mirrors a remote socket into the account actor with an owner check, and removal is tied to the recorded registration. The remote proxy is not the physical socket owner. |
| `server/crates/waddle-server/src/clustering/route_bridge/registration/owner_sweep.rs:258` → `UnregisterAndReleaseIfEmpty` | Call / conditional removal | Clustered convergence of an owner-side resource mirror; the resulting local decision does not by itself prove the remote socket stopped. |
| `server/crates/waddle-server/src/clustering/route_bridge/types.rs:227`, `:240`; `registration/socket_forwarder.rs:86`, `:129`; `delivery/remote_socket.rs:431`, `:601` | Construct / transfer / deliver | Cluster transport envelopes and forwarding endpoints. The accepted-write variant and ordinary forwarding variant are distinct; transport response and effect custody must be interpreted according to each path. |

## Tests and remaining review

`server/crates/waddle-xmpp/src/registry/user_actor/tests.rs` and
`user_registry/tests.rs` exercise actor messages directly. Cluster bridge
tests and `server/routes/websocket/tests/registration.rs` exercise selected
edges; these test-only callers are not proof that every production caller is
listed above. Complete the per-site ledger and separately reconcile actor
message senders, remote handler registrations, room dispatch, and all cfg/test
profiles before treating the #2046 inventory item as complete.
