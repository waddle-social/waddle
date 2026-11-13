# Direct lanes: bare-JID and full-JID delivery through the durable mailbox

Decision record for moving delivery to a user whose account is owned by
another node ([#2173](https://github.com/waddle-social/waddle/issues/2173))
from the ordered relay to the durable mailbox. It fills the "Full-JID and
bare-JID delivery" row of
[`2046-relay-lane-responsibilities.md`](2046-relay-lane-responsibilities.md)
and follows the mailbox model of
[`p2-2-durable-mailbox-core.md`](p2-2-durable-mailbox-core.md) and the
request pattern of [`2174-room-request-lanes.md`](2174-room-request-lanes.md).
`S/` is `server/crates/waddle-server/src/`, `X/` is
`server/crates/waddle-xmpp/src/`, `W/` is `S/server/routes/websocket/`.

Status: see [Build state](#build-state) at the end.

## What the relay does for a user recipient

Every send to a `BareJid` or `FullJid` recipient is built in
`S/clustering/route_bridge/delivery/ordered.rs` as a `RemoteDeliverySeed`
and sent by `deliver_seeded_remote` (`ordered_send.rs`). Its callers are
`S/clustering/addressing.rs`, `route_to_connection`, the immediate delivery
effects (`RelayFullJid`, `RelayBareJid`, `RelayCarbons`), the carbon relay
(`interpret/carbons.rs`) and the ingress executor's relay copy
(`S/ingress/execute_relay_copy.rs`). The seed carries:

- the channel: an origin (a stream-management session or an entity), the
  recipient, the origin's claim epoch and the epoch of the recipient
  account's `UserActor` claim;
- the payload: `ProcessedDirectMessage` (a frozen recipient copy that skips
  the recipient pipeline), `Message`, `Iq` or `Presence`, and for a message
  that an ingress transaction owns, the recorded append obligation
  (`IngressAppendObligationRef`: the canonical message key and the copy's
  receipt key).

The receiver (`delivery/local.rs`, `receiver.rs`) runs the payload on the
node that holds the account's `UserActor` claim: a full JID through the
local connection or the stream-management registry, a bare JID through the
recipient pipeline. It answers `TargetUnavailable` when nothing took the
stanza, and the origin then runs its own fallback (offline storage, a
bounce, an IQ error). An uncertain outcome (`MaybeCommitted`) suppresses
that fallback and is not retried.

A send whose origin is a resource that is itself hosted through a remote
channel mirror (`route_remote_resource_origin`) does not use an ordered
channel to the account owner. It is not a direct lane and stays as it is
until the mirrors are retired (retirement inventory, "Remote channel
mirrors").

## Decisions

| # | Question | Decision |
| --- | --- | --- |
| D1 | Operation identity | `MailboxOperationKey::Direct(DirectRequestKey)`. The key of a copy that an ingress transaction owns is derived from the canonical message key, the copy's receipt key and the recipient JID, so every attempt and every recovery of that copy is the same operation. Any other copy (an IQ, a presence, a message outside an ingress transaction) mints its key before the first attempt. |
| D2 | Where the work is stored | A sibling table, `ingress_direct_requests`, holding what the relay envelope would have carried. `ingress_room_requests` is not generalised: its kinds, its occupancy and its frozen responses stay room-only. The pattern is reused: the request is the parent of its mailbox entry, the owner latches a decision once, the origin settles it. |
| D3 | The lane | Origin: the relay channel's origin (`MailboxLaneOrigin::SmSession` or `Entity`). Recipient: `MailboxLaneRecipient::FullJid` or `BareJid`, both already part of lane encoding version 1. |
| D4 | The owner | The node that holds the `UserActor` claim of the recipient's bare JID, for both recipient kinds. It is the claim the relay channel already names as its target. |
| D5 | What acceptance commits | Custody of the stanza by the recipient's session path, then the decision. See [Acceptance](#acceptance). |
| D6 | Refusal | `Rejected` means nothing took the stanza. The origin reads it as it reads a `TargetUnavailable` refusal and runs the fallback it runs today. |
| D7 | Gate | Protocol epoch 2 or later, read on the origin when it sends; durable stream management is the only stream-management authority. Otherwise the relay carries the stanza. |
| D8 | Wire | No change to `RemoteStanzaEnvelope` or the reply types. A receiver keeps accepting `BareJid` and `FullJid` payloads, because the relay still serves the cases listed under [What stays on the relay](#what-stays-on-the-relay). `deliver_ordered` stays at `v16`. |

## The request

`ingress_direct_requests`, PostgreSQL only, under the epoch guard
(migration V1033, `server/crates/waddle-server/sql/baseline/postgres.sql`, which carries it since the schema baseline):

| Column | Meaning |
| --- | --- |
| `request_key UUID PRIMARY KEY` | The `DirectRequestKey`. |
| `recipient_jid TEXT` | The lane's recipient, bare or full. |
| `sender_entity TEXT` | The entity whose claim the origin presented as sender; the target resolves an append obligation against it, as the relay receiver does with the envelope's sender claim. |
| `payload BYTEA` | The `OrderedRelayPayload` the relay would have sent, serialised once at this boundary and parsed back by the repository. A payload that names another recipient than the row, or a room payload, is refused in both directions. |
| `message_key UUID NULL REFERENCES ingress_messages ON DELETE CASCADE` | Set for a copy an ingress transaction owns, so the request leaves with its canonical message. |
| `owner_decision SMALLINT` | 0 undecided, 1 accepted, 2 rejected. Moves once, from 0. |
| `refusal SMALLINT NULL` | For a rejection: 1 nothing took the stanza, 2 the recipient's session is hosted by another node. |
| `settled_at TIMESTAMPTZ NULL` | Set by the origin when it has read the decision. |
| `created_at TIMESTAMPTZ` | For retention and monitoring. |

`ingress_mailbox_entries` gains a nullable `direct_key UUID REFERENCES
ingress_direct_requests ON DELETE CASCADE`, operation kind 6, a parent check
that requires exactly `direct_key` for kind 6, and a partial unique index on
`direct_key`. The guarded-table count goes from 18 to 19. The runtime role
holds `SELECT, INSERT, UPDATE, DELETE` on the table. The definer-API
candidate gets the guard trigger and no function, so the repository refuses
with `DefinerApiUnsupported` there.

- **Origin commit.** One ingress transaction locks the lane, inserts the
  request and appends the entry `Pending`. A retry with the same key finds
  both and writes nothing. The existing per-lane and per-node pending caps
  apply; a refused append is reported to the caller as backpressure, which
  is what a full relay channel reports today.
- **Retention.** As for room requests: collected with its entry once the
  entry is `Consumed` and `settled_at` is an hour old. A request that
  refers to a canonical message also leaves with that message, and keeps
  the message from collection while its entry is not consumed
  (`retained_child_unions` in `S/ingress_substrate/substrate.rs`): the
  target's worker proves the copy against the message and the origin
  reads the decision through the entry.
- **Dead letters.** The entry stays `Pending` at its lane head. The origin's
  wait has already ended as uncertain.

## Acceptance

The target's worker (`decide_direct` in
`S/clustering/route_bridge/delivery/direct.rs`) runs the stored payload
through the code the relay receiver runs (`deliver_user_payload`: the
connection or the stream-management session registry for a full JID, the
recipient pipeline for a bare JID), and commits one ingress transaction
(`IngressAuthority::decide_direct_request`) that asserts the account's
`UserActor` claim, locks the lane as its target, latches the request's
decision and moves the cursor.

Custody is taken by the recipient's session path, which is where the
admission authority lives; the worker itself admits nothing:

- A **live** connection admits the frame to durable stream management on
  its own `Owner` fence when it takes it from its endpoint
  (`W/stream_management/durable.rs`). It holds the only projection of the
  session's outbound window and checks that each admission it makes is the
  next sequence, so a second admitter on the same fence would
  desynchronise it.
- A **detached** session is appended to through the stream-management
  session registry. Under the durable selector the registry's detached
  readers and appenders also consult the durable authority
  (`S/sm_authority/detached.rs`, attached with
  `InMemorySmSessionRegistry::attach_durable_detached`): a `Detached`
  session inside its resume window is listed with the presence, carbons and
  interest flags of its stored resource snapshot, and a stanza for it is
  admitted on `AdmissionAuthority::Detached` under the session's claim,
  retained unexposed and replayed on resume. The claim is taken on the
  admitting node first when another node holds it, by the swap a resume of
  a detached session uses. The worker reaches these sessions through the
  same registry calls, without change.

The order of hand-over and decision depends on what makes a repeated
attempt safe:

| Stanza | Order | A repeated attempt |
| --- | --- | --- |
| A copy an ingress transaction owns, to a full JID | hand over, then decide | The append records the copy's ingress receipt, so the repeat is recognised and nothing is delivered twice. `TargetUnavailable` from the hand-over is decided `Rejected`; any other failure is retried under the entry's retry budget. |
| Any other stanza (IQ, presence, a message outside an ingress transaction, anything to a bare JID) | check that something on the node could take it, decide, then hand over once | Not repeated: an entry that records a decision is only moved. A failure after the decision loses the stanza, as an uncertain relay outcome does today, and cannot deliver it twice. |
| Nothing on the node could take it | decide `Rejected` | — |

A resource without stream management falls under the second row: the entry
is decided before the write attempt, which is the non-SM
attempted-uncertain outcome the relay has.

The account owner's worker cannot tell a session claimed by a third node
from an absent one, and refuses both as not delivered. The refusal "hosted
elsewhere" is written for bound requests (see "Bound requests").

### Bare JID

The account owner freezes the resource selection when it accepts: the
recipient pipeline runs once on the target, as it does for a relayed
bare-JID stanza, after the decision is recorded. A repeated attempt does
not run the pipeline again.

## Consumption by the origin

The origin's seam is `deliver_seeded_remote` (`ordered_send.rs`), which
every user-recipient send passes through: `deliver_seeded_direct` commits
the request under the channel's send lock, so the lane's sequence is the
order the origin sent in, and then waits up to ten seconds for the decision
without the lock.
One transaction locks the lane as origin, checks the entry's disposition
against the request's `owner_decision`, consumes the entry and settles the
request. The outcome maps onto what the relay's callers already handle:

| Mailbox outcome | Caller sees |
| --- | --- |
| Accepted | `Delivered` |
| Rejected (either refusal) | `Unavailable`: the caller runs its local fallback |
| No decision within the budget, dead letter, storage failure after the commit | `MaybeCommitted`: no fallback, no second send. The entry stays; the target's poller decides it, and a copy owned by an ingress transaction is also covered by that message's receipts and the ingress maintenance pass. |
| The lane or the account owner's node is at its pending cap | a definite no-effect outcome (`Dropped`, or `Unavailable` for an IQ), as for a full relay channel |
| Nothing committed (not gated, no `UserActor` claim, storage failure before the commit) | the relay is used |

There is never a local fallback or a relay send after a commit whose
decision is unknown.

## Lost hint, adoption, carbons, offline

- **Lost hint.** The origin hints the target node after its commit; the
  target's poller scans its backlog every second regardless.
- **Adoption.** A lane records the owner incarnation its origin last
  proved. The adoption pass reads the `UserActor` claim of the recipient's
  bare JID for a lane with a `BareJid` or `FullJid` recipient and writes
  that claim onto the lane when this node holds it. There is no seating
  step: the account's sessions are read from durable stream management and
  the local registries when an entry is worked.
- **Carbons.** Unchanged. `RelayCarbons` addresses the carbon owner's
  account; the account owner selects the carbon-enabled resources and
  records the per-resource carbon receipts (`S/ingress_uow/carbon_receipts.rs`)
  exactly as it does for a relayed carbon. Only the transport of the request
  to the account owner changes.
- **Offline storage and bounces.** Unchanged. They run on the origin, from
  a `Rejected` decision instead of a `TargetUnavailable` refusal.
- **Call setup tickets and deferred handoff.** The caller-side handling in
  `ordered.rs` (`defer_until_relay_completion`, `call_setup`) wraps
  `deliver_seeded_remote` and is not changed: the mailbox path returns the
  same outcome type from inside it.

## What stays on the relay

- Every send when the gate is off: epoch below 2.
- A send whose origin resource is hosted through a remote channel mirror
  (`route_remote_resource_origin`).
- A send for which the origin could not commit a request.

## Bound requests

With the account coordinator selected, a stanza for a full JID whose
active binding another node hosts is not given to the account owner: it is
a direct request addressed to the node that hosts the binding (migration
V1037, now in `server/crates/waddle-server/sql/baseline/postgres.sql`).

- **The request** records the target node incarnation
  (`target_node_id`, `target_node_epoch`) and the exact binding
  (`target_binding`), all three or none. A request without a target is
  decided by the account owner as above. A key names one request, target
  included.
- **The lane** is (the account's `UserActor` entity, the full JID). Every
  origin on every node shares it, so the hosting node decides a resource's
  stanzas in commit order. Its fence takes both epochs from the account's
  claim and names the hosting node as target; a claim that moves
  supersedes it. When the resource binds again on another node under the
  same claim, the lane moves to that node only once it is drained
  (`MailboxRepository::lock_lane_retargeting_drained`); until then the
  origin's commit is refused with no effect and the caller runs its
  fallback.
- **The decision** (`IngressAuthority::decide_bound_request`) is taken by
  the node the lane names, under the lane lock. The target node hands the
  stanza only to the connection that carries the named binding, or to the
  resource's detached session when no connection holds the resource; a
  connection that carries another binding refuses it as not delivered. A
  copy an ingress transaction owns is handed over first and decided
  afterwards; anything else is decided first and handed over once.
- **A relayed append claim** is resolved by the origin before it commits a
  bound request (`try_deliver_registered_remote_resource_at`), as before it
  hands a frame to a remote channel mirror (#1790): a rejected claim is a
  definitive `Unavailable` there, recorded once, and never reaches the
  hosting node. The hosting node fences the claim again when it hands the
  copy over.
- **Hosted elsewhere.** Any other node that works the lane, which happens
  when a new account owner adopts it after the account's claim moved, may
  only refuse the request with the "hosted elsewhere" refusal. The origin
  reads every refusal as `Unavailable`.
- **Gate.** A bound request is carried only when the mailbox carries
  direct requests (D7). Otherwise the resource on the other node is
  reported `Unavailable` and the caller runs its fallback; the remote
  channel mirror is not used with the coordinator selected. A cluster
  test that expects live delivery to a full JID bound on another node
  therefore activates protocol epoch 2 first
  (`cluster_support::activate_mailbox_epoch`): the harness nodes start
  through the owner-as-runtime path, which never activates it.

## Deletion precondition for the `BareJid` and `FullJid` relay lanes

1. Every clustered deployment at protocol epoch 2 or later with durable
   stream management. The `relay` selector and the `legacy` mode are
   removed; what remains is the owner-as-runtime startup, which never
   leaves epoch 0 and is deleted after the restricted runtime cutover.
2. Remote channel mirrors retired, so that the account owner is the node
   that hosts every session of the account, or direct lanes targeted at the
   session's node instead of the account owner.
3. Then: remove the `BareJid` and `FullJid` recipients and the
   `ProcessedDirectMessage`, `Message`, `Iq` and `Presence` payloads with a
   `deliver_ordered` version bump, and delete `ordered.rs`,
   `ordered_send.rs` and the receiver.

## Build order

1. V1033, the repository, the operation and parent variants. Dark.
2. Target: resolve, take custody, decide; worker dispatch by parent;
   adoption of user lanes.
3. Origin: commit, hint, wait, consume, map the outcome; the gate.
4. Two-node tests.

## Build state

Built, not yet verified by a test run. V1033, the repository
(`S/ingress_uow/direct_requests.rs`), the origin commit, wait and
consumption and the owner's decision (`S/ingress/direct_request.rs`), the
origin seam and the worker (`delivery/direct.rs`), worker dispatch and
adoption of account lanes (`S/ingress/mailbox_worker.rs`) and retention
(`S/ingress/maintenance/implementation.rs`) are in the tree.

Bound requests (V1037, the target node and binding, the "hosted
elsewhere" refusal written by a node that is not the request's target)
and the PostgreSQL repository suite (`tests/direct_requests_postgres.rs`)
are in the tree, not yet verified by a test run.

Not built:

- A sweep that reports decided direct requests no origin consumed. The
  monitoring query `waddle_ingress_direct_requests` reports them.
