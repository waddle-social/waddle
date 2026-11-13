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
| Dual registration | `S/server/dual_registration.rs` (`mirror_register`, `mirror_register_outcome`, `mirror_unregister`). Callers: the registry binding publisher in `W/registration.rs`, which every websocket bind, rollback and teardown goes through (`withdraw_binding`, `retire_binding`, `retire_remote_binding`); `S/server/session_janitors/implementation.rs` (2), `S/clustering/route_bridge/registration/socket.rs`, `S/clustering/route_bridge/delivery/remote.rs`, `S/server/routes/interpret/deps.rs` | The account coordinator owning `connections` and `bind_slots`; one registration per binding (M3) | **present**. The coordinator is the only account authority; the mirror is still written for the readers listed in 2138 | `grep -rn 'mirror_register\|mirror_unregister' S` is empty and the module is deleted |
| User-resource mirrors (account-owner copies of a socket's state on another node) | `X/registry/user_actor/implementation.rs` and `X/registry/user_registry.rs` (`users[].resources`, `pending_unregisters`); `S/clustering/route_bridge/registration/owner.rs`, `owner_update.rs`, `owner_retire.rs`, `owner_sweep.rs`; the reconciliation janitor (`Janitor::RemoteOwnerMirror` in `S/server/session_janitors/implementation.rs`) | Exact connection bindings with account-owned selection metadata, generation-fenced (M3, package E) | **present** | `UserRegistryActor` holds no per-resource clones; `route_bridge/registration/owner*.rs` and the `remote_owner_mirror` janitor are deleted; `xmpp.remote_owner_mirror.*` metrics are gone |
| Remote channel mirrors (a remote-hosted registry entry whose channel forwards to the socket's node) | `S/clustering/route_bridge/registration/socket_forwarder.rs`, `socket.rs`; relay messages `remote_resource_register.v4`, `remote_resource_unregister.v2`, `remote_resource_update.v1`, `remote_resource_write_accepted.v1`, `remote_resource_force_detach.v3`, `remote_user_side_effect.v4` in `S/clustering/relay/implementation.rs`; compatibility handlers in `S/clustering/relay/remote_resource_compat.rs` | Delivery through the durable mailbox to the owning connection actor (M2, M4) | **present** | None of the `remote_resource_*` and `remote_user_side_effect` message IDs is registered; `socket_forwarder.rs` is deleted |
| Routing bypasses (enqueue paths that reach a socket without the owning endpoint: direct, peer, pending, ordered and remote enqueues) | Listed call site by call site in [`2046-caller-inventory.md`](../planning/2046-caller-inventory.md) sections 8 and 10 and [`2046-current-caller-candidates.md`](../planning/2046-current-caller-candidates.md); the endpoint itself is `X/registry/connection_registry/outbound.rs` | One domain-facing addressing model: every delivery addressed to a binding and admitted by its endpoint (package B, M3) | **present**. The caller inventory is the baseline for the bypass freeze | Every row of the caller inventory's bypass table is deleted or reclassified as a documented, non-authoritative index |
| Ordered relay lanes | `S/clustering/ordered_relay.rs` and `S/clustering/ordered_relay/`; `S/clustering/route_bridge/delivery/ordered.rs`, `ordered_send.rs`; relay message `deliver_ordered.v16` in `S/clustering/relay/implementation.rs`; `ordered_relay_handoff_tx` on `WsConnState` | Each lane served from the mailbox, with its ordered-relay path deleted and its wire version bumped (M4: #2173, #2174, #2175); the responsibility of each lane is in [`2046-relay-lane-responsibilities.md`](../planning/2046-relay-lane-responsibilities.md) | **present**. Remaining users, by lane: (1) groupchat transport — retired for every ingress transaction at protocol epoch 2 or later under the `mailbox` selector, where `without_relayed_dispatch` in `S/ingress/commit_mailbox.rs` removes the sender; still sent below epoch 2 (a clustered deployment without the restricted owner phase), under `WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE=relay`, for a message committed without a source binding, and by `RoomMessageRoute::execute` (`S/server/routes/interpret/room_routing.rs`) for a dispatch outside the ingress plan; (2) MUC join, occupant presence update and leave (`JoinPresence`, `OccupantPresence`) and (3) Muji signalling (`MujiJingleIq`) — retired for client-sent stanzas at protocol epoch 2 or later under the `mailbox` selector, where `route_room_request` in `W/handlers/presence/room_request.rs` commits a room request instead; still sent below epoch 2 and under `WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE=relay`, and at every epoch for the leave replayed when a connection ends (`W/cleanup.rs`, `S/clustering/route_bridge/delivery/muc/cleanup.rs`) and for the join repair (`S/clustering/route_bridge/delivery/muc/proxy.rs`); a receiver at the mailbox epoch refuses those kinds on the relay with the unsupported-envelope outcome; (4) `BareJid` and `FullJid` recipients (`ProcessedDirectMessage`, `Message`, `Iq`, `Presence`) — retired for a send committed at protocol epoch 2 or later under the `mailbox` selector, where `deliver_seeded_direct` in `S/clustering/route_bridge/delivery/direct.rs` commits a direct request instead; still sent below epoch 2, under the `relay` selector, for an origin resource hosted through a remote channel mirror, for an account without a `UserActor` claim and when the request could not be committed. Final removal needs every clustered deployment at epoch 2 or later with the `relay` selector removed, and the remote channel mirrors retired. The reserved kinds `PrivateMessage`, `BareRoomIq`, `OccupantIq` and `FanoutChunk` have no producer and no receiver but are part of the `v16` wire enum. No file of the relay is unused, so none is deleted | `deliver_ordered` is unregistered or carries only lanes with a documented owner; `S/clustering/ordered_relay*` is deleted |
| Legacy stream management (the in-memory session registry as the authority on enable, resume, detach and expiry, with detach-time persistence) | `W/stream_management/implementation.rs` (enable and resume when no durable store is published, `attempt_cross_node_resume_raced`); `X/stream_management/session_registry/cross_node_resume.rs` (hydration), `persistence_codec.rs`; `S/sm_persistence_fenced/`; the legacy arm of the SM expiry janitor in `S/server/session_janitors/implementation.rs`. The unselected `S/sm_custody/` candidate is retired | The durable session authority (`S/sm_authority`, [`2139-durable-sm-session-model.md`](../planning/2139-durable-sm-session-model.md)); sessions without a durable record are drained once to offline storage by the owner phase or by the store's opening (`S/sm_authority/legacy_drain.rs`) | **present, unselected**: startup always publishes the durable authority and refuses `WADDLE_XMPP_DURABLE_SM=legacy`. The legacy paths run only for an application state that never ran startup's selection (`DurableSmSelection::Legacy`, the unit-test fixtures) | `DurableSmSelection::Legacy` is deleted; `grep -rn 'restore_from_persistence\|attempt_cross_node_resume_raced\|PostgresFencedSmPersistence' S X` is empty |
| PostgreSQL owner-as-runtime startup | `S/main.rs` (the branch without `WADDLE_DB_RUNTIME_ATTESTATION`, for the PostgreSQL driver), `bootstrap_shared_owner_postgres` in `S/db/bootstrap.rs`; the owner constructors each store keeps for it | Restricted runtime startup: `owner-phase`, `install-attestor`, `emit-handoff`, then `RuntimeStartupIdentity::attach_serving` | **present**. The restricted path is on main and production has not been cut over; deletion follows [the cutover](restricted-runtime-cutover.md). SQLite keeps its single-role startup | `bootstrap_shared_owner_postgres` is deleted and `S/main.rs` refuses PostgreSQL without a handoff |
| Dark connection actor (the actor that hosts the live loop without being the owner) | `W/connection_actor.rs`, `W/connection_actor_core.rs`, `W/connection_actor_driver_task.rs` and `W/connection_actor_driver_task/`, `W/connection_actor/`, `W/actor_local_endpoint.rs`; selected by `LiveConnectionHost::Actor` in `S/server/state.rs`, whose default is `PlainTask` | The connection actor as the only `/ws` owner (M3) | **present, unselected** in production: `LiveConnectionHost` defaults to `PlainTask`, and the test-support environment variable `WADDLE_TEST_LIVE_CONNECTION_HOST` selects the actor | `LiveConnectionHost` and the plain-task loop in `W/connection.rs` are deleted; the actor is the only host |
| `WsConnState` writers | `W/state.rs` (the struct); writers in `W/connection.rs` (44 occurrences), `W/cleanup.rs` (19), `W/batch_write.rs` (14), `W/connection_actor_driver_task.rs` (14), `W/connection_actor_core.rs` (13), `W/outbound.rs` (9), `W/frame.rs` (9), `W/stream_management/registration.rs` (7), `W/connection_actor.rs` (7), `W/registration.rs` (5), `W/replay.rs` (4), and twelve more files with fewer | One owner per field: the connection actor, durable stream management, the account coordinator or the transport pump, as assigned in [D5 of the completion contracts](../planning/2046-completion-contracts.md#d5-field-owners) | **present** | No function outside the connection actor takes `&mut WsConnState`; the fields D5 marks "Deleted" are gone |

## Account-resource deletion plan

The dual registration, user-resource mirror and registry-ownership rows
above are deleted together, after the account coordinator of
[`2138-account-coordinator.md`](../planning/2138-account-coordinator.md) is
the authority. None of them can be deleted today: the WebSocket loop still
writes each one, and routing reads them. Each step lists what must hold on
main first.

| Step | What is deleted | Preconditions |
|---|---|---|
| 1 | `mirror_register`, `mirror_register_outcome` and `mirror_unregister` in `S/server/dual_registration.rs`, with the file | The connection actor sends `Bind`, `PublishEndpoint` and `RegisterActor` for every bind and `SuccessfulResume` for every resume; `BeginRetirement` and `CompleteRetirement` replace every `mirror_unregister` caller (`W/registration.rs`, `W/cleanup.rs`, `W/stream_management/registration.rs`, the session janitors, the route bridge); the durable resource rows commit in the bind transaction |
| 2 | `users[].resources` and `pending_unregisters` in `X/registry/user_registry.rs`, and the `UserActor` connection map with its `RegisterConnection` and `UnregisterConnection` messages | Step 1; `registry/selection.rs` and `UserActor` delivery read the coordinator's slots; recipient selection, carbon policy and presence fan-out pass with the coordinator as their only source; remote resources are addressed through `clustering::addressing`, not through mirror entries |
| 3 | `connections` and `bind_slots` as the owner of a binding in `X/registry/connection_registry.rs` | Steps 1 and 2; a full JID resolves to a `BindingRef` through the coordinator before every delivery; `LocalConnectionBindingId` has no remaining reader, which needs the remote registration snapshot to carry a `BindingRef` |
| 4 | `presence_states`, `last_activity` and `pending_subscription_stanzas` as registry fields | Step 3; the coordinator holds them process-locally, as D5 assigns |

A transport-only socket index may remain after step 3 if the connection
actor needs one to find its pump. It is recorded under "Derived indexes
that may remain" when that is decided, and nothing may read it to choose a
destination.

### State of the plan with the coordinator as the only authority

The account coordinator is the only account-resource authority: startup
always selects it, `WADDLE_XMPP_ACCOUNT_AUTHORITY` is removed and ignored
with a startup warning, and every `/ws` connection reports its binding to
it. Deleted with the selector:

- `AccountAuthorityMode` and its `Registry` variant; an application that
  never ran startup's selection holds `AccountAuthoritySelection::Unselected`,
  which serves no account;
- the registry arm of every account read (`S/account_coordinator/reads.rs`
  callers: recipient inventory, bare-JID selection, carbons, presence probe
  and fan-out, roster and block-list push sets, inbox pushes, the group-DM
  leave, the route bridge's presence check and bare-JID presence fan-out).
  A read the coordinator cannot answer is a failed read; no caller reads
  the registry or the `UserActor` in its place.

No step of the table above is deletable yet. Each was checked against the
source after the selector was removed:

- **Step 1 is blocked by the remaining readers.** The readers that still
  read the `UserActor` or a remote channel mirror are listed in
  [`2138-account-coordinator.md`](../planning/2138-account-coordinator.md),
  "What still writes the registry and the mirrors". `mirror_register` and
  `mirror_unregister` are still called from `W/registration.rs`,
  `W/cleanup.rs`, `S/server/session_janitors/implementation.rs`,
  `S/clustering/route_bridge/registration/socket.rs`,
  `S/clustering/route_bridge/delivery/remote.rs` and the interpreter
  (`S/server/routes/interpret/deps.rs`, `routing.rs`).
- **`X/registry/selection.rs`** is still called by stream-management
  promotion (`S/sm_promotion/live.rs`, `S/sm_promotion/pending.rs`) and by
  `X/routing.rs`; it goes when those read the coordinator.
- **`RegistryBindingPublisher`'s confirmation as the gate on a bind**
  stays while the mirror is read: a binding whose registry write failed
  would be recorded at the coordinator and unreachable by those readers.
- **Steps 2 to 4**, the remote channel mirrors (`socket_forwarder.rs`, the
  `remote_resource_*` relay messages) and the `owner*.rs` mirrors with the
  `RemoteOwnerMirror` janitor follow step 1 unchanged.
- **The `BareJid` and `FullJid` ordered lanes** of `deliver_ordered` are
  still sent below protocol epoch 2, under the `relay` acceptance selector,
  for an origin resource hosted through a remote channel mirror and when a
  direct request could not be committed (row "Ordered relay lanes"). Their
  removal, with the wire version bump to `v17`, needs those senders gone.

`WsConnState` fields that D5 marks "Deleted", and why each is still
present:

| Field | Remaining users |
|---|---|
| `registry_owner` | The owner token of the connection's registry entry: every owner-gated registry write (`W/frame.rs`, `W/batch_write.rs`, `W/connection.rs`, the presence, roster, blocking and MUC handlers, `W/stream_management/implementation.rs`) and every teardown path in `W/cleanup.rs` and `W/cleanup/durable_sm.rs`. It goes with the registry entry (step 3) |
| `recovered_pending_endpoint`, `resume_endpoint_handoff_blocked` | The failed-resume endpoint handback in `W/stream_management/registration.rs` and its guards in `W/registration.rs` and `W/stream_management/durable.rs`. They go with the channel endpoint, when the session's outbound queue is the actor-owned endpoint (S5) |
| `pending_tx` (the connection loop's unpublished endpoint, a local of `W/connection.rs` passed to registration) | Registration takes the endpoint it publishes from it. Same condition as above |
| `ingress_generation` | `W/frame.rs` stamps it on each ingress append. It goes when `ConnectionGeneration` is replaced by the `ConnectionRef` (D3) |

The fields D5 assigns to the connection actor are held in `WsConnState`,
which the actor owns through `LiveSocketState`; they have not been split
into a separate actor-owned struct. The coordinator-owned rows
(`carbons_enabled`, `roster_interested`, `blocklist_interested`,
`presence_*`) are the connection's projection: they are sent as `UpdateSnapshot` and read by routing from the
coordinator, and are still copied onto the registry entry for the readers
of step 1. `account_binding` is the actor's handle on its coordinator
binding.

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
