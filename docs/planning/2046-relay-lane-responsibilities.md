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
  and decides the head entry from canonical rows. Occupancy is in room-actor
  memory and is not recovered: a successor that does not know the sender as
  an occupant refuses the stanza, and that refusal is the recorded decision.
- **Deletion precondition.** Protocol epoch 2 on every deployment that
  clusters. A deployment without the restricted owner phase stays at epoch 0
  and keeps the relay.

### MUC join

- **Status.** Not cut over; relay at every epoch.
- **Identity.** A server-minted join occurrence, `RoomJoin(JoinOccurrenceKey)`,
  minted by the origin before its first owner attempt and reused by every
  repair of that attempt; a later join of the same occupant is a new
  occurrence. The logical lane origin is the bound full JID with its
  `OccupancySessionGeneration`, so the session's own attempt and the
  `UserActor` repair share one lane and one sequence space.
- **Owner.** The node that holds the room's claim.
- **Acceptance commits.** The occupancy row for `(room, occupant JID,
  generation)`, the frozen join response (self-presence with status codes,
  occupant presences, history and subject in the XEP-0045 §7.2 order) as
  child delivery keys addressed to the joining resource, and the entry's
  disposition.
- **Consumption.** The owner reads the join presence from a canonical request
  row. The response children are consumed by the node that hosts the joining
  resource, at the full-JID frontier below.
- **Lost hint.** Poller, as for groupchat. The entry precedes any groupchat
  entry of the same session on the `MucStanza` lane, and the cursor moves only
  to its immediate successor, so a message never overtakes its join.
- **Owner transfer.** The successor must load the room's occupancy rows before
  it decides any entry of the lane. Without durable occupancy it fails closed:
  it refuses dependent entries and never infers occupancy from a cursor that
  has passed a join.
- **What does not exist.** (1) A canonical request row for a non-message
  stanza: an entry's foreign key and `UNIQUE (message_key, operation_kind)`
  refer to `ingress_messages`, and `MessageEnvelope` holds only messages.
  (2) Durable occupancy: occupants live in room-actor memory. (3) Durable
  custody of the join response at the joining resource. Each needs a ledgered
  migration under the epoch guard and its own retention rule; none is
  designed in the documents this record follows.
- **Decision.** The join stays on the relay until those three exist. No
  mailbox row is written for a join, so there is no second authority.
- **Deletion precondition.** The three items above, then epoch-gated removal
  of `JoinPresence` and the join repair, with a `deliver_ordered` version bump
  and rejection of older peers before admission.

### Occupant presence (update, leave, cleanup replay)

- **Status.** Not cut over; relay at every epoch.
- **Identity.** `RoomPresence(occurrence)` and `RoomLeave(occurrence)`, minted
  like the join occurrence, on the same lane as the join they follow. A
  cleanup replay reuses the leave occurrence minted when the connection ended
  and carries the occupancy generation it fences.
- **Owner.** The room claim holder.
- **Acceptance commits.** The occupancy row change fenced by generation (a
  stale generation is a recorded no-op), the frozen presence broadcast as
  child keys, and the disposition.
- **Consumption, lost hint, transfer.** As for the join. A leave whose
  occupancy row is absent on a successor is accepted as already applied.
- **What does not exist.** The same three items as the join. Cleanup retry
  custody across a restart additionally needs the leave occurrence to be
  committed by the origin before the connection's state is dropped.
- **Decision and deletion precondition.** With the join.

### Muji signalling

- **Status.** Not cut over; relay at every epoch.
- **Identity.** `MujiSignal(occurrence)` per relayed Jingle IQ, on the
  `MujiSignaling` room lane, which has its own sequence and cursor and
  neither blocks nor is blocked by the `MucStanza` lane.
- **Owner.** The room claim holder, which runs the call mixer for the room.
- **Acceptance commits.** The IQ's frozen result or error and the
  server-initiated `session-accept`, as child keys addressed to the
  requesting resource, with the disposition. Call state remains with #1663.
- **Consumption.** The owner reads the IQ from a canonical request row; the
  IQ response is completed at the requesting resource's frontier.
- **What does not exist.** A canonical request row for an IQ, durable
  response custody, and durable occupancy for the membership gate.
- **Decision.** Stays on the relay. The executor re-checks membership against
  the local room actor, so a successor without the occupant answers with an
  IQ error, which is terminal and not retried.
- **Deletion precondition.** As for the join, for the IQ request and
  response; then removal of `MujiJingleIq` with a version bump.

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

`deliver_ordered` stays at `v14`: no lane's variant has been removed and
neither the envelope nor the reply changed. The groupchat cutover removes a
sender, not a variant: `GroupchatMessage` is still sent below epoch 2 and
under the `relay` selector, and a node at any version still accepts it. Removing a variant bumps the
suffix in `S/clustering/relay/implementation.rs`; a peer that still registers
the old id then answers an unknown-message error before it admits anything,
and the sender treats that as no effect.

## What still uses the ordered relay

| Configuration | Groupchat | Join, presence, Muji | Bare and full JID |
| --- | --- | --- | --- |
| Clustered PostgreSQL, epoch ≥ 2, selector `mailbox` | mailbox; relay only for a message committed without a source binding below epoch 2 | relay | relay |
| Clustered PostgreSQL, epoch ≥ 2, selector `relay` | relay | relay | relay |
| Clustered PostgreSQL, epoch < 2 (no restricted owner phase) | relay | relay | relay |
| Single node | none | none | none |
