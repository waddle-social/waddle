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
| Ordered relay lanes | `S/clustering/ordered_relay.rs` and `S/clustering/ordered_relay/`; `S/clustering/route_bridge/delivery/ordered.rs`, `ordered_send.rs`; relay message `deliver_ordered.v16` in `S/clustering/relay/implementation.rs`; `ordered_relay_handoff_tx` on `WsConnState` | Each lane served from the mailbox, with its ordered-relay path deleted and its wire version bumped (M4: #2173, #2174, #2175); the responsibility of each lane is in [`2046-relay-lane-responsibilities.md`](../planning/2046-relay-lane-responsibilities.md) | **present**. Remaining users, by lane: (1) groupchat transport — retired for every ingress transaction at protocol epoch 2 or later, where `without_relayed_dispatch` in `S/ingress/commit_mailbox.rs` removes the sender; still sent below epoch 2 (a clustered deployment without the restricted owner phase), for a message committed without a source binding, and by `RoomMessageRoute::execute` (`S/server/routes/interpret/room_routing.rs`) for a dispatch outside the ingress plan; (2) MUC join, occupant presence update and leave (`JoinPresence`, `OccupantPresence`) and (3) Muji signalling (`MujiJingleIq`) — retired for client-sent stanzas at protocol epoch 2 or later, where `route_room_request` in `W/handlers/presence/room_request.rs` commits a room request instead; still sent below epoch 2, and at every epoch for the leave replayed when a connection ends (`W/cleanup.rs`, `S/clustering/route_bridge/delivery/muc/cleanup.rs`) and for the join repair (`S/clustering/route_bridge/delivery/muc/proxy.rs`); a receiver at the mailbox epoch refuses those kinds on the relay with the unsupported-envelope outcome; (4) `BareJid` and `FullJid` recipients (`ProcessedDirectMessage`, `Message`, `Iq`, `Presence`) — retired for a send committed at protocol epoch 2 or later, where `deliver_seeded_direct` in `S/clustering/route_bridge/delivery/direct.rs` commits a direct request instead; still sent below epoch 2, for an origin resource hosted through a remote channel mirror, for an account without a `UserActor` claim and when the request could not be committed. Below epoch 2 the relay carries such a stanza only to the account owner: a full JID whose coordinator binding is hosted on a node other than the one delivering the copy needs a bound request, which is not carried below epoch 2, so it is `Unavailable` there (offline storage, bounce or IQ error) rather than relayed. The `WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE` selector is removed (ignored with a startup warning), so nothing keeps the relay at epoch 2. Final removal needs every clustered deployment at epoch 2 or later, which means the PostgreSQL owner-as-runtime startup deleted (it never activates the epoch), and the remote channel mirrors retired. The reserved kinds `PrivateMessage`, `BareRoomIq`, `OccupantIq` and `FanoutChunk` have no producer and no receiver but are part of the `v16` wire enum. No file of the relay is unused, so none is deleted | `deliver_ordered` is unregistered or carries only lanes with a documented owner; `S/clustering/ordered_relay*` is deleted |
| Legacy stream management (the in-memory session registry as the authority on enable, resume, detach and expiry, with detach-time persistence) | `X/stream_management/session_registry/persistence_codec.rs` and the registry's legacy `DetachedSession` maps, appends and promotion inventory; the session writers of `S/sm_persistence_fenced/`; legacy promotion in `S/sm_promotion/` and the graceful-shutdown drain of registry sessions. The unselected `S/sm_custody/` candidate is retired | The durable session authority (`S/sm_authority`, [`2139-durable-sm-session-model.md`](../planning/2139-durable-sm-session-model.md)); sessions without a durable record are drained once to offline storage by the owner phase or by the store's opening (`S/sm_authority/legacy_drain.rs`) | **partly deleted**: startup always publishes the durable authority and refuses `WADDLE_XMPP_DURABLE_SM=legacy`, and every test fixture selects it too. `DurableSmSelection::Legacy` and the registry's resumable enable, resume and cross-node resume attempt (`handle_sm_resume_terminal`, `attempt_cross_node_resume_raced`) are deleted; a state without a selection refuses resumable enables and resumes. Also deleted: the registry's restore at startup (`restore_from_persistence*`), its cross-node steal and hydration (`prepare_cross_node_resume`, `finish_cross_node_steal`, `hydrate_reclaimed*`; a reclaimed claim is settled through the durable authority instead) and the legacy arm of the SM expiry janitor. Still present: the persistence codec, the fenced session writers, legacy promotion and the shutdown drain of registry sessions. Under the durable authority they write no row of their own (checked by `tests/xep0198_durable_ws.rs`), so their deletion loses nothing | `grep -rn 'persistence_codec\|PostgresFencedSmPersistence\|drain_all_for_shutdown' S X` is empty |
| PostgreSQL owner-as-runtime startup | `S/main.rs` (the branch without `WADDLE_DB_RUNTIME_ATTESTATION`, for the PostgreSQL driver), `bootstrap_shared_owner_postgres` in `S/db/bootstrap.rs`; the owner constructors each store keeps for it | Restricted runtime startup: `owner-phase`, `install-attestor`, `emit-handoff`, then `RuntimeStartupIdentity::attach_serving` | **present**. The restricted path is on main and production has not been cut over; deletion follows [the cutover](restricted-runtime-cutover.md). SQLite keeps its single-role startup | `bootstrap_shared_owner_postgres` is deleted and `S/main.rs` refuses PostgreSQL without a handoff |
| Dark connection actor (the actor that hosts the live loop without being the owner) | Gone: `W/connection_actor*.rs`, `W/connection_actor/`, `W/actor_local_endpoint.rs` and `LiveConnectionHost` no longer exist. Every `/ws` connection runs on the actor in `W/connection/live_actor.rs` | The connection actor as the only `/ws` owner (M3) | **retired** | `grep -rn 'LiveConnectionHost\|connection_actor_core\|actor_local_endpoint' S` is empty |
| Actor-owned outbound endpoint (an endpoint whose queue carries IDs and keeps envelopes in process-local custody, with provisional registry and `UserActor` publication) | Gone: `ActorOutboundCustody`, `ActorOutboundId`, `ActorOutboundLease`, `ActorOutboundReceiver`, `ActorSocketOutboundEndpoint` and the `Actor` queue in `X/registry/connection_registry/endpoint.rs`; `ActorProvisionalConnectionEntry`, `publish_provisional_if_expected`, `activate_provisional_if_confirmed`, `retire_provisional_if_exact`; the `UserActor` messages `RegisterProvisionalConnectionIfExpected`, `ConfirmProvisionalConnection`, `HoldUserActorForPublication`; the unwired `dark_envelope_custody` module; in `S/websocket_transport.rs` the actor frame constructors, `RetainedWebSocketInput`, `retain_actor_cleanup` and the uninstallable `ActorWriteProbe`; `WriteAuthority::completed` and `after_attempt` | The live actor writes through the ordinary binding endpoint (`SocketOutboundEndpoint`) | **retired**. `ActorOutboundStage` stays: it is how a write receipt reports its stage (`S/websocket_transport.rs`, `W/send.rs`) | `grep -rn 'ActorOutboundCustody\|ActorSocketOutboundEndpoint\|ActorProvisionalConnectionEntry\|ActorWriteProbe' S X` is empty |
| `WsConnState` writers | `W/state.rs` (the struct); writers in `W/connection.rs` (44 occurrences), `W/cleanup.rs` (19), `W/batch_write.rs` (14), `W/connection_actor_driver_task.rs` (14), `W/connection_actor_core.rs` (13), `W/outbound.rs` (9), `W/frame.rs` (9), `W/stream_management/registration.rs` (7), `W/connection_actor.rs` (7), `W/registration.rs` (5), `W/replay.rs` (4), and twelve more files with fewer | One owner per field: the connection actor, durable stream management, the account coordinator or the transport pump, as assigned in [D5 of the completion contracts](../planning/2046-completion-contracts.md#d5-field-owners) | **present** | No function outside the connection actor takes `&mut WsConnState`; the fields D5 marks "Deleted" are gone |
| Migration history (V1–V15, V1001–V1037 and the namespace split) | Deleted: `S/db/migrations/global.rs`, `waddle.rs`, `namespace.rs` and the SQL files they included. What recognises a database the history built stays: `server/crates/waddle-server/sql/baseline/*.retired-ledger.json`, `*.fingerprint.json` | The schema baseline, migration 2000 (`server/crates/waddle-server/sql/baseline/`, `S/db/migrations/baseline.rs`), and migrations from 2001 in `S/db/migrations/after_baseline.rs`. A database at V1037 is stamped where migrations run ([design](../planning/2046-schema-baseline.md)) | **retired** | `rg -n 'migrations::(global\|waddle)\|MigrationNamespace' server/crates` finds nothing |

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
  still sent below protocol epoch 2, for an origin resource hosted through a remote channel mirror and when a
  direct request could not be committed (row "Ordered relay lanes"). They
  reach the account owner only; a full JID bound on another node is not
  relayed to its binding below epoch 2 and is `Unavailable` there. Their
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

## Kept after the retirement sweep, and why

The sweep that removed the remote room acceptance selector and the
actor-owned outbound endpoint checked every remaining legacy and fallback
path against one premise: production moves by one coordinated `Recreate`
cutover to ingress protocol epoch 2 with the mailbox, durable stream
management and the account coordinator. These stay, each for the reason
given:

- **Relay paths below protocol epoch 2** (every "still sent below epoch 2"
  sender in row "Ordered relay lanes", the relay-era branches of
  `S/ingress/commit_mailbox.rs`, `S/ingress/direct_request.rs`,
  `S/ingress/room_request.rs` and `S/ingress/commit_room.rs`). Only the
  restricted-runtime owner phase activates the epoch
  (`activate_ingress_protocol_epoch` in `S/server/runtime_owner.rs`); the
  PostgreSQL owner-as-runtime startup never does, so a clustered deployment
  started that way runs at epoch 0 on these paths. That startup is what
  production runs until the cutover, and main deploys automatically, so
  these paths go in the same change that deletes the owner-as-runtime
  startup.
- **The leave replayed at connection end and the join repair** on the
  ordered relay (`W/cleanup.rs`, `S/clustering/route_bridge/delivery/muc/`)
  are sent at every epoch; no mailbox replacement exists for them yet.
- **The reserved MUC kinds** `PrivateMessage`, `BareRoomIq`, `OccupantIq`
  and `FanoutChunk` of `deliver_ordered.v16`. They have no producer and no
  receiver, but removing them changes the `v16` enum, which is an
  incompatible relay activation on an automatically deployed main. They go
  with the `v17` bump that removes the `BareJid` and `FullJid` recipients.
- **Legacy stream management** (row above). The resumable enable and
  resume of the in-memory registry, its startup restore, its cross-node
  steal and hydration, and the legacy arm of the expiry janitor are
  deleted. Its legacy `DetachedSession` path (the persistence codec, the
  fenced session writers of `S/sm_persistence_fenced/`, legacy promotion and
  the shutdown drain of registry sessions) remains: `SmPersistenceStorage`
  also carries the ingress custody (`sm_ingress_appends`) and
  acknowledgement journal the durable path uses, so the session half has to
  be split from it, and the ingress recovery tests that seed registry-held
  detached sessions moved onto durable sessions, before it can be deleted.
  `sm_sessions` and `sm_unacked` are not droppable by a later migration:
  the durable authority keeps its resource snapshots and retained frames
  there. Only the legacy rows are gone, drained by the owner phase.
- **Test-support transport admission** (`SocketAdmission::clustered`,
  `standalone` and `start_socket` in `S/websocket_transport.rs`, marked
  "the inactive connection owner is not integrated yet") are compiled only
  with `test-support` and are the fixtures of `tests/websocket_transport.rs`
  and `tests/socket_admission.rs`.
- **Unselected database candidates** (`descendant_exists_with_candidate_api`
  and `descendant_settled_with_candidate_api` in
  `S/ingress_uow/effect_descendants.rs`, `prune_completed_before_with_candidate`
  in `S/notification_outbox/prune.rs`, the pinned MAM handles in
  `S/db/physical_attestation/pinned_mam.rs`). They belong to the restricted
  runtime's definer API and are exercised by its focused tests; they are
  decided with the owner-as-runtime deletion.
- **`WsConnState` fields.** Every field is read by the live loop or its
  handlers; the four D5 marks "Deleted" are listed in the table above with
  the change each waits for.

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
