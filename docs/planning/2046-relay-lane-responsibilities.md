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
- **Copies.** Every copy is a child of the sealed manifest and has one
  durable send attempt (`ingress_send_attempts`, keyed by message, receipt
  and recipient). The node that hosts the target claims that attempt before
  it writes to the socket, so a copy is written once whichever node offers
  it and however often. The owner's live execution and its recovery
  (`deliver_owner_obligations`) send a copy for a resource on another node
  over the room's ordered channel with the copy's canonical obligation
  (`append_sealed_remote` in `S/ingress/execute_relay_copy.rs`); the hosting
  node's answer is the child's outcome and the owner records it. When the
  owner ends between its commit and its execution, each node's ingress
  maintenance pass delivers the children whose targets it hosts and records
  them, without the owner.
- **Proof of the frozen occupancy.** On the node that runs the actor the
  manifest was sealed by, that actor checks the child's occupancy watermark
  in the turn that writes the copy. The incarnation and the watermarks exist
  only in that actor, so any other node proves the child against the room's
  durable occupancy (`room_manifest_replay`): the copy is written to a
  connection while `ingress_room_occupants` seats the target under that
  connection's generation. A seat held by another generation, or no seat,
  leaves the copy undelivered as for a target that is not connected. Once
  the room's claim has left the incarnation that sealed the manifest, a
  child whose target has no seat is settled as departed
  (`OccupancyDeparted`); while that claim stands the owner's actor decides.
  The manifest does not record the generation that held the seat when it
  was sealed, so a resource that left and joined the room again on the same
  connection before a recovery pass receives the copy.
- **Not covered.** A target whose only session is a detached
  stream-management session on a node without the sealing actor is not
  appended to; its copy stays owed until the session resumes or its seat is
  removed.
- **Lost hint.** The owner's poller scans its backlog every second. A pass
  works its lanes two at a time and waits for both, so one lane held for
  its receiver budget delays every other lane of the node by that long.
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
- **Seating on any takeover.** A node seats a room's durable occupancy
  once per claim epoch (`seat_room_occupancy_in` in
  `S/clustering/route_bridge/mailbox.rs`) before it decides a mailbox entry
  of the room and before it reads the roster for a groupchat stanza of one
  of its own connections (`ensure_room_occupancy_seated`, called by
  `dispatch_to_room` in `S/server/routes/interpret/room_dispatch.rs`). A
  node that took the claim through its orphan reaper, a steal or a restart
  therefore serves an occupant connected to it without a new join. When the
  occupancy cannot be loaded and the sender is not in the actor's roster,
  the stanza is answered with `internal-server-error`, not `not-acceptable`.
- **Occupancy writer.** The room actor records every seat it grants or
  removes in `ingress_room_occupants` under the room claim, including a
  resource connected to the owner itself and removals by kick, ban,
  affiliation change, the ghost sweep and destroy; the join's decision
  completes the row. A successor seats each row through a join projection,
  which refuses a row whose connection generation is no longer current, and
  removes the rows it did not seat. The owner's groupchat commit reads the
  table once the room is seated.
- **Receiver.** A node that carries room requests through the mailbox
  refuses `JoinPresence` on the relay with the unsupported-envelope
  outcome, so the join repair in `route_bridge/delivery/muc/proxy.rs` has
  no effect against such an owner. The repair still exists for the relay
  path.
- **Deletion precondition.** Protocol epoch 2 on every deployment that
  clusters, then removal of `JoinPresence` and the join repair.

### Occupant presence (update, leave, cleanup replay)

- **Status.** Presence updates and leaves sent by a client are cut over with
  the join. The leave replayed when a connection ends
  (`websocket/cleanup.rs`, `route_bridge/delivery/muc/cleanup.rs`) is still
  sent on the relay; an owner at the mailbox epoch refuses it there.
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
- **Connection end.** The connection's cleanup commits the leave as a
  request (`commit_room_leave_for_ending_connection` in
  `S/server/routes/websocket/cleanup.rs`). A connection whose node is
  killed commits nothing: its occupant stays seated at a mailbox-epoch
  owner until the ghost sweep removes it, and that removal deletes the row.
