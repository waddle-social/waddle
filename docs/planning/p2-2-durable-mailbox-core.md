# P2.2 — durable mailbox core

Status: **active from protocol epoch 2**.
Migration V1031 creates the lane and entry tables and `MailboxRepository` is
the only code that writes them. On a clustered PostgreSQL deployment the
ingress commit appends and decides entries in every transaction whose
protocol epoch is 2 or later, unless
`WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE=relay` is set. The sections from
"Ownership and boundaries" onward are the design review that preceded the
implementation and are kept for their failure analysis. Where they describe a
question as open, the "Implemented contract" section below is the answer in
force.

## Implemented contract

### Decisions

| Question | Decision |
| --- | --- |
| Tables | Two: `ingress_mailbox_lanes` and `ingress_mailbox_entries`. An entry refers to its canonical `ingress_messages` row and stores no payload; acceptance evidence stays in `ingress_messages.owner_decision`, the sealed manifest and `ingress_owner_rejections`. |
| Operation identity | `MailboxOperationKey::RoomAcceptance(MessageKey)`, unique across every lane (`UNIQUE (message_key, operation_kind)`). `DispatchToRoomRemote` and delivery keys name one owner-specific attempt and are not identity. |
| Lane key | The stable logical lane `(origin, recipient, room lane)` from `waddle_xmpp::ingress::mailbox`. The row stores the full canonical encoding and indexes its versioned digest; a lookup that finds a row with the same digest and another encoding fails closed. |
| Fences | `origin_epoch`, `target_epoch`, `target_node_id` and `target_node_epoch` are columns beside the identity. A caller presents the fence it has proven against its claims in the same transaction. An equal fence is current, a fence whose epochs are nowhere smaller and somewhere larger replaces the stored one, and anything else is stale. |
| Sequence | `last_sequence` on the lane row, read under `FOR UPDATE` and incremented with the entry insert. Gap-free in committed history, `NUMERIC(20)`, 1 through `u64::MAX`, with a typed exhaustion error. |
| Cursor | `last_dispositioned` on the lane row: the last sequence the lane's target has dispositioned, exclusive. It moves only to its immediate successor, in the target's transaction. |
| Dispositions | `Pending → OwnerAccepted \| OwnerRejected → Consumed`. The owner's decision moves the cursor; consumption by the origin does not. A terminal entry keeps which decision it was. |
| Admission | Per-lane and per-node pending caps, checked after the lane lock and before the counter moves. A refused append writes nothing. |
| Retention | An entry is removed only by the cascade from its canonical message. |
| Backends | PostgreSQL only. SQLite records V1031 and creates nothing; a single-node deployment seals in one transaction and writes no lane. |
| Dead letters | A dead-lettered entry stays `Pending` at its lane head. The cursor never passes it without an operator repair. |

### Lock order

Inside one transaction: lane row, then canonical message, then entry.
`MailboxRepository::lock_lane` (origin, by identity),
`lock_lane_for_target`, `lock_lane_for_origin` and `claim_backlogged_lane`
(by lane id) take the lane and return a `LockedMailboxLane`; `append`,
`decide`, `consume` and `read_after_cursor` require it and check that it was
locked by the same transaction. `append` locks the canonical message before
it looks the operation up, so two appenders that hold different lanes
serialize on the message and at most one allocates.

Message garbage collection locks the canonical message with `SKIP LOCKED` and
reaches entries only through the cascade. It never locks a lane, so it cannot
wait on a lane held by an appender that is waiting for that message.

### Retention

An entry that is not `Consumed` retains its canonical message. On PostgreSQL
the retention clock (`refresh_retention`), the collector's candidate scan and
its final delete all treat such an entry like an SM reference
(`retained_child_unions` in `ingress_substrate/substrate.rs`), and each reads
it in a fresh READ COMMITTED statement snapshot. The deployed CNPG monitoring
query carries the same clause, and `gc_monitoring_predicate_matches_collector`
keeps the two identical. A consumed entry retains nothing and leaves with its
message through the cascade. Lane rows are not collected.

Lock schedules:

- Appender first: the appender holds its lane and the canonical message. The
  collector's `SKIP LOCKED` passes over the message and finishes.
- Collector first: the collector holds the canonical message. The appender
  holds its lane and waits for the message. The collector needs no lane, so it
  either skips the message, after which the appender proceeds, or deletes it,
  after which the appender fails with `MailboxError::ParentMissing` and rolls
  its lane back.

The unregistered GC pilot (`ingress_gc_fk_transition_v1_candidate.sql`,
`ingress_gc_api_v1_candidate.sql`) lists the entry foreign key in its FK
manifest, turns it into `NO ACTION`, refuses a message whose entry is not
consumed and deletes consumed entries explicitly.

### Remote room acceptance

`RemoteRoomAcceptance` (`ingress_uow`) selects how a groupchat message for a
room owned by another node reaches the owner's acceptance. `DurableMailbox`
is the default and is set from `WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE`
(`mailbox` or `relay`). It can take effect only with clustered fencing on
PostgreSQL (`mailbox_acceptance_enabled`), and it does take effect in a
transaction whose protocol epoch is at least `mailbox_acceptance_epoch()`,
which is 2 (`mailbox_acceptance_active`). `OrderedRelay` leaves the relay
path as it is at every epoch.

In a transaction that uses the mailbox (`ingress/commit_mailbox.rs`):

- **Origin commit.** After its stream row and before any canonical row, the
  origin locks its lane: origin = the stream-management session (or the bound
  resource for a stream without one), recipient = the room's `MucStanza` lane.
  The fence is the stream's claim epoch and the room claim read from
  `clustering_claims` in the same transaction. The canonical message is
  created with its authenticated source binding, because only a bound message
  can carry an owner decision and the binding is immutable. Once the
  `DispatchToRoomRemote` intent is recorded, the same transaction sets
  `owner_decision = 0` and appends the entry `Pending`. A message that already
  exists without a source binding keeps the relay's behaviour and gets no
  entry.
