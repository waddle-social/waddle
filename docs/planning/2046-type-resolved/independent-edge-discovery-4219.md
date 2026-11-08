# Independent indirect-edge discovery: partial source review

This is a **partial, non-exhaustive** independent discovery of candidate call,
borrow, and shared-entry dataflow edges. It is a handoff for further review,
not a complete caller inventory or a validator-coverage claim. The integrator
identified the inspected checkout as merge `6138c24`, with server source from
main `4219bdd` plus inventory work. This reviewer did not independently
revalidate that provenance. Line anchors refer to the source inspected in
`/private/tmp/waddle-2046-p3-resume-custody`; recheck them against the eventual
source target before integration. The comparison was against the candidate
and review tables as inspected, and is not a claim about later edits to them.

Operations and classifications below are **proposed** using the current
inventory vocabulary. Each call or ask is a source edge conditional on its
branch and successful progression; none alone proves that a branch ran, a
queue drained, a socket accepted or wrote bytes, an actor retired, or custody
completed. Test senders establish neither production reachability nor live
`/ws` selection.

## Production: authority-revocation rollback

- [`registration.rs:710`](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L710)
  and [`:789`](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L789)
  -> [`rollback_registered_connection` at `:238`](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L238):
  `borrow-transfer`, `authority;derived-index`. These branches run when the
  admission permit or shutdown check fails before or after the registration
  mirror. The helper retires the binding, owner-conditionally unregisters the
  connection entry and clears `conn.registry_owner`.