- **Deletion precondition.** Removal of `OccupantPresence`.

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

Decided in [`2173-direct-lanes.md`](2173-direct-lanes.md).

- **Status.** Cut over for a send whose origin commits it at protocol epoch
  2 or later under the `mailbox` selector with
  `WADDLE_XMPP_DURABLE_SM=durable`, when the recipient account has a
  `UserActor` claim. Relay otherwise, and always for a send whose origin
  resource is hosted through a remote channel mirror.
- **Identity.** `Direct(DirectRequestKey)`. A copy an ingress transaction
  owns derives the key from its canonical message key, its receipt key and
  the recipient; any other stanza mints one. The request row
  (`ingress_direct_requests`) holds the payload the relay envelope carries.
- **Owner.** The node that holds the recipient account's `UserActor` claim,
  for both recipient kinds.
- **Acceptance commits.** The decision, the entry's disposition and the
  lane cursor, in one transaction that asserts the account claim. Custody
  is taken by the recipient's session path: a live connection admits the
  frame to durable stream management on its own fence. A copy with an
  ingress receipt is handed over before the decision and recognised on a
  repeat; any other stanza is handed over once, after the decision.
- **Consumption.** The origin waits up to ten seconds, consumes the entry
  and settles the request. Accepted is `Delivered`; rejected is
  `Unavailable`, from which the origin runs the fallback it runs today; no
  decision is `MaybeCommitted`, after which nothing else carries the
  stanza.
- **Lost hint.** The target's poller.
- **Owner transfer.** The adoption pass writes the account's current
  `UserActor` claim onto a backlogged lane with a `BareJid` or `FullJid`
  recipient when this node holds it. Resources are never reselected for a
  decided entry.
- **What does not exist.** Admission to a detached durable session from
  outside a connection: such a recipient is rejected and the origin stores
  the message offline, as the relay does in the same configuration. A
  writer of the "hosted elsewhere" refusal. A sweep of unconsumed
  decisions.
- **Deletion precondition.** Every clustered deployment at epoch 2 or later
  with the `mailbox` selector and durable stream management, the `relay`
  selector and the `legacy` mode removed, and remote channel mirrors
  retired; then removal of the `BareJid` and `FullJid` recipients with a
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
without a version bump, and so does the direct-lane cutover: the envelope
and the reply types are unchanged, a receiver still accepts `BareJid` and
`FullJid` payloads, and the refusal of relayed room kinds by a
mailbox-epoch receiver uses the existing `UnsupportedEnvelope` reason.

## What still uses the ordered relay

| Configuration | Groupchat | Join, presence, Muji | Bare and full JID |
| --- | --- | --- | --- |
| Clustered PostgreSQL, epoch ≥ 2, selector `mailbox`, durable stream management | mailbox; relay only for a message committed without a source binding below epoch 2 | mailbox for client-sent joins, presence updates, leaves and Muji signalling; the relay kinds are refused by the receiver, including the leave replayed at connection end | mailbox; relay for a remote-mirror origin, an account without a claim and a request that could not be committed |
| Clustered PostgreSQL, epoch ≥ 2, selector `mailbox`, legacy stream management | as above | as above | relay |
| Clustered PostgreSQL, epoch ≥ 2, selector `relay` | relay | relay | relay |
| Clustered PostgreSQL, epoch < 2 (no restricted owner phase) | relay | relay | relay |
| Single node | none | none | none |

No part of `ordered_relay.rs`, `route_bridge/delivery/ordered*.rs` or the
receiver is unused in every configuration, so none is deleted. Final
removal needs every clustered deployment at epoch 2 or later with the
`relay` selector and legacy stream management removed, and remote channel
mirrors retired.