- **Transport.** A commit that appends or finds the entry drops the relayed
  dispatch from its external effects (`without_relayed_dispatch`), so the
  origin sends the owner no stanza. The entry is the transport: the owner
  reads the request from the canonical rows, woken by the hint or found by
  its poller. The recorded `DispatchToRoomRemote` intent stays and is
  receipted by the owner's first decision.
- **Admission.** Under the lane lock, a lane at 64 or an owner node at 16 384
  pending entries refuses the stanza: the commit records an ordinary denial
  with a `resource-constraint` error of type `wait` and appends nothing. A
  stanza already bound to its wire position is never refused for a backlog it
  is part of.
- **Owner commit.** The owner's relayed commit looks the entry up by
  `RoomAcceptance(MessageKey)`, locks its lane as target with the room claim
  it has just asserted, and only then locks the canonical message. Clustered
  rooms seal their manifest in this mode. Before commit the same transaction
  latches `owner_decision` to accepted, or stores the frozen error as a
  rejection child, maps it in `ingress_owner_rejections` and latches rejected;
  then it moves the entry to `OwnerAccepted` or `OwnerRejected` and the lane
  cursor past it. The owner's first decision may refuse a message even though
  the origin's row already records its dispatch. On a replay the latched
  decision is the authority, whatever the replayed plan concluded.

No second acceptance record exists: the entry's disposition and
`owner_decision` are written together and the manifest or the rejection child
is the evidence.

### Origin handled count

The origin never counts a stanza as handled on the strength of its own commit
while its room owner is undecided (`ingress/origin_gate.rs`):

- **Commit.** When the entry is still `Pending`, the origin's commit binds the
  stanza to its wire position but persists the handled checkpoint of the
  prefix before it, and the decision carries a `PendingRoomAcceptance`. A
  retransmission that finds the owner's decision already recorded consumes
  the entry in its own commit and is counted normally.
- **Connection.** `frame.rs` leaves the stanza's reserved inbound slot open
  (the slot mechanism deferred ordered-relay hand-offs already use) and calls
  `IngressAuthority::watch_room_acceptance`. `h` and every `<a/>` therefore
  report only the proven prefix, while `<r/>` and later stanzas are still
  processed. Later stanzas complete their own slots but cannot move `h` past
  the open one.
- **No free slot.** A connection holds at most 256 deferred slots, each
  reserved when its stanza is routed, and a held slot keeps its place in the
  completion queue until the owner's decision settles it. When no slot can be
  reserved, or the queue is closed, the stanza is still committed and its
  entry appended; the handler then waits for the owner's decision itself
  (`await_room_acceptance_inline`), so the connection reads no further frame
  and `h` cannot cross the stanza. A proven decision counts it; anything else
  leaves it an unhandled hole. The stanza is not refused, because the
  refusal would have to precede a commit that alone knows whether the room
  is remote.
- **Authoritative lookup.** The watcher polls `resolve_room_acceptance`: it
  reads the entry, and for a decided entry locks the lane as origin and the
  canonical message, requires `owner_decision` to record the same decision
  (and a refusal to have its rejection child), consumes the entry and
  commits. The same read serves a lost relay reply and an indeterminate owner
  commit; a relay reply, a relay failure and a timeout are never evidence.
- **Refusal.** When the decision read is a refusal, the origin loads the
  owner's frozen error from the rejection child, receipts the source's
  `ErrorReply` obligation in the same transaction and answers the sender
  with it: in the slot's completion, in the handler's own response after an
  inline wait, or on the sender's local connection for a stream without
  stream management. No relay reply carries the error.
- **Settlement.** A read decision completes the slot and `h` advances; the
  existing checkpoint flush then persists it. If no decision can be proven
  within 60 seconds, or the entry is missing, dead-lettered, contradicted by
  the canonical message or fenced by a later stream claim, the watcher
  withholds the slot: the tracker turns it into a permanent hole, the stanza
  is never acknowledged on that connection and the sender keeps
  responsibility for it. A resumed or new session retransmits it and resolves
  to the same canonical message and entry.

A stream without stream management has no handled count; its watcher only
consumes the decision. `ingress.mailbox.room_acceptances{outcome}` counts how
waits end.

Separate registered and handled frontiers are not built: the single
checkpoint excludes the one open stanza, and the tracker's contiguity rule
keeps `h` behind it.

### Hints, poller and dead letters

