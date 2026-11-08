# P3 outbound edge review

Source review target: `main@4b1fd1f348393337c827fb98ac4dea64f0f9dcf3`.
The compiler-marker inventory is pinned to `3ae674c121c06b44934c915ecc0ef469dca54833`.
The 52 files in its target manifest plus the selected P3 edge files comprise
56 unique paths. Their Git blobs are identical at both commits; see
[`source-provenance.tsv`](source-provenance.tsv). The review below therefore
describes the selected P3 paths at both source revisions. It is a bounded edge
review; the all-caller semantic read/write checklist remains open.

## Producer, admission, and receiver edges

| Edge | Current source path | Review result |
| --- | --- | --- |
| Envelope contract | [`OutboundStanza`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/outbound.rs#L81) owns a stanza, delivery kind, pending-row metadata, optional write-acceptance token, and ingress obligation. Its constructors distinguish direct, peer, pending-flush, and acceptance-aware envelopes. | An accepted item is more than the serialized stanza bytes; custody must account for its metadata and retain disposition state. |
| Ordinary/local route producer | [`deliver_direct_to_full_locally`](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1423) constructs a direct envelope and enqueues it for a locally hosted full JID. [`deliver_ordered_local_copy`](../../../server/crates/waddle-server/src/server/routes/interpret/route_to_connection/implementation.rs#L1449) freezes the entry/owner before asynchronous readiness work, then captures an owner-checked enqueue. | Direct and ordered paths converge at registry admission, with distinct durability and retry semantics. |
| UserActor producer | [`delivery.rs`](../../../server/crates/waddle-xmpp/src/registry/user_actor/delivery.rs#L241) creates direct, peer, and pending-flush envelopes; the actor selection path dispatches them to the selected full-JID entry. | The UserActor selection result determines the local resource. A later registry lookup must still check the selected binding where the caller requires owner stability. |
| Pending replay | [`pending_delivery/flush.rs`](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L626) creates a row-tagged replay envelope and [`send_to_if_owner`/`send_to`](../../../server/crates/waddle-server/src/pending_delivery/flush.rs#L728) admits it. | The row is acknowledged only after the receiving WebSocket write/recovery path resolves its `OutboundWriteAcceptance`; channel enqueue alone is not acceptance. |
| Ordered archive dispatch | [`archive_dispatch.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L78) routes an owned envelope through the per-entry ordered path; archive-generated frames attach ingress obligations at [the constructor](../../../server/crates/waddle-xmpp/src/registry/connection_registry/archive_dispatch.rs#L217). | This caller preserves archive order and carries a second durable obligation with the stanza. |
| Room-effect delivery | [`queue_local`](../../../server/crates/waddle-server/src/room_effect_outbox/drain.rs#L594) submits local room-effect stanzas through `send_to_with_write_acceptance`. | Durable room-effect completion waits on the receiver's write/recovery signal, not on endpoint admission. |
| Remote-resource delivery | [`remote_socket.rs`](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L558) reconstructs envelopes from remote frames; [`try_send_outbound_if_owner`](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L569) checks the registered local owner. The acceptance-aware frame path starts at [line 626](../../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_socket.rs#L626). | Registration ID, socket generation, and current connection identity guard remote frames before local enqueue. A remote `Delivered` response for an acceptance-aware frame waits for the writer acknowledgment. |
| Remote owner mirror queue and forwarder | [`registration/owner.rs`](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L370) creates a legacy count-bounded `SocketOutboundEndpoint` queue for the remote-hosted `ConnectionEntry`, then starts the forwarder at [line 515](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/owner.rs#L515). [`registration/socket_forwarder.rs`](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket_forwarder.rs#L27) dequeues one original envelope at a time and awaits the remote relay at [line 125](../../../server/crates/waddle-server/src/clustering/route_bridge/registration/socket_forwarder.rs#L125). | This owner-side queue is a distinct in-flight custody stage from the socket-node queue in `remote_socket.rs`. The forwarder holds a dequeued envelope across relay awaits; no per-envelope byte ledger is visible on this legacy mirror path. Acceptance-aware frames preserve their token end to end, but ordinary mirror frames have only enqueue/relay reply semantics. |
| Roster fanout | [`roster_fanout.rs`](../../../server/crates/waddle-server/src/server/routes/websocket/handlers/roster_fanout.rs#L114) sends through an owner-checked entry. | This is an additional production fanout producer; it shares admission but not the room/pending durable acceptance contract. |
| Registry admission | [`sending.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L534) looks up the full-JID entry for ordinary sends. Owner-qualified sends compare the entry binding and then use [`try_send_archive_ordered`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/sending.rs#L602). The entry endpoint commits through its binding admission checks and queue sender in [`endpoint.rs`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/endpoint.rs#L345). | Ordinary sends and owner-qualified sends share the endpoint but differ in the freshness proof at admission. `AcceptedOrUnknown` is not a safe retry proof. |
| Live `/ws` queue | The live socket sets [`OUTBOUND_CHANNEL_SIZE = 256`](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L210) and creates [`channel_with_socket_receiver`](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L462). Registration wraps that endpoint in a [`ConnectionEntry`](../../../server/crates/waddle-server/src/server/routes/websocket/registration.rs#L643). | This is the active legacy endpoint. The receiver queue is bounded by item count; it has no per-envelope serialized-byte budget. |
| Live receiver and writer | The live loop receives an envelope at [`connection.rs:863`](../../../server/crates/waddle-server/src/server/routes/websocket/connection.rs#L863) and delegates to [`handle_outbound_stanza`](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L26). For SM, the writer records the wire representation in recovery before sink write; without SM, successful sink write is the acceptance point. It acknowledges only after that point at [`outbound.rs:184`](../../../server/crates/waddle-server/src/server/routes/websocket/outbound.rs#L184). | Receiver dequeue is not terminal custody. The acceptance token is resolved by the connection's write/recovery owner, not registry enqueue. |
| Dual registration and mirrors | Bind registration publishes the connection entry and then calls the [`mirror_register` path](../../../server/crates/waddle-server/src/server/dual_registration.rs#L109); `UserRegistryActor` stores resource clones and handles owner-aware registration/removal. Remote owner registration separately creates and publishes remote entries. | The mirrors are active lookup/teardown and remote-routing machinery. They cannot be dropped until replacement selection and retirement are wired. |
| Room routing | [`RoomMessageRoute`](../../../server/crates/waddle-server/src/server/routes/interpret/room_routing.rs#L15) splits planning from execution; [`dispatch_to_room`](../../../server/crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L171) selects the local room actor path; relay planning is in [`relay_plan.rs`](../../../server/crates/waddle-server/src/server/routes/interpret/relay_plan.rs#L10). | Room route planning and local dispatch are distinct from remote-resource socket admission and from durable room-effect completion. |

## Byte-budget and reclamation finding

The active `/ws` endpoint uses `mpsc::channel(capacity)` in
[`SocketOutboundEndpoint::channel_with_socket_receiver`](../../../server/crates/waddle-xmpp/src/registry/connection_registry/endpoint.rs#L145).
Its capacity is 256 envelope items, not bytes. The queue stores the original
`OutboundStanza` values, so a count cap alone does not bound their aggregate
payload size.

`ActorOutboundCustody` does have a count limit and a serialized stanza plus
envelope-metadata byte budget ([fields](../../../server/crates/waddle-xmpp/src/registry/connection_registry/actor_outbound.rs#L72),
[admission](../../../server/crates/waddle-xmpp/src/registry/connection_registry/actor_outbound.rs#L153)).
The actor-local endpoint can create it, but the live `/ws` path selects the
legacy `SocketOutboundEndpoint` channel. The only in-repository
`start_actor_local_endpoint` occurrences are its API definition and the
internal call to `ActorLocalEndpointOwner::start`; no production `/ws` caller
selects this actor endpoint. Its byte accounting is therefore not evidence of
budgeting or reclamation for the active legacy envelope queue.

The serialized writer/transport queues and the SM recovery queue are later
ownership stages. Their limits do not provide an admission-time byte budget
for the original envelope while it is queued or dequeued before serialization.
The first P3 bounded-envelope contract should reserve count and bytes before
publication, retain the original-envelope credit across queue/dequeue and
unknown completion, and release it exactly once when a terminal disposition
or explicit downstream ownership transfer is proven. Keep any serialized
writer budget separate from the original-envelope budget.

## Scope limit

This review manually classifies the listed producer/admission/receiver edge
families. The compiler TSVs supply type-resolved declaration-use spans for the
selected builds, but they do not assign semantic read/write roles. The
remaining source rows are explicitly `manual-review-required`; the full
ConnectionRegistry/ConnectionEntry/WsConnState caller and authority checklist
stays open.
