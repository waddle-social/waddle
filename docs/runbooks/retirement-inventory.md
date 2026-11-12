# Retirement inventory

Every legacy path that
[#2046](https://github.com/waddle-social/waddle/issues/2046) retires, where
it lives, what replaces it, and whether it is still present. The final
deletion is checked against this table: a path is retired when the check in
its row finds nothing outside tests.

Status column: **present** means the path is live on main; **present,
unselected** means the code exists and production does not use it;
**retired** means it is gone.

Read from the source at this commit by search; counts are occurrences
outside test files and test modules. `S/` is
`server/crates/waddle-server/src/`, `W/` is `S/server/routes/websocket/`,
`X/` is `server/crates/waddle-xmpp/src/`.

| Legacy path | Where it lives today | Replaced by | Status | Deletion check |
|---|---|---|---|---|
| Dual registration | `S/server/dual_registration.rs` (`mirror_register`, `mirror_register_outcome`, `mirror_unregister`). Callers: `W/registration.rs` (5), `W/cleanup.rs` (5), `W/stream_management/registration.rs` (2), `S/server/session_janitors/implementation.rs` (2), `S/clustering/route_bridge/registration/socket.rs`, `S/clustering/route_bridge/delivery/remote.rs`, `S/server/routes/interpret/deps.rs` | The account coordinator owning `connections` and `bind_slots`; one registration per binding (M3) | **present**. Actor selection is authoritative through it today | `grep -rn 'mirror_register\|mirror_unregister' S` is empty and the module is deleted |
| User-resource mirrors (account-owner copies of a socket's state on another node) | `X/registry/user_actor/implementation.rs` and `X/registry/user_registry.rs` (`users[].resources`, `pending_unregisters`); `S/clustering/route_bridge/registration/owner.rs`, `owner_update.rs`, `owner_retire.rs`, `owner_sweep.rs`; the reconciliation janitor (`Janitor::RemoteOwnerMirror` in `S/server/session_janitors/implementation.rs`) | Exact connection bindings with account-owned selection metadata, generation-fenced (M3, package E) | **present** | `UserRegistryActor` holds no per-resource clones; `route_bridge/registration/owner*.rs` and the `remote_owner_mirror` janitor are deleted; `xmpp.remote_owner_mirror.*` metrics are gone |
| Remote channel mirrors (a remote-hosted registry entry whose channel forwards to the socket's node) | `S/clustering/route_bridge/registration/socket_forwarder.rs`, `socket.rs`; relay messages `remote_resource_register.v4`, `remote_resource_unregister.v2`, `remote_resource_update.v1`, `remote_resource_write_accepted.v1`, `remote_resource_force_detach.v3`, `remote_user_side_effect.v4` in `S/clustering/relay/implementation.rs`; compatibility handlers in `S/clustering/relay/remote_resource_compat.rs` | Delivery through the durable mailbox to the owning connection actor (M2, M4) | **present** | None of the `remote_resource_*` and `remote_user_side_effect` message IDs is registered; `socket_forwarder.rs` is deleted |
| Routing bypasses (enqueue paths that reach a socket without the owning endpoint: direct, peer, pending, ordered and remote enqueues) | Listed call site by call site in [`2046-caller-inventory.md`](../planning/2046-caller-inventory.md) sections 8 and 10 and [`2046-current-caller-candidates.md`](../planning/2046-current-caller-candidates.md); the endpoint itself is `X/registry/connection_registry/outbound.rs` | One domain-facing addressing model: every delivery addressed to a binding and admitted by its endpoint (package B, M3) | **present**. The caller inventory is the baseline for the bypass freeze | Every row of the caller inventory's bypass table is deleted or reclassified as a documented, non-authoritative index |
| Ordered relay lanes | `S/clustering/ordered_relay.rs` and `S/clustering/ordered_relay/`; `S/clustering/route_bridge/delivery/ordered.rs`, `ordered_send.rs`; relay message `deliver_ordered.v14` in `S/clustering/relay/implementation.rs`; `ordered_relay_handoff_tx` on `WsConnState` | Each lane served from the mailbox, with its ordered-relay path deleted and its wire version bumped (M4: #2173, #2174, #2175) | **present** | `deliver_ordered` is unregistered or carries only lanes with a documented owner; `S/clustering/ordered_relay*` is deleted |
| PostgreSQL owner-as-runtime startup | `S/main.rs` (the branch without `WADDLE_DB_RUNTIME_ATTESTATION`, for the PostgreSQL driver), `bootstrap_shared_owner_postgres` in `S/db/bootstrap.rs`; the owner constructors each store keeps for it | Restricted runtime startup: `owner-phase`, `install-attestor`, `emit-handoff`, then `RuntimeStartupIdentity::attach_serving` | **present**. The restricted path is on main and production has not been cut over; deletion follows [the cutover](restricted-runtime-cutover.md). SQLite keeps its single-role startup | `bootstrap_shared_owner_postgres` is deleted and `S/main.rs` refuses PostgreSQL without a handoff |
| Dark connection actor (the actor that hosts the live loop without being the owner) | `W/connection_actor.rs`, `W/connection_actor_core.rs`, `W/connection_actor_driver_task.rs` and `W/connection_actor_driver_task/`, `W/connection_actor/`, `W/actor_local_endpoint.rs`; selected by `LiveConnectionHost::Actor` in `S/server/state.rs`, whose default is `PlainTask` | The connection actor as the only `/ws` owner (M3) | **present, unselected** in production: `LiveConnectionHost` defaults to `PlainTask`, and the test-support environment variable `WADDLE_TEST_LIVE_CONNECTION_HOST` selects the actor | `LiveConnectionHost` and the plain-task loop in `W/connection.rs` are deleted; the actor is the only host |
| `WsConnState` writers | `W/state.rs` (the struct); writers in `W/connection.rs` (44 occurrences), `W/cleanup.rs` (19), `W/batch_write.rs` (14), `W/connection_actor_driver_task.rs` (14), `W/connection_actor_core.rs` (13), `W/outbound.rs` (9), `W/frame.rs` (9), `W/stream_management/registration.rs` (7), `W/connection_actor.rs` (7), `W/registration.rs` (5), `W/replay.rs` (4), and twelve more files with fewer | One owner per field: the connection actor, durable stream management, the account coordinator or the transport pump, as assigned in [D5 of the completion contracts](../planning/2046-completion-contracts.md#d5-field-owners) | **present** | No function outside the connection actor takes `&mut WsConnState`; the fields D5 marks "Deleted" are gone |

## Derived indexes that may remain

The umbrella allows a retained derived index if it has a documented owner
and a non-authoritative purpose. None is recorded as retained yet. When a
row above is closed by keeping an index, add it here with its owner, what
it is derived from and what must never read it as authority.

## Related deletions tracked elsewhere

- The per-store database URL settings, refused under the restricted
  runtime: [M1 gaps](../planning/2046-completion-contracts.md#m1-gaps-serve-under-the-runtime-role).
  `UnconvertedStores` no longer occurs in the source.
- The field-by-field owner table: D5 in the same document.
- Call sites and line references: [`2046-caller-inventory.md`](../planning/2046-caller-inventory.md),
  which states where its line numbers have drifted.
