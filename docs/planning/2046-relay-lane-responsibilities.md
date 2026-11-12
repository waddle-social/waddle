# Ordered relay consumers and their durable responsibilities

This record lists everything the ordered relay
(`waddle.clustering.relay.deliver_ordered.vN`, `S/clustering/ordered_relay.rs`,
`S/clustering/route_bridge/delivery/`) carries, and for each consumer the
durable responsibility that replaces it (#2046 package C; #2173, #2174,
#2175). `S/` is `server/crates/waddle-server/src/`, `X/` is
`server/crates/waddle-xmpp/src/`. The mailbox itself is described in
[`p2-2-durable-mailbox-core.md`](p2-2-durable-mailbox-core.md).

A lane is cut over when its mailbox responsibility is authoritative in every
ingress transaction at protocol epoch 2 or later; the relay keeps serving the
lane below that epoch, where no mailbox row may be written. A lane that is not
cut over is served by the relay at every epoch.

## What `deliver_ordered` carries

One envelope type, `RemoteStanzaEnvelope`, with a channel
`(origin, origin epoch, recipient, target epoch)` and one payload:

| Payload | Recipient key | Producers |
| --- | --- | --- |
| `MucProxy { kind: GroupchatMessage, origin: Server }` | `Room { lane: MucStanza }` | `plan_remote_room` in `S/server/routes/interpret/room_dispatch.rs`, executed by `ExternalRoomEffect::RelayMucProxy`; `RoomMessageRoute::execute` in `room_routing.rs` for a dispatch outside the ingress plan |
| `MucProxy { kind: JoinPresence, origin: Connection(generation) }` | `Room { lane: MucStanza }` | the join handler (`handlers/presence/muc/implementation.rs`); the join repair in `route_bridge/delivery/muc/proxy.rs`, which re-sends the join from a `UserActor` entity origin with `canonical: None` |
| `MucProxy { kind: OccupantPresence, origin: Connection(generation) }` | `Room { lane: MucStanza }` | presence updates and leaves (`handlers/presence/implementation.rs`, `handlers/presence/muc/implementation.rs`); the generation-fenced cleanup replay (`websocket/cleanup.rs`, `route_bridge/delivery/muc/cleanup.rs`) |
| `MucProxy { kind: MujiJingleIq, origin: Connection(generation) }` | `Room { lane: MujiSignaling }` | relayed Muji `session-initiate`/`session-terminate` (`handlers/iq/sans_io.rs`) |
| `MucProxy` kinds `PrivateMessage`, `BareRoomIq`, `OccupantIq`, `FanoutChunk` | `Room { lane: MucStanza }` | none; the receiver answers them with `ParseFailure` |
| `ProcessedDirectMessage`, `Message` | `FullJid`, `BareJid` | `ExternalDeliveryEffect::RelayFullJid`, `RelayBareJid`, `RelayCarbons` and the remote occupant copies of a room fan-out (`route_bridge/delivery/ordered.rs`, `ordered_send.rs`) |
| `Iq`, `Presence` | `FullJid`, `BareJid` | direct IQ and presence routing to a resource hosted elsewhere |

The reply (`OrderedRelayReply`) carries the owner's frames for the sender and,
for an ingress-owned delivery, a receipt token the origin confirms after it
has written those frames.

Relay messages other than `deliver_ordered` (remote resource registration and
frames, `mailbox_hint.v1`, MUC cleanup, admin queries, extension room sends,
resume steal) are not ordered lanes and are not part of this record.

## Responsibilities

Each row names: the operation identity, the owner, what the owner's acceptance
commits, how the target consumes the work, how a lost hint is repaired, what
an owner transfer does, and what must hold before the relay path is deleted.

### Groupchat transport

- **Status.** Cut over. In a transaction at protocol epoch 2 or later that
  appends or finds the mailbox entry, the origin sends no relay payload
  (`without_relayed_dispatch` in `S/ingress/commit_mailbox.rs`). Below that
  epoch, under the `relay` selector, and for a message without a source
  binding, the relay carries the stanza as before.
- **Identity.** `MailboxOperationKey::RoomAcceptance(MessageKey)`, unique
  across lanes. The canonical request is the `ingress_messages` row the origin
  committed, with its source binding.
- **Owner.** The node that holds the room's claim.
- **Acceptance commits.** In one transaction: the sealed room manifest
  (audience, child delivery keys, sender reflection) or the rejection child
  with its frozen error; `owner_decision`; the entry's disposition; the lane
  cursor; and the receipt of the origin's `DispatchToRoomRemote` intent, whose
  obligation is to bring the request to the owner.
- **Consumption.** The owner reads the stanza from the canonical envelope the
  entry refers to, the principal from the source binding and the stanza
  language from the entry, and runs its ordinary owner commit. The origin
  sends no payload. The origin reads the decision from the entry, proves it
  against `owner_decision`, consumes the entry and only then counts the
  stanza. A refusal is answered by the origin from the rejection child
  (`frozen_room_refusal`), which also receipts the source's `ErrorReply`
  obligation. An acceptance's reflection is the owner's own frozen
  obligation: the owner's worker repairs the message's unreceipted
  obligations as soon as its commit has executed
  (`deliver_owner_obligations`), and the ingress maintenance pass repeats
  that for anything still owed after a minute.
- **Lost hint.** The owner's poller scans its backlog every second.
- **Owner transfer.** The successor adopts the lane once it holds the claim
  and has seated the room's durable occupancy in its room actor, then
  decides the head entry from canonical rows. An occupant without a row in
  `ingress_room_occupants` is not recovered: a successor that does not know
  the sender as an occupant refuses the stanza, and that refusal is the
  recorded decision.
- **Deletion precondition.** Protocol epoch 2 on every deployment that
  clusters. A deployment without the restricted owner phase stays at epoch 0
  and keeps the relay.

### MUC join

- **Status.** Cut over for every clustered PostgreSQL deployment at protocol
  epoch 2 or later under the `mailbox` selector: the join handler commits a
  room request and sends no relay payload. Relay below epoch 2 and under the
  `relay` selector. Decided in
  [`2174-room-request-lanes.md`](2174-room-request-lanes.md).
- **Identity.** `RoomJoin(RoomRequestKey)`, minted by the origin's handler
  before its first owner attempt; a later join of the same occupant is a new
  key. The lane origin is the bound full JID with its
  `OccupancySessionGeneration` (`MailboxLaneOrigin::BoundResource`).
- **Owner.** The node that holds the room's claim.
- **Acceptance commits.** One transaction (`decide_room_request` in
  `S/ingress/room_request.rs`) asserts the room claim, latches the request's
  decision, stores the join handler's reply as ordered rows of
  `ingress_room_responses` (other occupants' presence, self-presence with
  status codes, history, subject), upserts the occupant's row in
  `ingress_room_occupants` and moves the lane's cursor. The handler has
  already changed the room actor and fanned presence out to the other
  occupants when that transaction runs; a retry of the same entry runs the
  handler again and keeps the first recorded decision and response.
- **Consumption.** The owner reads the join presence from
  `ingress_room_requests`. The origin reads the response in the transaction
  that consumes the entry and sets `settled_at`, and hands the frames to the
  requester's connection as the handler's reply.
- **Lost hint.** Poller, as for groupchat. A groupchat entry of a resumable
  stream sits on the session's lane, so the join's cursor does not order it:
  the origin holds a groupchat stanza while the same connection's join is
  undecided, and the owner's room actor refuses a sender it has not seated.
- **Owner transfer.** The successor loads the room's occupancy rows and
  seats them in its room actor (`SeedRecoveredOccupants`) before it decides
  any entry of the room; lanes are adopted only after that load. Voice
  requests, pending mediated invitations, call and Muji session state and
  rate-limit state are not stored and are not recovered.
- **What does not exist.** (1) A join accepted for a resource connected to
  the owner itself writes no occupancy row, so such an occupant is not
  recovered by a successor and must rejoin. (2) The owner's groupchat commit
  does not read `ingress_room_occupants`; it relies on the room actor, which
  is seated from those rows. (3) An occupant removed by a kick, a ban or the
  ghost sweep keeps its row until its next leave or join. (4) The join
  repair in `route_bridge/delivery/muc/proxy.rs` still exists for the relay
  path.
- **Deletion precondition.** Protocol epoch 2 on every deployment that
  clusters, then removal of `JoinPresence` and the join repair.

### Occupant presence (update, leave, cleanup replay)

- **Status.** Presence updates and leaves sent by a client are cut over with
  the join. The leave replayed when a connection ends
  (`websocket/cleanup.rs`, `route_bridge/delivery/muc/cleanup.rs`) is still
  relayed at every epoch.
- **Identity.** `RoomPresence(RoomRequestKey)` and
  `RoomLeave(RoomRequestKey)`, on the lane of the connection that sent them.
- **Owner.** The room claim holder.
- **Acceptance commits.** As for the join: the decision, the frozen reply and
  the occupancy change (a presence update rewrites the row of the request's
  generation, a leave deletes it). A request whose generation is not the one
  seated for the full JID is accepted without running the handler and
  answered with nothing. A requester whose row cannot be parsed is refused
  with `not-acceptable`.
- **Consumption, lost hint, transfer.** As for the join.
- **What does not exist.** The connection-end leave is not committed as a
  request: `IngressAuthority::commit_room_leave_at_connection_end` exists and
  has no caller. Until it is called from the connection's cleanup, a
  connection that ends without a leave keeps its occupancy row until the
  relayed cleanup replay or the ghost sweep removes the occupant, and that
  removal does not delete the row.
- **Deletion precondition.** The cleanup call site, row deletion on the
  owner's own removals, then removal of `OccupantPresence`.

### Muji signalling

- **Status.** Cut over for every clustered PostgreSQL deployment at protocol
  epoch 2 or later under the `mailbox` selector; relay otherwise.
- **Identity.** `RoomIq(RoomRequestKey)` per relayed Jingle IQ, on the
  `MujiSignaling` room lane, which has its own sequence and cursor and
  neither blocks nor is blocked by the `MucStanza` lane.
- **Owner.** The room claim holder, which runs the call mixer for the room.
- **Acceptance commits.** The IQ's result or error and the server-initiated
  `session-accept` as the frozen response, with the decision and the entry's
  disposition. Call state remains with #1663.
- **Consumption.** The owner reads the IQ from `ingress_room_requests`; the
  origin hands the response to the requesting resource when it consumes the
  entry.
- **Owner transfer.** The executor checks membership against the room actor,
  which a successor seats from durable occupancy. Call state is not
  recovered, so a successor answers a terminate for a session it does not
  know as the actor does for unknown state.
- **Deletion precondition.** Protocol epoch 2 on every deployment that
  clusters, then removal of `MujiJingleIq`.

### Full-JID and bare-JID delivery

- **Status.** Not cut over; relay at every epoch.
- **Identity.** The canonical delivery key of the frozen copy
  (`DeliveryKey` under its `MessageKey`), which already exists for
  ingress-owned messages. IQ and presence copies have no canonical row.
- **Owner.** For a full JID, the node that hosts the resource's session; for
  a bare JID, the node that holds the `UserActor` claim, which freezes the
  resource selection at acceptance.
- **Acceptance commits.** The frozen recipient resources and their delivery
  keys.
- **Consumption.** Custody ends at an exact stream-management append:
  `DurableSmStore::admit` with `AdmissionAuthority::Detached { session,
  generation }` and an operation id derived from the delivery key, followed
  by the entry's consumption. Admission is idempotent under its operation id,
  so a repeated attempt after an indeterminate commit appends nothing twice.
  A resource without stream management records `AttemptedUncertain` after a
  real transport attempt.
- **Lost hint.** The target's poller.
- **Owner transfer.** A session that moved resumes under a new generation; an
  admit against the old one is refused and the entry is retried against the
  session's current owner. Resources are never reselected.
- **What does not exist.** The live handlers do not use the durable
  stream-management store (`WADDLE_XMPP_DURABLE_SM` defaults to `legacy`, and
  durable mode shares its tables with the legacy store until the handlers are
  wired), so no target can resolve a full JID to the `(session, generation)`
  an admit needs or expose an admitted frame to a live transport. IQ and
  presence copies have no canonical request row. A lane whose recipient is a
  user has no target-side adoption rule: lane adoption reads the room claim
  only.
- **Decision.** Stays on the relay, whatever `WADDLE_XMPP_DURABLE_SM` says.
- **Deletion precondition.** Durable stream management authoritative in the
  live handlers; a canonical request for IQ and presence copies; user-lane
  adoption; then removal of the `BareJid` and `FullJid` recipients with a
  version bump.

### Reserved MUC kinds

`PrivateMessage`, `BareRoomIq`, `OccupantIq` and `FanoutChunk` have no
producer and no receiver. They are removed with the `MucStanza` lane and need
no replacement.

## Wire version

`deliver_ordered` is at `v16`. `v15` marks that a peer's join, presence
update and leave handlers send no `JoinPresence` or `OccupantPresence`
payload at the mailbox epoch; `v16` marks the same for `MujiJingleIq`.
Neither the envelope nor the reply changed and no variant was removed: the
kinds are still sent below epoch 2 and under the `relay` selector, and a
node at the current id still accepts them. The id lives in
`S/clustering/relay/implementation.rs`; a peer that still registers an
older id answers an unknown-message error before it admits anything, and
the sender treats that as no effect. The groupchat cutover removed a sender
without a version bump.

## What still uses the ordered relay

| Configuration | Groupchat | Join, presence, Muji | Bare and full JID |
| --- | --- | --- | --- |
| Clustered PostgreSQL, epoch ≥ 2, selector `mailbox` | mailbox; relay only for a message committed without a source binding below epoch 2 | mailbox for client-sent joins, presence updates, leaves and Muji signalling; relay for the leave replayed at connection end | relay |
| Clustered PostgreSQL, epoch ≥ 2, selector `relay` | relay | relay | relay |
| Clustered PostgreSQL, epoch < 2 (no restricted owner phase) | relay | relay | relay |
| Single node | none | none | none |