- [`registration.rs:250`](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L250)
  -> [`mirror_unregister` at `dual_registration.rs:187`](../../../server/crates/waddle-server/src/server/dual_registration.rs#L187):
  `call`, `derived-index`. The bounded tell may fail and admission does not
  establish completed removal. The existing A03 rollback row identifies the
  separate [`registration.rs:808`](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L808)
  call, not this helper call.

## Production: room presence and message delivery

- [`muc/implementation.rs:1778`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L1778),
  [`muc_update.rs:425`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc_update.rs#L425),
  [`cleanup.rs:288`](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L288),
  [`cleanup.rs:359`](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L359),
  [`cleanup.rs:417`](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L417),
  and [`bot.rs:769`](../../../server/crates/waddle-server/src/server/routes/interpret/bot.rs#L769)
  -> [`route_room_presence_to_occupant` at `muc/implementation.rs:389`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L389):
  `call`, `transport`. These separately cover conditional join, presence-update,
  self-leave, peer-leave, Muji-update, and bot routes. For example, join
  broadcast is guarded by the tests at
  [`muc/implementation.rs:1758`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L1758),
  and leave broadcast is guarded by
  [`cleanup.rs:334`](../../../server/crates/waddle-server/src/server/routes/websocket/cleanup.rs#L334).
- [`muc/implementation.rs:398`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L398)
  and [`bot.rs:758`](../../../server/crates/waddle-server/src/server/routes/interpret/bot.rs#L758)
  -> [`deliver_room_presence_locally` at `muc/implementation.rs:441`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L441);
  [`muc/implementation.rs:453`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L453)
  -> [`ConnectionRegistry::try_send_to` at `sending.rs:512`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L512):
  `call`, `transport` and `call`, `derived-index;transport`, respectively.
  The registered-remote attempt happens first at `:395`. A full local channel
  is retried once immediately; on a second full result the helper logs and
  counts the drop, then returns `true` at
  [`:468`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L468).
  Consequently that result does not select the later fallback. `Delivered` is
  an endpoint enqueue outcome, not independent physical-write evidence.
- With `clustering`, when the prior remote attempt does not take the frame and
  local delivery returns `false`,
  [`muc/implementation.rs:406`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L406)
  -> [`route_to_connection` at `route_to_connection/implementation.rs:148`](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L148):
  `call`, `transport`. The dependencies are scoped to a room ordered-relay
  origin at `:403-405`; this is a conditional fallback, not unconditional
  cluster fanout.
- [`room_system_message.rs:259`](../../../server/crates/waddle-server/src/server/routes/interpret/room_system_message.rs#L259)
  -> [`route_to_connection` at `route_to_connection/implementation.rs:148`](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L148):
  `call`, `transport`, once per snapshot occupant with a personalized full-JID
  destination. It remains subject to the route's recursion, planning, relay,
  and delivery branches.
- [`interpret/implementation.rs:309`](../../../server/crates/waddle-server/src/server/routes/interpret/implementation.rs#L309)
  -> [`dispatch_to_room` at `room_dispatch.rs:171`](../../../server/crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L171),
  and [`room_dispatch.rs:826`](../../../server/crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L826)
  -> [`interpret_with_depth` at `interpret/implementation.rs:239`](../../../server/crates/waddle-server/src/server/routes/interpret/implementation.rs#L239):
  `call`, `authority;transport`. The first is the `DispatchToRoom` event arm;
  dispatch requires the state, room registry and full sender guarded at
  `room_dispatch.rs:178-205`. The second recursively interprets room pipeline
  events, which may contain per-occupant `RouteToConnection` events. The
  room-origin relay dependency is set only under `clustering` and the
  conditional claim-fence branch at `room_dispatch.rs:814-824`.
- [`drain.rs:721`](../../../server/crates/waddle-server/src/room_effect_outbox/drain.rs#L721)
  -> [`queue_local` at `drain.rs:594`](../../../server/crates/waddle-server/src/room_effect_outbox/drain.rs#L594);
  [`drain.rs:610`](../../../server/crates/waddle-server/src/room_effect_outbox/drain.rs#L610)
  -> [`send_to_with_write_acceptance` at `sending.rs:248`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L248):
  `call`, `transport` and `call`, `derived-index;transport`, respectively.
  Recipients fan out with bounded concurrency while each recipient's stanzas
  remain sequential. On `NotConnected`,
  [`drain.rs:731`](../../../server/crates/waddle-server/src/room_effect_outbox/drain.rs#L731)
  -> [`registered_remote_resource_write_accepted_delivery` at `muc/implementation.rs:527`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/presence/muc/implementation.rs#L527).
  Local acceptance receivers are awaited by
  [`drain.rs:579`](../../../server/crates/waddle-server/src/room_effect_outbox/drain.rs#L579)
  -> [`await_acks` at `:629`](../../../server/crates/waddle-server/src/room_effect_outbox/drain.rs#L629)
  before that non-inline completion path. The acceptance contract is described
  at [`sending.rs:242`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L242);
  enqueue alone does not establish that acceptance or final completion.

## Production: carbons inventory consumers

- In the planning branch,
  [`carbons.rs:58`](../../../server/crates/waddle-server/src/server/routes/interpret/carbons.rs#L58)
  -> [`recipient_inventory` at `recipient_selection.rs:123`](../../../server/crates/waddle-server/src/server/routes/interpret/recipient_selection.rs#L123);
  its [`:140`](../../../server/crates/waddle-server/src/server/routes/interpret/recipient_selection.rs#L140)
  -> [`local_recipient_inventory` at `:89`](../../../server/crates/waddle-server/src/server/routes/interpret/recipient_selection.rs#L89):
  `call`, `authority`. Under `clustering`, a remote inventory may return at
  `:136-137` before the local edge. The local consumer's `:94` actor lookup is
  already represented by A09. Its returned live carbon flags are filtered at
  [`carbons.rs:66`](../../../server/crates/waddle-server/src/server/routes/interpret/carbons.rs#L66)
  to plan per-resource effects.
- [`carbons.rs:77`](../../../server/crates/waddle-server/src/server/routes/interpret/carbons.rs#L77)
  and [`:278`](../../../server/crates/waddle-server/src/server/routes/interpret/carbons.rs#L278)
  -> [`ConnectionRegistry::other_carbon_resources_for_user` at `resources.rs:182`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/resources.rs#L182):
  `call`, `derived-index`. The first is only the registry-only planning case
  when no user registry is supplied; the second reads current enabled entries
  in the immediate fanout helper. These are distinct policy sources from the
  actor snapshot and do not themselves prove a carbon was sent.

## Test-only and test-support entry and custody paths

- [`sm_promotion/tests.rs:69`](../../../server/crates/waddle-server/src/sm_promotion/tests.rs#L69)
  -> [`RegisterUserResource` handler at `user_registry.rs:1090`](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1090):
  `call`, `authority`, test actor ask. The helper registers on the connection
  registry and extracts the same entry at `:66-67`; callers include
  [`:567`](../../../server/crates/waddle-server/src/sm_promotion/tests.rs#L567)
  and [`:806-807`](../../../server/crates/waddle-server/src/sm_promotion/tests.rs#L806).
- [`execute_local_carbons_tests.rs:83`](../../../server/crates/waddle-server/src/ingress/execute_local_carbons_tests.rs#L83),
  [`:155`](../../../server/crates/waddle-server/src/ingress/execute_local_carbons_tests.rs#L155),
  [`:171`](../../../server/crates/waddle-server/src/ingress/execute_local_carbons_tests.rs#L171),
  [`:306`](../../../server/crates/waddle-server/src/ingress/execute_local_carbons_tests.rs#L306),
  and [`:323`](../../../server/crates/waddle-server/src/ingress/execute_local_carbons_tests.rs#L323)
  -> [`user_registry.rs:1090`](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1090):
  `call`, `authority`, test actor asks. The module is
  [`cfg(test)`](../../../server/crates/waddle-server/src/ingress/execute.rs#L262).
  At `:80-85` the first path registers, sets carbons on, and passes the same
  entry to the actor; its [`:91`](../../../server/crates/waddle-server/src/ingress/execute_local_carbons_tests.rs#L91)
  calls the planning helper at [`:16`](../../../server/crates/waddle-server/src/ingress/execute_local_carbons_tests.rs#L16).
  The receiver's shared-entry contract is documented at
  [`user_registry.rs:1077`](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1077),
  and the actor reads the carbon atomic at
  [`user_actor/implementation.rs:539`](../../../server/crates/waddle-xmpp/src/registry/user_actor/implementation.rs#L539).
- [`tests/ingress_cases/detached_progress.rs:47`](../../../server/crates/waddle-server/tests/ingress_cases/detached_progress.rs#L47),
  [`:347`](../../../server/crates/waddle-server/tests/ingress_cases/detached_progress.rs#L347),
  and [`tests/ingress_cases/user_actor_keyed_append.rs:31`](../../../server/crates/waddle-server/tests/ingress_cases/user_actor_keyed_append.rs#L31)
  -> [`user_registry.rs:1090`](../../../server/crates/waddle-xmpp/src/registry/user_registry.rs#L1090):
  `call`, `authority`, integration-test actor asks carrying the registered entry.
  The files are included by [`ingress_commit.rs:104`](../../../server/crates/waddle-server/tests/ingress_commit.rs#L104)
  and [`xep0198_ingress_authority.rs:498`](../../../server/crates/waddle-server/tests/xep0198_ingress_authority.rs#L498),
  respectively. The keyed-append case asserts queued `ingress_append` identity
  at [`user_actor_keyed_append.rs:75`](../../../server/crates/waddle-server/tests/ingress_cases/user_actor_keyed_append.rs#L75),
  not a physical socket write.
- [`websocket/test_state.rs:529`](../../../server/crates/waddle-server/src/server/routes/websocket/test_state.rs#L529)
  and [`interpret/tests/mod.rs:99`](../../../server/crates/waddle-server/src/server/routes/interpret/tests/mod.rs#L99)
  -> [`mirror_register` at `dual_registration.rs:109`](../../../server/crates/waddle-server/src/server/dual_registration.rs#L109):
  `call`, `authority`. Both obtain the just-registered `ConnectionEntry` from
  the connection registry before mirroring. The helper and the websocket test
  module require `test` or `test-support`, as shown at
  [`dual_registration.rs:108`](../../../server/crates/waddle-server/src/server/dual_registration.rs#L108)
  and [`websocket/mod.rs:116`](../../../server/crates/waddle-server/src/server/routes/websocket/mod.rs#L116).
  A concrete consumer is
  [`routing_route_to_connection.rs:122`](../../../server/crates/waddle-server/src/server/routes/interpret/tests/routing_route_to_connection.rs#L122),
  which writes shared presence at `:128-130` and interprets the route at `:142`.
- [`tests/supervised_io/connection_actor.rs:1457`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1457),
  [`:1519`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1519),
  [`:1742`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1742),
  and [`:1850`](../../../server/crates/waddle-server/tests/supervised_io/connection_actor.rs#L1850)
  -> [`TrySendDirectWithCustody` handler at `user_actor/delivery.rs:269`](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L269):
  `call`, `transport`, test-support actor asks. The suite is gated by
  [`tests/supervised_io.rs:3`](../../../server/crates/waddle-server/tests/supervised_io.rs#L3).
  The first ask exercises provisional refusal; the second occurs after
  publication and exercises acceptance. The handler is described as dark,
  without production callers at [`delivery.rs:261`](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L261).

## Dormant WebSocket actor state ownership

- [`actor_local_endpoint.rs:918`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L918)
  -> [`spawn_parked` at `connection_actor_driver_task.rs:433`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_driver_task.rs#L433),
  and [`actor_local_endpoint.rs:993`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L993)
  -> [`spawn_physical_parked` at `connection_actor_driver_task.rs:454`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_driver_task.rs#L454):
  `call`, `authority`. The second caller and callee require `test-support`;
  the first is release compiled but the endpoint is documented as
  [dormant and not selected by `/ws`](../../../server/crates/waddle-server/src/server/routes/websocket/actor_local_endpoint.rs#L1).
- [`connection_actor_driver_task.rs:484`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_driver_task.rs#L484)
  -> [`ConnectionActorCore::new` at `connection_actor_core.rs:92`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L92):
  `call`, `authority`, moving `WsConnState` into its owner. Separately,
  [`connection_actor_core.rs:142`](../../../server/crates/waddle-server/src/server/routes/websocket/connection_actor_core.rs#L142)
  -> [`handle_classified_xmpp_frame_with_admission` at `frame.rs:603`](../../../server/crates/waddle-server/src/server/routes/websocket/frame.rs#L603):
  `borrow-transfer`, `authority`, lending `&mut self.state`. The generic
  inbound seam has no established live producer; the module guidance at
  [`websocket/mod.rs:79`](../../../server/crates/waddle-server/src/server/routes/websocket/mod.rs#L79)
  explicitly keeps startup on the existing loop.

## Reproducible bounded method and limits

From the checkout root, use symbol searches to identify candidate senders,
then inspect each call, callee, branch and module/feature gate with line-numbered
source. The following searches reproduce the main discovery lanes:

```sh
rg -n 'mirror_register\(|mirror_unregister\(|rollback_registered_connection\(' server/crates --glob '*.rs'
rg -n '\.ask\((RegisterUserResource|TrySendDirectWithCustody|GetRoutingResources|GetResources)' server/crates --glob '*.rs'
rg -n 'route_room_presence_to_occupant|deliver_room_presence_locally|dispatch_to_room\(|queue_local\(' server/crates --glob '*.rs'
rg -n 'WsConnState|spawn_parked|spawn_physical_parked|process_inbound_frame' server/crates/waddle-server/src/server/routes/websocket --glob '*.rs'
rg -n 'recipient_inventory\(|other_carbon_resources_for_user\(' server/crates --glob '*.rs'
```

For each manually confirmed source edge, compare the exact source path/line
and destination path/line against the supplied `indirect-edge-candidates.tsv`,
then assess it independently rather than deriving candidates from the review
TSV. Text search misses aliases, multiline method construction, trait dispatch,
callbacks, generated code and runtime-selected branches. The searches do not
cover all selection consumers, test fixtures, relay/receipt fanout, lifecycle
transitions, WebSocket mutable borrows, or every build profile. No validator,
Cargo command, test, runtime exercise, Git mutation, or remote check was run
for this discovery. No validator coverage or completeness is claimed.