- **Poller** (`clustering/route_bridge/mailbox.rs`). Each clustered node runs
  one. It wakes on a hint or every second, scans at most 128 lanes that record
  this node as target and still owe a disposition
  (`ingress_mailbox_lanes_backlog`), and works them two at a time. A worker
  stays on its lane while each head entry is decided, up to 64 entries (the
  lane's pending cap), so a burst from one sender behind one coalesced hint
  is decided in one visit and in lane order. The poller is the authority for
  finding work; it idles while the relay selector is in force.
- **Work** (`ingress/mailbox_worker.rs`). A worker claims a lane with
  `SKIP LOCKED`, reads its head entry and rebuilds the request from canonical
  rows: the message from the canonical envelope, the sender's principal from
  the source binding, the origin id from the alias and the stanza language
  from the entry's `request_language`, which the canonical message does not
  retain. It then runs the owner's ordinary relayed-groupchat commit, which
  asserts the room claim and records the decision. Claiming a lane proves
  nothing about the room. Frames the owner produces for the sender are not
  written by the worker; their receipts stay pending. Right after a decided
  commit the worker runs the ingress recovery of that one message
  (`deliver_owner_obligations`), which rebuilds the frozen copies still owed
  and routes each to the node that hosts its recipient. What that attempt
  cannot deliver is retried by the maintenance pass.
- **Hint** (`waddle.clustering.relay.mailbox_hint.v1`). After a commit that
  leaves a stanza awaiting its owner, the origin sends the node recorded as
  the lane's target a message with no payload. Its only effect is to wake that
  node's poller. It is a relay message of its own, so `deliver_ordered` is not
  touched; a peer without it answers with an unknown-message error, which the
  sender ignores.
- **Retry.** A failed attempt is counted on the entry and the next attempt is
  deferred by 250 ms doubling to 30 s. After eight failures, or when the
  canonical evidence cannot be read, the entry is dead-lettered: it stays
  `Pending` at its lane head and the cursor does not move, so nothing behind
  it on that lane is decided. Only `repair_mailbox_dead_letter`, an operator
  action, returns it to work.
- **Telemetry.** `ingress.mailbox.deliveries{outcome}` (`decided`, `retried`,
  `dead_lettered`) and `ingress.mailbox.room_acceptances{outcome}` are
  registered at zero. `ingress.mailbox.hints{outcome}` is context only.

### Cutover

The cutover from relay acceptance to mailbox acceptance is the protocol
epoch, and it is coordinated, not rolling (D1 in
[`2046-completion-contracts.md`](2046-completion-contracts.md)).

- **Activation.** `supported_protocol_epoch()` is 2. With every server
  stopped, the owner phase (`prepare_restricted_runtime_owner`) advances
  `ingress_protocol_epoch` one step per transaction until it reaches 2,
  stamping each step with the deployment's lineage
  (`activate_ingress_protocol_epoch`). A binary that implements only epoch 1
  then fails every ingress transaction it opens with
  `EpochUnsupported`, inside that transaction, and a guarded write that was
  blocked behind the activation fails its epoch proof when it continues.
- **Per-transaction boundary.** Every ingress transaction reads the live
  epoch under the epoch row lock before it writes. A commit below epoch 2
  dispatches to the room owner through the ordered relay, records no source
  binding for that purpose, writes no lane and no entry, seals no clustered
  manifest and counts the stanza on the origin's commit. A commit at epoch 2
  or later binds the source, opens the owner decision and appends the entry.
  Because the activation runs while no server runs, the two kinds never
  interleave on one stream.
- **Relay-era rows.** A `DispatchToRoomRemote` intent recorded below epoch 2
  belongs to a message without a source binding, and the schema allows an
  owner decision only on a bound message. Such a message was already
  acknowledged to its sender. It is never given a mailbox entry or a
  decision, including when its stanza is retransmitted after the activation
  (`record_origin` returns no entry for an unbound message). Its unreceipted
  dispatch stays with the ingress maintenance pass, exactly as before the
  activation. Nothing is adopted from one era into the other.
- **Startup refusal.** A node that selects the mailbox on clustered
  PostgreSQL checks the mailbox rows before it serves
  (`MailboxRepository::verify_cutover_state`, from
  `IngressAuthority::finish_startup`). It refuses to start, with
  `MailboxError::Cutover`, when an entry belongs to a message without a
  source binding or without an owner decision, or when the live epoch is
  below 2 and an entry is still pending or unconsumed: no commit at that
  epoch could ever finish it. It does not repair either state.
- **Deployments without the owner phase.** A PostgreSQL deployment that does
  not run the restricted-runtime owner phase stays at epoch 0. It keeps the
  relay's behaviour whatever the selector says, and nothing fences an older
  binary there.
- **Selector.** `ingress.remoteRoomAcceptance` in the Helm chart sets
  `WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE`. `relay` keeps relay acceptance at
  every epoch and must be set on every node or none; it exists for the
  lanes that still depend on the ordered relay and is removed with them.

### Owner transfer and unread decisions

- **Lane adoption.** A lane records the owner incarnation its origin last
  proved, and a node's backlog scan reads only lanes recorded for itself.
  Every fifth poll pass a node also reads up to 128 backlogged lanes recorded
  for another incarnation (`scan_foreign_backlog`), decodes each lane's room
  and looks its claim up in `clustering_claims`. For a room the claims table
  assigns to this node, a second transaction reads the claim again, locks the
  lane and writes the claim onto it (`adopt_backlogged_lane`), provided the
  claim epoch is later than the one on file. The lane then appears in this
  node's backlog scan and its head entry is worked like any other. Adoption
  decides nothing: the owner's commit asserts the room claim again. A lane
  whose room has no claim, or whose claim is held elsewhere, is left alone,
  so its entry stays pending until some node owns the room. A restarted owner
  is a new incarnation and adopts its own earlier lanes the same way, once it
  holds the room's claim again. The origin still moves the lane itself when
  it commits the same stanza again under the new claim.
- **Occupancy after a transfer.** A room's occupants live in the room actor's
  memory. A successor that does not know the sender as an occupant refuses
  the stanza through the ordinary groupchat checks; that refusal is the
  owner's decision and is recorded like any other. No acceptance is derived
  from the predecessor's state.
- **Unread decisions.** Every thirtieth poll pass a node reads up to 128
  decided entries appended at least ten minutes ago
  (`scan_unconsumed_decisions`) and consumes each one whose canonical message
  proves the same decision, under the lane and canonical locks
  (`sweep_unconsumed_decisions`). The origin's watcher gives up after 60
  seconds, so a live origin has finished with an entry long before. The
  terminal entry keeps which decision it was until its message is collected,
  so a retransmission that arrives after the sweep still reads the decision
  and is counted. An entry whose canonical message does not prove its
  decision is left in place and logged; it keeps retaining its message.

### Remote copies of a frozen manifest

The owner's commit seals the room manifest: the audience, the child delivery
keys and the sender's reflection. Delivering those copies does not depend on
the mailbox entry or on the node that accepted the message:

- The owner's live execution relays each copy to the node that hosts the
  occupant and records a receipt for each one that lands.
- A copy whose relay failed, whose reply was lost, or which was never
  attempted because the owner stopped, leaves its route receipt pending. The
  ingress maintenance pass of every node reads unreceipted rows from the
  shared database and rebuilds exactly the frozen copies
  (`ingress/recovery_muc.rs`): occupants named by the manifest and not yet
  completed, never the current roster. A rebuilt copy for an occupant this
  node does not host goes through the detached-delivery effect
  (`QueueDetached`) addressed to the frozen resource. It stays pending, and a
  later pass retries it, while the node that owns the occupant cannot be
  reached.
- Because the rebuild reads only canonical rows, it gives the same result
  after a restart and after the room's claim has moved. An occupant who
  joined after the acceptance is not in the manifest and receives no live
  copy.

The poller does not write the frames the owner produces for the sender;
their receipts stay pending and the same recovery delivers them.

### Limits of the current implementation

These hold today and bound what the mailbox selector may be used for.

- **Relay kinds.** The origin relays no groupchat stanza in a transaction
  that uses the mailbox. The `GroupchatMessage` kind is still sent below
  epoch 2 and under the `relay` selector, so no ordered-relay kind is removed
  and its wire id is unchanged.
- **Retransmission of a decided stanza.** A retransmission whose entry is
  already decided is counted as handled and gets no answer from the origin:
  a refusal is not repeated, and the reflection of an acceptance is only
  what the owner's obligation delivers.
- **Latency of the sender's reflection.** The reflection is delivered by the
  owner's obligation repair, not in the origin's response. If that repair
  fails, the next attempt is the maintenance pass, at least a minute later.
- **Lane rows.** Lane rows are never deleted. A lane exists per
  stream-management session and room, so the table grows with sessions.
- **Other lanes.** Room requests (joins, occupant presence, Muji
  signalling) and direct requests (bare-JID and full-JID delivery) are
  further operations with their own parent tables; see
  [`2174-room-request-lanes.md`](2174-room-request-lanes.md),
  [`2173-direct-lanes.md`](2173-direct-lanes.md) and
  [`2046-relay-lane-responsibilities.md`](2046-relay-lane-responsibilities.md).

### Guard, roles and manifest

Both tables are in `EPOCH_GUARDED_TABLES` and in
`ingress_epoch_guard_manifest`, with the statement-level DML guard and the
truncate guard enabled `ALWAYS`. The owner phase grants the runtime `SELECT,
INSERT, UPDATE` on both (`TABLE_GRANTS` in `server/runtime_owner.rs`): rows
are inserted, locked and updated, and deleted only by the cascade, which runs
as the table owner. The restricted-ingress manifest pins their structure and
triggers. The definer-API candidate installs its guard trigger on both and
exposes no mailbox function, so the repository refuses with
`MailboxError::DefinerApiUnsupported` on a transaction that uses that
candidate.

### Tests

`crates/waddle-server/tests/mailbox_core_postgres.rs` covers, against real
PostgreSQL: a blocked second appender behind a held lane lock, unique
contiguous sequences under concurrent appenders, no gap after a rollback, the
full unsigned range and exhaustion, one entry for one operation across lanes
(sequential and raced), a replan from owner A to owner B that returns the
first entry and fences A out, head-only owner decisions with an exclusive
cursor, admission refusal before any write, and refusal of unproven,
stale-proof and unsupported-epoch writes. It also runs the real collector
against a pending, a decided and a consumed entry, and drives both lock
schedules with an observed blocked backend. The binary runs in nextest's
`postgres` group and its tests are in `required-cluster-tests.txt`.
`tests/ingress_relation_guard_lab/gc_pilot.rs` covers the same retention rule
in the GC pilot.

## Ownership and boundaries

P2.2 owns the dark, durable ordered-lane core: typed lane and sequence identity,
transactional allocation, canonical work references, a closed disposition
model, bounded reads, exact fenced updates, and integration with canonical
ingress retention. It has no production writer. An append is local database
work and never waits on the network inside its transaction.

P2.3 ([#2118](https://github.com/waddle-social/waddle/issues/2118)) owns the
distinct acceptance and target-consumption units of work, zero-payload hints,
target-first polling and reconciliation. Its accepted baseline is a bounded
scan of 128, claim of 2 and two workers; its last-dispositioned cursor is
exclusive. Hints, readiness, relay replies and timeouts confer no authority.
P2.4 owns lane layout, `is_lane_head`, bounded workers and admission, retry,
operator repair and the execution policy for dead letters. Each existing relay
consumer cuts over separately. Endpoint schema belongs to P3.1.

[#1948](https://github.com/waddle-social/waddle/issues/1948) owns remote room
acceptance before origin XEP-0198 `h`; it consumes this substrate. It must
neither create a second acceptance ledger nor treat delivery as a prerequisite
for acceptance. Clustered manifest sealing, epoch activation and production
admission retain their separate cutover gates, including #1689. No change to an
ordered-relay wire ID is part of P2.2. #1689 and epoch activation are required
for live lanes, not for dark DDL once #2116's prerequisite is met.

Current ordered relay is in memory: its channel includes origin, recipient,
origin epoch and target epoch. It separates MUC stanzas from Muji signaling
([`ordered_relay.rs`](../../server/crates/waddle-server/src/clustering/ordered_relay.rs)).
Canonical messages, effects and receipts already exist in ingress; the mailbox
references them rather than storing stanza bytes. MUC presence and IQ need
special care: the current relay can carry a stanza with no canonical ingress
reference, including join repair with `canonical: None`
([`proxy.rs`](../../server/crates/waddle-server/src/clustering/route_bridge/delivery/muc/proxy.rs),
[`remote_route_helpers.rs`](../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_route_helpers.rs)).
A message-reference-only core does not make those lanes ready to cut over. Each
affected cutover must first identify existing canonical custody or review a
single authoritative way to establish it; do not introduce a parallel payload
or acceptance ledger to fill the gap.

## Five decisions for the architecture gate

The following choices are a proposed starting point, not a claim that the
questions have already been adjudicated.

1. **Lane key and owner epochs.** Use a stable logical lane with claim epochs
   as fences, not identity. Start from `(logical origin, recipient, room lane
   where applicable)`: dropping the origin would merge channels that are
   independently ordered today. Keep bare JID, full JID and room recipients
   distinct. Persist the exact normalized identity and validate it on lookup.
   Current owner claim, node incarnation and any relevant lifecycle generation
   are validated at the mutation boundary; the successor resumes the same lane
   and sequence space. A stale owner cannot accept, consume, discard or repair
   successor work. The review must decide the durable origin identity across
   SM resume and the current join-repair path, which can change origins; it must
   prove required ordering across each actual handoff, not assume that simply
   removing epoch fields preserves it.
2. **Allocator.** Use one transactional per-lane counter. Lock the lane before
   dependent claims and rows, check the exact existing operation before
   incrementing, then increment and insert in the same transaction. A rollback
   rolls back both; a committed duplicate returns the existing sequence rather
   than allocating another while the entry is retained. After authorized row
   cleanup, a duplicate must read the exact retained canonical outcome without
   allocating again; any required original sequence must survive in that
   canonical proof. Proposed range: zero for an empty counter and
   positive, nonwrapping `u64` sequences through `18446744073709551615`, with
   an explicit exhaustion error. PostgreSQL storage must represent and check
   that entire range; do not narrow it silently to signed `BIGINT`. Confirm
   that gap-free committed allocations are worth the lane lock and specify the
   treatment of deleted entries and cursors before implementation.
3. **Reference granularity.** For message work, propose that each entry refers
   to a canonical `MessageKey` and an exact effect or delivery reference. A
   client join instead needs a distinct closed typed canonical request variant
   in the same ingress authority; its key and storage contract remain blocked
   by question 2. Child work refers to the owner's frozen
   delivery keys, never an audience rediscovered during consumption. The
   existing remote dispatch effect is an owner-specific reference, however,
   and cannot alone supply the stable acceptance-request identity needed on
   owner transfer; see blocking question 1. Require message/reference/target
   agreement; a delivery key alone is not proof that it belongs to the supplied
   message. Decide the exact request and child variants against #1948 before
   coding them.
4. **Single-node behavior.** The supported single-node backend and whether it
   ever writes a durable mailbox remain an owner decision. A PostgreSQL dark
   core is the initial proposal; it does not decide SQLite feature support or
   change existing single-node behavior. Resolve the backend policy before
   schema or runtime work.
5. **Room lanes.** Preserve separate `MucStanza` and `MujiSignaling` lanes.
   MUC presence and messages share ordering; Muji signaling orders against
   itself and must not stall unrelated MUC traffic. This preserves the current
   separation, but the missing canonical reference for some presence/IQ/Muji
   traffic still blocks their individual cutovers.

## Blocking contract questions before schema or repositories

1. **BLOCKING — stable remote-room request identity.** What canonical, pre-commit request
   identity survives a room-owner transfer, and how does it relate to the
   existing owner-specific dispatch effects without creating a second
   acceptance ledger? Today `DispatchToRoomRemote`'s semantic key includes
   `RelayTargetIdentity` ([`effect_intent.rs`](../../server/crates/waddle-xmpp/src/ingress/effect_intent.rs#L1399)),
   which carries node ID and optional node incarnation; remote-room planning
   supplies both from the current owner claim
   ([`room_dispatch.rs`](../../server/crates/waddle-server/src/server/routes/interpret/room_dispatch.rs#L1226)).
   The effect's storage identity includes that target and `DeliveryKey::effect`
   derives from the effect key ([`effect_intent.rs`](../../server/crates/waddle-xmpp/src/ingress/effect_intent.rs#L1474),
   [`keys.rs`](../../server/crates/waddle-xmpp/src/ingress/keys.rs#L49)).
   A fresh plan for a different owner can therefore produce a different effect
   and delivery key for the same submission; indexing that key would not find
   the earlier acceptance. Before choosing schema or uniqueness constraints,
   review the identity's canonical inputs and scope, its immutable logical
   target and content checks, and how an owner-specific dispatch references the
   same request. Prove that a lost reply followed by a claim transfer recovers
   the original accepted manifest and children, while distinct requests remain
   distinct. A digest of the owner-dependent effect key does not solve this.
2. **BLOCKING — origin-changing repair and canonical custody.** What lane, handoff and
   exclusive-cursor contract preserves MUC ordering when the logical origin
   changes, and what canonical work reference exists for the repair? The
   current channel includes its origin
   ([`ordered_relay.rs`](../../server/crates/waddle-server/src/clustering/ordered_relay.rs#L82));
   a normal source can use `SmSession`, but join repair explicitly creates a
   `UserActor` entity origin and sends `canonical: None`
   ([`remote_route_helpers.rs`](../../server/crates/waddle-server/src/clustering/route_bridge/delivery/remote_route_helpers.rs#L7),
   [`proxy.rs`](../../server/crates/waddle-server/src/clustering/route_bridge/delivery/muc/proxy.rs#L337)).
   The proposed origin-bearing key would produce different lanes for these
   origins. Before schema selection, decide how source and repair are recognized
   as the same logical responsibility, whether they share or hand off ordering
   and cursor state, and how durable canonical custody is established without
   inventing a parallel payload or acceptance authority. Preserve original
   join-before-message ordering through a maybe-committed repair and restart.
3. **BLOCKING — acceptance-proof lifetime and cleanup lock order.** What durable event
   proves the origin can no longer need an acceptance lookup, and how are the
   entry, canonical evidence and parent reclaimed without lock inversion? The
   current collector locks and rechecks the canonical message and uses a fresh
   READ COMMITTED view
   ([`substrate.rs`](../../server/crates/waddle-server/src/ingress_substrate/substrate.rs#L940));
   its existing retention frontier tracks SM references, effects and descendants
   ([`retention.rs`](../../server/crates/waddle-server/src/ingress_substrate/retention.rs#L25)).
   Neither currently accounts for mailbox acceptance. Define the origin's
   recoverable checkpoint or other authoritative release condition, proof
   lifetime through outages and owner changes, and safe removal of terminal
   entries before the full parent retention tail begins. Neither a lost reply,
   relay ACK, elapsed time alone, nor child consumption proves the origin has
   recovered acceptance. Specify lane-first cleanup versus parent-first GC,
   foreign-key behavior, bounded retry and both lock schedules; a collector
   holding the parent cannot wait on a lane held by an appender waiting for
   that parent. No candidate cleanup algorithm is accepted by this document.

These questions require an independent architecture decision and integrated
failure evidence, not merely adding tests to the matrix. The positive-only
lookup, per-lane transactional counter and separate MUC/Muji lanes remain
proposals subject to that review and the #2116 prerequisite.

### Adversarial review findings, 2026-11-02

These findings narrow the design but **do not clear** the three blocking
questions or the #2116 prerequisite:

- A candidate remote-room request identity is `(MessageKey,
  RemoteRoomAcceptance)`, unique across lanes. The room, original sender,
  semantic digest and typed source authority are immutable checked request
  data. `DispatchToRoomRemote` and its derived `DeliveryKey` include the
  current owner and are dispatch provenance, not stable request identity.
  Recovery must also recognize an originally **local** room result after a
  local-to-remote owner change; no remote dispatch effect exists in that case.
  A missing origin ID requires an independently verified canonical source
  binding, since a supplied message key and digest alone do not authenticate
  its sender.
- Existing origin-ID aliases intentionally deduplicate by sender **bare** JID,
  target and origin ID. A replacement connection or sibling resource can
  therefore resolve the same `MessageKey` from a different physical lane.
  Preserve the first lane and sequence. A separately authenticated retry may
  look up or reconcile the original outcome from the retained row or, after
  authorized cleanup, its exact retained canonical proof. It must not allocate
  or execute a second request or borrow the new session's authority for old work. The
  original response stays bound to its exact session/endpoint custody, even
  if a replacement reuses the full JID. A replacement or sibling receives only
  a separately authorized, occurrence-specific response; successful SM resume
  preserves the original logical session under its own fence. If the old
  request is pending, the new source frontier must remain behind it or use a
  reviewed durable
  dependency barrier. A response owed to a new stanza occurrence needs its
  own recipient, stanza ID and recoverable response custody.
- A proposed client MUC lane origin is the bound full JID plus
  `OccupancySessionGeneration`, carried through SM resume and remote-resource
  forwarding. `SmSession` and `UserActor` are execution authorities, not
  different origins for repair. A join needs a new typed canonical request in
  the existing ingress authority: the current join sends `canonical: None`
  and `MessageEnvelope` holds only messages. Mint a distinct occurrence key
  before each first owner attempt; repair reuses it, while a later identical
  XEP-0045 join remains a new synchronization request. Source registration
  must preserve arrival order before lane-lock acquisition. Both local and
  remote room execution must honor the same pending lane head after a claim
  transfer.
- A committed room projection records a lifecycle revision but leaves
  occupancy in actor memory. A successor cannot infer a joined occupant from
  the projection or a cursor past the join. Recover the exact current
  occupancy before dependent work, or fail closed. Composing the owner result
  with mailbox disposition also requires changing the room store's current
  independent transaction boundary. No schema or lane cutover should assume
  these contracts already exist.
- The proposed lane-before-parent order is not today's ingress order: the
  current commit locks the SM stream before canonical resolution. Restructure
  that enclosing transaction or prove another whole-system order. A candidate
  GC schedule has lane-first append/terminal cleanup, while GC locks only the
  canonical parent with `SKIP LOCKED` and never a lane. Under a fresh READ
  COMMITTED snapshot, GC must check for **any** mailbox row, including a
  terminal row being deleted concurrently, before deleting deliveries or the
  parent. Otherwise the immediate foreign-key check can deadlock with a
  lane-first cleaner waiting on the parent. The release proof still needs a
  durable origin handled ordinal/frontier and retained checkpoint or
  tombstone across P3.3 recovery; elapsed time, relay replies and child
  consumption do not supply it. Deleting the mailbox row must not make the
  accepted or rejected owner outcome undiscoverable during the remaining
  canonical alias-retention tail. For manifest-bearing messages, an accepted
  result may be read back from the exact immutable canonical room manifest;
  a rejected result instead needs an exact canonical owner-rejection proof
  and frozen error. These are mutually exclusive, and missing or mismatched
  evidence fails closed. The current rejection detector does not recognize a
  remote denial alongside an already recorded remote-dispatch intent, so
  P2.2/P2.3 must extend that reconciliation before relying on rejection
  readback after row cleanup. Other mailbox variants cannot borrow this
  proof; retain their row until an equivalent result and release rule is
  reviewed. The current eight-day alias
  tail is a bounded deduplication horizon, not proof against every later
  fresh-session client resubmission.

### Current-tree contract review, 2026-11-03

A read-only review of the local P2.1 draft at `978c9085` found canonical
`source_binding`, `owner_decision`, `ingress_owner_rejections`, and paired
retention checks that were not present in this document's `main@8ecc94bb`
snapshot. The closed owner-result and readback helpers are still test-only.
These newer pieces narrow the work but do not clear the three blocking contract
questions or #2116. No mailbox schema, repository, runtime path, or test was
added by this review.

1. **Request identity remains blocked.** A candidate operation identity is
   `RoomAcceptance(MessageKey)`, globally unique across lanes and physical
   owners. The original source binding, room, sender, digest, occurrence, and
   first lane/sequence must be checked and retained; an owner-specific
   `DispatchToRoomRemote` key is attempt provenance. The current relayed-source
   assertion requires that remote-dispatch effect, so it cannot recover an
   originally local accepted result after a local-to-remote owner transfer.
   A successor must first read the exact accepted manifest or rejection before
   deciding whether any new execution is permitted. A duplicate from another
   resource may reconcile the original responsibility, but it cannot move the
   lane or inherit the original response custody.
2. **Join and source order remain blocked.** Both normal join dispatch and
   repair need one stable logical origin, proposed as bound full JID plus
   `OccupancySessionGeneration`, and a server-minted canonical join occurrence
   in the existing ingress authority. Current repair changes execution origin
   and sends `canonical: None`. More fundamentally, the current stream lock
   allocates from `handled + 1`; delaying `h` while admitting a second pending
   stanza can reuse an ordinal. Select either separate durable registered and
   handled frontiers or a durable one-pending-occurrence admission gate with
   bounded buffering and continued SM-control handling. Accepted occupancy
   must also be recoverable and superseded by later leave/kick/rebind work;
   replaying an old join must not resurrect it.
3. **Proof release remains blocked.** A positive release certificate must bind
   the operation, original source occurrence, exact canonical outcome,
   recoverable response custody, and contiguous handled checkpoint. An absent
   session row, relay ACK, child completion, socket write, or elapsed time is
   not that certificate. Retain an equivalent tombstone after session
   retirement and start a fresh full parent tail only after authorized release.
   For cleanup, take sorted lanes before canonical parents, then entry locks.
   GC takes canonical parents with `SKIP LOCKED`, never a lane, and checks for
   **any** mailbox row, including terminal rows, in a fresh READ COMMITTED
   snapshot before deleting dependent deliveries, a rejection child, or the
   parent. Excluding terminal rows can cause unsafe collection attempts or
   foreign-key failures; an entry-first cleaner would additionally permit a
   parent/entry deadlock. Both appender-first and GC-first schedules need real
   PostgreSQL barrier tests.

The existing ingress commit takes the SM stream lock before canonical
resolution, so the proposed lane-first order requires an explicit source
admission redesign. These are architecture findings, not an accepted storage
contract or evidence of a passing implementation. Keep #1986's DDL and
repository gate closed until #2116 and the three contracts are resolved and
reviewed on the implementation tree.

## Candidate storage contract

The names and physical migration are intentionally not selected. This is a
contract for review after #2116, not executable DDL.

- A **lane record** has a stable identifier, a versioned canonical identity and
  its checked sequence counter. A bounded digest of a length-delimited encoding
  including every type discriminator can index the lane identity. Retain the
  complete normalized values and compare them on every resolution; a digest
  collision fails closed rather than merging lanes. The encoding version cannot
  change lane identity silently. This matters because maximum-size origin and
  recipient JIDs can exceed PostgreSQL's composite B-tree index-row limit; the
  existing ingress alias schema documents the same constraint
  ([`waddle.rs`](../../server/crates/waddle-server/src/db/migrations/waddle.rs)).
- An **entry** is uniquely identified by lane and positive sequence. Its closed
  typed role refers to either a canonical message plus exact effect/delivery,
  or a reviewed canonical non-message request such as a join. It stores no
  stanza or duplicate frozen manifest. Foreign-key and repository checks for
  the message variant must establish the same message and exact reference;
  the join variant needs its own exact canonical reference checks. Neither may
  cascade away live custody. In particular,
  `ingress_deliveries` keys a delivery separately from its message; a lookup or
  foreign key on the delivery key alone must not authorize a mismatched pair.
- A bounded exact-operation identity must be known before attempting commit and
  independent of the chosen lane and physical owner. Propose a unique index on
  the entry itself over that identity; its actual canonical inputs and
  relationship to existing effect/delivery references remain blocked by
  questions 1 and 2. Bind the operation to its logical recipient and reviewed
  lane/handoff contract. Changing the physical owner must not create another
  operation. Reassigning an existing operation to a different lane or logical
  target conflicts; a separately authenticated cross-lane duplicate may only
  look up or reconcile the original responsibility without moving it; after
  authorized row cleanup it reads the retained canonical outcome. A
  lane-scoped index alone cannot enforce this. Use fixed-width keys or a
  bounded digest with full-value equality and collision failure; handle nullable
  variant columns explicitly so SQL null uniqueness cannot admit duplicates.
  Different child deliveries need their own canonical keys, not invented
  mailbox identities.
  Validate each reference against its recorded canonical obligation before
  appending, and fail closed on invalid stored discriminants. Resolve each
  operation's uniqueness scope during the design gate. A separate bounded
  target-first lookup supports later polling; its layout is subject to P2.4.
- Request outcomes need a closed, single-authority sum such as `Pending`,
  `OwnerAccepted { canonical_result }` or
  `OwnerRejected { canonical_rejection }`; child delivery dispositions are
  separate. The owner must commit a rejection's typed reason, original request
  and frozen error decision without minting an accepted manifest. The review
  must define each legal transition and its exact actor, compare-and-set fence
  and proof. Dead letters retain recoverable custody until authorized repair
  or final policy disposition; they are not successful delivery. Advancing a
  cursor past one requires an explicit ordering and policy decision. Terminal
  transitions cannot erase evidence needed by a prior acceptance **or
  rejection** lookup while retry remains possible.

A checksummed, expand-only, append-only ledger migration may add the reviewed
tables only after the #2116 gate. It must cover both tables in the actual epoch-guard
manifest and `EPOCH_GUARDED_TABLES`, including guarded DML and truncate, and
follow the reviewed owner/runtime role, schema-qualification and migration-lock
contract. No initializer DDL or pool escape is allowed. The repository exposes
only typed transaction-taking append, bounded exclusive-cursor read and exact
fenced disposition operations. It does not own a separate transaction or
connection.

## Acceptance, consumption and retention

1. **Acceptance:** the origin commits a canonical request and its mailbox
   responsibility together. For a room **message**, the owner checks its exact
   current claim and occupancy/lifecycle authority and, in one authoritative
   transaction, freezes the final canonical manifest, authoritative room stanza
   ID and complete child keys and changes the existing request's disposition to
   accepted. A join instead needs a separately reviewed typed canonical result,
   occupancy projection and response-custody contract before its lane cuts over.
   The row refers to its applicable canonical evidence; another table, relay
   ACK or in-memory flag must not independently declare acceptance. A rejected
   request cannot fabricate an accepted manifest. A committed owner rejection
   instead records a canonical frozen error result and its response custody.
   The origin advances `h` only after it positively verifies an accepted or
   rejected handled result and commits the contiguous source frontier; an
   unavailable, absent, stale or ambiguous lookup cannot advance it. The actual
   acceptance transaction and wire behavior are #1948/P2.3 work.
2. **Uncertain commit:** the stable operation identity is available before the
   write. After a lost reply or indeterminate commit, a successful authoritative
   lookup of the exact identity and matching canonical evidence can recover the
   same accepted or rejected result. An absent, unavailable or conflicting
   lookup is not positive commit proof and cannot advance `h` or justify
   replacing the manifest or frozen rejection. Retry response occurrences and
   refusal semantics must be specified with #1948.
3. **Consumption:** the target checks the exact current claim and row state;
   its transaction commits the disposition with the corresponding canonical
   receipt or durable same-key downstream custody. A socket write or relay ACK
   is not a durable SM append. Stale owners, changed targets and late responses
   cannot retire successor obligations. The existing canonical receipts remain
   the effect-completion authority.
4. **Retention:** adding, settling or removing mailbox custody and the existing
   collector synchronize on the applicable canonical parent. A live or repairable
   entry keeps that parent, identity and evidence available. The current message
   collector needs extension for any approved non-message request variant.
   Once every canonical obligation, SM reference and retained mailbox entry
   permits collection,
   begin a fresh full ingress retention tail. Integrate the candidate scan,
   locked recheck, retention refresh and final delete. The post-lock recheck
   must use a fresh READ COMMITTED statement snapshot or an independently proven
   equivalent; an old snapshot cannot establish that no custody was added. A
   foreign key alone is not a retention policy. The existing collector and
   descendants are in
   [`substrate.rs`](../../server/crates/waddle-server/src/ingress_substrate/substrate.rs)
   and [`retention.rs`](../../server/crates/waddle-server/src/ingress_substrate/retention.rs).
   Specify who removes final mailbox entries and how long positive-lookup proof
   survives before the parent becomes eligible; P2.4 owns the worker policy,
   not a competing message GC authority.

The global lock order is protocol guard, ordered lane, `UserClaim`, endpoint,
resource/effects, P1 receipts, then SM claim/session/unacked. Exact room and
origin claims and multiple lanes/parents need a reviewed deterministic order.
An appender must not take a lane after a canonical parent lock. In particular,
GC holding a canonical message lock must not wait for a lane or child lock held
by an appender waiting for that message. Design terminal-entry cleanup and
foreign-key checks around this order; use bounded retry where needed, and prove
both sides of the race. No transaction may wait on network or an external
callback. #2116 owns first-lock, physical attestation, role, isolation,
cancellation and migration-transition enforcement.

## Deterministic PostgreSQL test matrix

These are future acceptance tests, not evidence that a harness or implementation
exists. Use independent real PostgreSQL connections, isolated fixtures and
explicit barriers; a sleep or an in-memory mock is not evidence of lock order.
Record the selected tests and executed inventory with zero silent PostgreSQL
skips. Add the applicable SQLite tests if the backend decision requires them.

| Area | Controlled schedule and required result |
| --- | --- |
| Allocation — P2.2 | Hold the first lane lock while a second appender attempts the same lane; prove it cannot commit first, then release and assert unique monotonic sequences. Concurrent different lanes make independent progress. Roll back the first allocation and show no committed gap. Test the maximum and explicit exhaustion. |
| Idempotency — P2.2 | Race the same pre-known operation across lanes; at most one responsibility commits, and an attempted lane or logical-target reassignment conflicts. A separately authenticated cross-lane duplicate reads the original responsibility without moving it; while that responsibility is pending, the new source frontier cannot overtake it. Replan a remote dispatch from owner A to B, whose effect keys differ: the request identity must recover the same accepted or rejected result, not allocate a second one. Distinct requests and child keys remain distinct. Test maximal JIDs, discriminators, encoding round trips and an injected digest collision that fails closed. |
| Fences and handoff — P2.2/P2.3 | Change origin and target owner epochs and node incarnations while an entry is pending and while acceptance or consumption is paused; stale updates affect zero rows or return a typed fence failure. Include room lifecycle/occupancy changes, multiple-lane lock order and SM resume. Exercise the actual maybe-committed `SmSession` join followed by `UserActor` repair with `canonical: None`, restart and exclusive-cursor recovery: the reviewed custody and handoff preserve join-before-message ordering. |
| Epoch and roles — P2.2/#2116 | Under the reviewed restricted role and real migration, verify both new tables in the manifest and DML/truncate guards. At the activated test epoch, unproven, stale and unsupported-epoch writes fail; properly prepared writes succeed. Exercise owner transition, schema shadowing and migration ordering as required by #2116, rather than inferring those guarantees from an epoch-zero write. |
| Acceptance — #1948/P2.3 | Pause before owner commit and prove the origin cannot advance `h`; then commit the frozen manifest and all child identities before acknowledgement. Change membership around acceptance and retry: recover the original manifest, stanza ID and child keys. Owner rejection, unavailable owner and stale claims never manufacture acceptance. |
| Unknown commit — P2.2/#1948 | Lose the reply or cancel after an owner commit, and recover by the exact authoritative operation lookup without a second row, manifest or allocation. Exercise unavailable, absent and mismatched lookup results; none authorize success. Preserve proof across owner restart and handoff. |
| Consumption — P2.3 | Pause before and after a target transaction commits its exact receipt or durable custody; retry and ownership transfer do not duplicate or retarget the effect. A relay ACK or socket write alone cannot settle the canonical obligation. Pending children may remain after owner acceptance. |
| Cursor and dispositions — P2.2/P2.3/P2.4 | Read bounded pages strictly after the exclusive cursor. Test concurrent appends, retained gaps/deleted entries and every allowed/forbidden transition. A dead letter or policy discard cannot silently skip an ordered obligation; P2.3/P2.4 must establish the applicable advance and repair policy. |
| GC and lock order — P2.2/P2.4 | Lose the owner reply after acceptance, complete children and advance time past the existing eight-day tail while the origin still cannot resolve acceptance; GC must preserve exact proof through owner transfer and origin restart. After the reviewed durable release condition, clean the entry and start a full parent tail. Race append, release and cleanup with parent-first GC in both lock schedules; no dangling reference, deadlock, stale-owner deletion or early collection is permitted. Test no-cascade and wrong-parent references. |
| Migration — P2.2 | Fresh install and supported upgrade preserve existing data; ledger checksums and guard inventory match; unsupported old binaries and partial installation fail according to #2116's reviewed migration contract. No runtime writer or wire-format change is introduced. |

P2.2 acceptance requires the reviewed choices recorded on #1986, a dark
migration and typed transactional repositories, the P2.2 portions of the real
PostgreSQL matrix, and a retention and lock-order review on the integrated diff.
The P2.3, P2.4 and #1948 rows describe their later contract tests; they do not
move those implementations into P2.2. This document alone satisfies none of the
implementation gates.
