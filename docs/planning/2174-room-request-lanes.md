# Room requests, durable occupancy and response custody

Decision record for moving MUC joins, occupant presence, leaves
([#2174](https://github.com/waddle-social/waddle/issues/2174)) and Muji
signalling ([#2175](https://github.com/waddle-social/waddle/issues/2175)) from
the ordered relay to the durable mailbox. It defines the three things
[`2046-relay-lane-responsibilities.md`](2046-relay-lane-responsibilities.md)
lists as missing. `S/` is `server/crates/waddle-server/src/`, `X/` is
`server/crates/waddle-xmpp/src/`.

Status: **built, not yet verified by a test run**. Migration V1032, the
three repositories, the owner's decision transaction, the origin's commit
and hand-over, occupancy seating at claim takeover and the Muji path are in
the tree; `deliver_ordered` is at `v16`. At protocol epoch 2 or later,
client-sent joins, presence updates, leaves and Muji
signalling for a room owned by another node are room requests; the relay
serves them below that epoch. The PostgreSQL
suite `room_requests_postgres` and the two-node suite
`xep0045_room_request_lanes` compile and have not been run.

Where the code differs from the decisions below:

- **Grants.** The runtime holds `SELECT, INSERT, UPDATE, DELETE` on
  `ingress_room_requests`, because the retention pass deletes requests as the
  runtime role, and `SELECT, INSERT` on `ingress_room_responses`, which the
  repository never updates.
- **Decision immutability.** `owner_decision` moves once because the
  repository's update is conditional on the undecided value and the origin's
  settlement is conditional on a decision; no trigger enforces it.
- **Join before message, owner side.** The owner's groupchat commit reads
  `ingress_room_occupants` in its transaction once this node has seated the
  room under the claim it asserts, and only when its plan accepts the
  message. A sender without a row fails the commit with `SenderNotSeated`:
  no decision is recorded, the worker asks the room actor to report its
  roster again and the entry is retried under the ordinary retry budget. A
  sender the actor refuses is answered from the actor's refusal as before.
- **Order of the owner's work.** The owner runs its handler against the room
  actor, which changes actor state and fans presence out, and then commits
  the decision transaction. A failure between the two is retried from the
  lane head: the handler runs again and the first recorded decision and
  response are kept.
- **Who writes occupancy.** The room actor is the writer of membership. It
  records a seat before the join's projection commits and reports the
  difference between its roster and what it last recorded after every
  message that can change occupancy (join, leave, kick, ban, affiliation
  and members-only removals, configuration changes, the ghost sweep's
  leave, a destroy). Each write is one ingress transaction that asserts the
  room claim (`IngressAuthority::record_room_seats`). It is not the
  transaction of the actor's own durable commit: that commit runs on the
  room store's connection, which cannot write an epoch-guarded ingress
  relation. A join whose seat cannot be recorded is refused; a removal
  whose write fails is repeated by the next report. This covers a resource
  connected to the owner itself.
- **What the decision transaction writes.** For a join it completes the row
  the actor recorded (`join_request`, the broadcast presence, role and
  affiliation) and writes nothing when that row is gone or belongs to
  another generation, so an occupant removed between the handler and the
  decision is not seated again.
- **Destroy.** The actor empties the room's rows when it is sealed for a
  destroy, while the claim is still held, and reports its roster again if
  the destroy is undone.
- **Leave at connection end.** The connection's cleanup commits a
  `RoomLeave` request for each remote room the ending connection occupies
  (`commit_room_leave_for_ending_connection` in
  `S/server/routes/websocket/cleanup.rs`), and the owner removes the
  occupancy row when it decides it. A connection whose node is killed
  commits nothing; its row stays until a successor finds its generation
  superseded or the ghost sweep removes the occupant.
- **Relay kinds on a receiver.** A node that carries room requests through
  the mailbox answers `JoinPresence`, `OccupantPresence` and `MujiJingleIq`
  on the ordered relay with the unsupported-envelope refusal, before the
  envelope's sequence is reserved.
- **Seating at claim takeover.** A recovered occupant is seated through a
  join projection (`SeedRecoveredOccupants`), not written into the roster
  directly. The projection commits under the room claim and proves, in the
  room store's transaction, that the row's connection generation is still
  the one `xmpp_occupancy_authority` holds for the full JID. A generation
  is an opaque identifier, so "older" means "not the current one". The
  occupant gets its affiliation from the room's restored affiliation list,
  the remote-occupant flags a join derives from the server's domain, a new
  watermark and a new occupancy order; its recorded role is kept while the
  recorded affiliation still holds. A row that is not seated (stale
  generation, nick held by another account, or a room this actor created)
  is deleted by the report that ends the same actor call, in one ingress
  transaction under the room claim; that is the seeding write, since the
  projection itself commits on the room store's connection. A projection
  that fails for another reason ends the seeding without deleting
  anything, and the room's lanes stay unadopted until a later pass. An
  affiliation the channel resolver granted is not durable and is not
  restored.
- **Source binding.** A request records the authenticated principal
  reference and no stream-management occurrence.
- **Unconsumed decisions.** The mailbox poller counts decided requests no
  origin consumed after the consumption grace on every sweep, publishes the
  count as `ingress.mailbox.room_requests.unconsumed_decisions` and logs
  one line when it is not zero. It consumes nothing.

## Decisions

| # | Question | Decision |
| --- | --- | --- |
| D1 | Where a presence or IQ request is stored | A sibling table, `ingress_room_requests`. `ingress_messages` is not generalised: its digest, alias, source-binding, owner-decision and retention triggers assume a message and stay untouched. |
| D1 | Operation identity | `MailboxOperationKey` gains `RoomJoin`, `RoomPresence`, `RoomLeave` and `RoomIq`, each carrying a `RoomRequestKey` (UUID) minted by the origin before its first owner attempt. The existing `RoomAcceptance(MessageKey)` is unchanged. |
| D1 | How an entry refers to a request | `ingress_mailbox_entries` gains a nullable `request_key` beside a now nullable `message_key`; exactly one is set, and `operation_kind` decides which. Uniqueness is one partial unique index per parent. |
| D2 | Durable occupancy | A new table, `ingress_room_occupants`, one row per occupying resource, written only by the room owner in the transaction that accepts a join, presence update or leave. `xmpp_occupancy_authority` stays what it is: the authority for which connection generation a full JID currently has. |
| D2 | What a successor recovers | Membership, nick, role, affiliation and the last available presence of each occupant. It fails closed for everything held only in actor memory (listed below). |
| D3 | Response custody | The owner freezes the response as ordered rows in `ingress_room_responses`, children of the request, in its acceptance transaction. The origin reads them when it reads the decision, in the same transaction that consumes the entry. No reverse lane exists. |
| D4 | Muji | The same request and response tables on the `MujiSignaling` room lane, which has its own sequence and cursor. |

## D1. Canonical room request

`ingress_room_requests`, PostgreSQL only, under the epoch guard:

| Column | Meaning |
| --- | --- |
| `request_key UUID PRIMARY KEY` | The `RoomRequestKey`; the operation identity. |
| `request_kind SMALLINT` | 1 join, 2 presence update, 3 leave, 4 IQ. |
| `room_jid TEXT` | Bare room JID the request is for. For a Muji IQ this is the room named in `<muji room='…'/>`, not the stanza's `to`. |
| `occupant_jid TEXT` | The requesting resource's full JID. |
| `occupancy_generation TEXT` | The `OccupancySessionGeneration` of the connection that sent it. |
| `source_binding_*` | The authenticated principal reference and, for a stream with stream management, its stream and ordinal: the same four values a bound message carries (`CanonicalSourceBinding`). |
| `stanza BYTEA` | The request stanza, serialised once at this boundary and parsed back into `Stanza::Presence` or `Stanza::Iq` by the repository. Bounded at the frame size limit. |
| `owner_decision SMALLINT` | 0 undecided, 1 accepted, 2 rejected. Moves once, from 0. |
| `settled_at TIMESTAMPTZ NULL` | Set by the origin when it has handed the response to the requester. |
| `created_at TIMESTAMPTZ` | For retention and monitoring. |

Rules:

- **Origin commit.** One ingress transaction through the typed entry locks
  the lane, inserts the request with its source binding and
  `owner_decision = 0`, and appends the entry `Pending`. The handler mints
  the key first and keeps it for every retry of that attempt. A later join of
  the same occupant is a new key.
- **Lane.** Origin = the bound full JID with its occupancy generation
  (`MailboxLaneOrigin::Entity(UserActor, full JID)` extended with the
  generation in a new origin variant, encoding version 1 unchanged for
  existing lanes); recipient = `Room { lane: MucStanza }` for presence and
  `Room { lane: MujiSignaling }` for IQ. The stream-management session is not
  the origin, so a connection's own attempt and a repair after resume append
  to one lane.
- **Join before message.** A groupchat stanza of the same connection must not
  be decided before its join. Groupchat entries of a resumable stream sit on
  the session's lane, which is a different lane, so the order is enforced by
  the owner, not by the cursor: the owner's groupchat commit refuses a sender
  that has no `ingress_room_occupants` row, and the origin does not send a
  groupchat stanza to a room while that connection's join for the room is
  undecided (it waits on the join's decision the way it waits on a room
  acceptance). Merging the two lanes was rejected: a groupchat entry is keyed
  by a canonical message whose first lane is immutable, and a sibling
  resource can resolve the same message.
- **Admission.** The existing per-lane and per-node pending caps apply. A
  refused join is answered with `resource-constraint` and writes nothing.
- **Retention.** A request is not archived. It is collected, with its entry
  and its responses through the cascade, once the entry is `Consumed` and
  `settled_at` is at least one hour old. An undecided or unsettled request is
  never collected. The collector is the ingress maintenance pass; it locks
  the request with `SKIP LOCKED` and never a lane.
- **Dead letters.** As for messages: the entry stays `Pending` at its lane
  head. The origin's wait ends without a decision and the requester gets
  `service-unavailable` from the origin; the request stays for an operator.

Entry table changes in V1032: `message_key` becomes nullable; `request_key
UUID NULL REFERENCES ingress_room_requests ON DELETE CASCADE` is added;
`operation_kind` admits 1 through 5; a check requires `message_key` exactly
for kind 1 and `request_key` exactly for kinds 2 through 5; the unique
constraint on `(message_key, operation_kind)` is replaced by partial unique
indexes on `message_key` and on `request_key`.

## D2. Durable occupancy

`ingress_room_occupants`, PostgreSQL only, under the epoch guard:

| Column | Meaning |
| --- | --- |
| `room_jid TEXT`, `occupant_jid TEXT` | Primary key: one row per occupying resource. |
| `nick TEXT` | Unique per room together with the bare JID rule the room actor applies. |
| `role SMALLINT`, `affiliation SMALLINT` | As the actor's `Occupant` holds them. |
| `occupancy_generation TEXT` | The generation that joined. A request carrying another generation for the same full JID is stale. |
| `occupancy_seat UUID NULL` | Identity of the occupancy, minted by the room actor for every join that starts one (migration V1035). It distinguishes two occupancies of one connection. NULL in a row written before V1035. See `2046-relay-lane-responsibilities.md`, "Seat identity". |
| `join_request UUID REFERENCES ingress_room_requests ON DELETE SET NULL` | The join occurrence that created the row. |
| `presence BYTEA NULL` | The occupant's last available presence as the room broadcasts it, at most 16 KiB; larger presence is stored without its extension payloads. |
| `room_epoch BIGINT` | The room claim epoch under which the row was last written. |

- **Writer.** Only the room owner, in the transaction that latches the
  request's decision, after asserting its room claim: a join inserts or
  replaces the row, a presence update rewrites `presence` (and `nick` on a
  nick change), a leave deletes it. A request whose generation is older than
  the row's is accepted as a no-op and answered with nothing.
- **Reader.** A node that takes a room's claim loads the room's rows before
  it decides any entry for that room and seeds the room actor with them
  (`real_jid`, `nick`, `role`, `affiliation`). Lane adoption for the room
  waits for that load.
- **Fails closed.** Not stored, and therefore not recovered: voice requests
  in flight, pending mediated invitations held by the actor, call and Muji
  session state (#1663), and per-occupant rate-limit state. A successor
  answers work that depends on them as the actor does for unknown state. An
  occupant whose row cannot be parsed is not seated, and its requests are
  refused with `not-acceptable` until it joins again.
- **Stale rows.** A row whose connection is gone is removed by the leave the
  origin commits at connection end, which is a `RoomLeave` request minted
  before the connection's state is dropped. If the origin node dies first,
  the existing ghost-occupant sweep removes the occupant through the owner's
  ordinary leave handling, which deletes the row.

## D3. Response custody

`ingress_room_responses`, PostgreSQL only, under the epoch guard:
`(request_key UUID REFERENCES ingress_room_requests ON DELETE CASCADE,
position INTEGER, stanza BYTEA, PRIMARY KEY (request_key, position))`.

- **Owner.** The acceptance transaction inserts the frozen response in the
  order the requester must receive it. For a join that is the XEP-0045 §7.2
  order the join handler already produces: other occupants' presence,
  self-presence with its status codes, history, subject. For a refusal it is
  the one error stanza. For an IQ it is the result or error followed by any
  server-initiated stanza the handler returns (`session-accept`). The rows
  are written with the decision; a request has responses if and only if it
  is decided.
- **Fan-out to other occupants** is not part of the response. The owner
  executes it after its commit as it does today; it is not replayed on a
  retry of a decided request.
- **Origin.** The connection that sent the request waits for the decision
  like a room acceptance. One transaction locks the lane as origin, verifies
  `owner_decision` against the entry, reads the responses in order, consumes
  the entry and sets `settled_at`. The frames then go to the requester's
  connection in one batch, after any stanza the connection had already
  queued. With stream management they enter the session's unacked queue
  through the ordinary outbound path; without it, a connection that ends
  before the write loses them, which is the documented attempted-uncertain
  outcome.
- **Exactly once.** The consuming transaction is the single hand-over. A
  second reader finds the entry consumed and delivers nothing. A requester
  whose connection ended before the hand-over finds, after resume, an entry
  that is decided and not consumed, and consumes it then; a new connection
  generation does not consume it, and the retention rule collects it only
  after an operator-visible age (it is reported by the unconsumed-decision
  sweep).
- **Why no reverse lane.** A response has exactly one reader, the origin of
  the request, which already waits on that request's entry. A lane in the
  opposite direction would add a second cursor and a second poller for work
  that has no ordering relation to anything but its own request.

## D4. Muji signalling

A relayed Muji `session-initiate` or `session-terminate` is a `RoomIq`
request on the `MujiSignaling` lane of its room. The owner runs the existing
relayed executor, which checks membership against the room actor (now
seeded from `ingress_room_occupants` after a transfer), and freezes the IQ
result and the `session-accept` as responses. Signalling order is the lane's
sequence. Call state is not made durable here.

## Guard, roles, pins and wire

- All three tables join `EPOCH_GUARDED_TABLES`, `ingress_epoch_guard_manifest`
  and the restricted-ingress manifest, with DML and truncate guards enabled
  `ALWAYS`; the guarded-table count goes from 15 to 18. Runtime grants:
  `SELECT, INSERT, UPDATE` on requests and responses, `SELECT, INSERT,
  UPDATE, DELETE` on occupants.
- The definer-API candidate gets its guard triggers and no function, so the
  repositories refuse with `DefinerApiUnsupported` there, as the mailbox does.
- SQLite records V1032 and creates nothing.
- `deliver_ordered` goes to `v15` when the join and presence senders are
  removed for mailbox transactions and to `v16` when the Muji sender is. The
  kinds stay in the enum while the relay serves epoch below 2, so each bump
  marks that a peer at the new id no longer sends those kinds at the mailbox
  epoch; a peer at the old id answers `UnknownMessage` before admission.

## Build order

1. V1032, the three repositories, `MailboxOperationKey` variants and the
   lane origin variant, with a PostgreSQL repository suite. Dark.
2. Owner: decide a room request (join, presence, leave) with occupancy and
   frozen responses in one transaction; worker dispatch by operation kind;
   seeding at claim takeover.
3. Origin: mint, commit, wait, consume and deliver; join gate for groupchat;
   leave at connection end. `v15`.
4. Muji through the same path. `v16`.
