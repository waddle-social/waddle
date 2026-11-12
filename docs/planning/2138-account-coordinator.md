# #2138 account coordinator: decision record

This records the model the account-resource work under
[#2138](https://github.com/waddle-social/waddle/issues/2138) builds to. It
applies decisions D3, D4 and D5 of
[`2046-completion-contracts.md`](2046-completion-contracts.md) to one
component and settles the points on which the earlier notes differ. Nothing
here is active: the connection registry and `UserActor` remain the writers
and the routing authority until the activation boundary at the end of this
document is crossed.

## One authority per bare JID

The account coordinator is the single authority for the resources of one
bare JID. It lives on the node that holds the account's `UserActor` claim
and is identified by the complete `UserClaimIdentity` (bare JID, owning node
lease, claim epoch, actor incarnation). It holds, per full JID, one **slot**:

- the exact `BindingRef` of the slot's current binding and that binding's
  lifecycle state;
- the selection metadata routing reads: presence availability, show, status
  and priority, and whether carbons, roster pushes and block-list pushes are
  wanted;
- two presentation flags, "offline messages flushed" and "pending
  subscription requests flushed".

A full JID names a slot, never a connection. Every mutation names the
`BindingRef` it acts for, and the coordinator refuses one that is not the
slot's current binding. A delayed command that carries only a full JID
cannot change a replacement.

The presentation flags are keyed by the stream-management session when the
binding has one and by the binding otherwise. A resume mints a new binding
for the same session, so a binding key alone would flush twice. The
coordinator holds the only copy of these flags. A detached session carries a
projection of them for the resume snapshot; it is not a second authority.

## Binding lifecycle

A binding moves through the states the pure model in
`waddle-xmpp/src/ownership/account_resource_transition.rs` already defines
and tests:

| State | Meaning | Routable |
| --- | --- | --- |
| Prepared | The coordinator has recorded the binding; nothing is published. | No |
| Published | The endpoint is published non-routable under this binding. | No |
| Active | The durable row committed. The binding is the slot's destination. | Yes |
| Detached | The endpoint is sealed; the logical resource stays resumable. | No |
| Retiring | Retirement has started and awaits the endpoint's evidence. | No |
| Retired | Terminal. The slot may take a new binding. | No |

The coordinator API names the edges: **prepare** (to Prepared), **install**
(Prepared to Published), **confirm** (Published to Active), **detach**,
**retire** (start and complete). "Replaced" is not a state of its own: a
replacement binding is prepared while the old one retires, and the slot's
install generation tells them apart.

## Publish before bind success

1. The connection actor mints a `BindingRef` and asks the coordinator to
   prepare it. The coordinator records it and returns.
2. The actor publishes its endpoint non-routable and reports install.
3. The node lease, user claim, binding row, resource row, stream-management
   session and the operation's outcome commit in one transaction through the
   typed ingress entry. On commit the coordinator activates the binding.
4. Only then does the actor answer the client with the bind result or
   `<resumed/>`.

A failure before step 3 retires the provisional binding by its exact
`BindingRef`. An unknown commit outcome in step 3 stays unresolved under the
same operation identity; only a known rejection lets the client proceed to a
fresh bind, which takes a new `BindingRef` on the same `ConnectionRef`.

## When an update affects routing

Presence, priority and the three interest flags reach routing in this order:

1. The connection sends the update to the coordinator with its `BindingRef`
   and the slot revision it last saw.
2. The coordinator checks the fence and the revision, applies the change to
   the slot, and records the operation's outcome under the operation
   identity, so a lost reply can be resolved without applying it twice.
3. Routing reads the slot. A read that starts after step 2 sees the change;
   one that started before does not.
4. The coordinator replies, and the connection updates its own read-only
   projection, which it uses only for its detach snapshot.

The connection's projection is never read by routing. Until activation the
connection registry's entry is what routing reads, and the coordinator's
slot is compared against it, not consulted.

## Selection

Selection is a pure function of the account's slots, with the rules the
`UserActor` implements today:

- **Every resource**: all slots that hold a binding, with no presence filter.
- **Available resources**: slots whose presence is available, with their
  priority.
- **Routable resources** for a stanza to the bare JID (RFC 6121 section
  8.5.2.1.1): among available slots with a non-negative priority, all those
  whose priority equals the highest. An empty result means offline handling.
- **Routing snapshot**: every slot's availability, priority and carbons
  flag, ordered by full JID, read in one step so carbon policy and
  destination selection agree.

A lookup the coordinator cannot answer is a typed error, never an empty
selection: "no resources" and "did not answer" lead to different outcomes
for offline storage and for occupancy eviction.

## Replacement fencing

A second bind for an occupied full JID is a new binding for the same slot.
The coordinator records the pending replacement and returns. It does not
wait for the old connection. The old connection actor retires on its own,
waits for its pump to terminate within its deadline, and reports completion
as a message carrying its `BindingRef`. The replacement becomes Active only
after that report on the same node. Across nodes the old pump cannot be
observed: the old node loses write authority with its lease, and the
durable session records the exposure ceiling. The coordinator never calls
back into a connection that may be waiting on it.

## Bounded unregister and repair

Unregister names a `BindingRef`. A stale one is a no-op with a typed result.
The coordinator keeps a bounded set of retirements whose endpoint evidence
has not arrived. Each has a deadline; past it the binding is reported for
repair and stays non-routable. Repair re-reads the durable row and either
completes the retirement or reports the row as held by a newer binding. It
never revives a retired binding.

## Owner migration

When the account's claim moves to another node, the new owner's coordinator
loads the resource rows under the new `UserClaimIdentity` and adopts each
binding whose row names a live `ConnectionRef`. Sockets do not move: a
connection hosted on another node stays there and is reached through the
addressing boundary. Adoption does not mint a stream-management session or
change a binding's `BindingRef`.

## Directed presence teardown

Directed presence a resource sent is owned by the slot. Detach keeps it,
because the resource may resume. A successful resume keeps it and moves it
to the new binding. Replacement and terminal retirement send the
unavailable presences and record that they were sent as part of the
retirement evidence, so a retried retirement does not send them again.

## Durable rows

A resource row is keyed by full JID and carries the claim identity and the
exact `ConnectionRef` and binding attempt of its current binding, the
install generation, the selection metadata and the presentation flags. The
stream-management reference is optional, and its check against the session
table is deferred to commit, because the session row and the resource row
are written in the same transaction in either order.

## What the connection actor sends

The pure state lives in `waddle-xmpp/src/ownership/account_coordinator.rs`
(`AccountResources`) over the per-resource slot model. Every call is
`AccountBindingAuthority::execute(full_jid, operation)`, where the operation has an
identity the caller keeps for retries and a mutation that names the claim
identity, the slot revision the caller last saw and the `BindingRef`. The
reply is `Applied` or `Replayed` with either `Committed { revision }` or a
typed rejection. Repeating an operation identity returns the recorded
outcome; reusing one with a different payload is an error.

| Event at the connection | Mutation | Binding state after |
| --- | --- | --- |
| Bind requested, or a fresh bind after a failed resume on the same socket (new `BindingRef`, same `ConnectionRef`) | `Bind` | Prepared |
| Endpoint published non-routable | `PublishEndpoint` | Published |
| Durable rows committed | `RegisterActor` | Active; the actor may now answer the client |
| Presence, priority, carbons, roster or block-list interest changed | `UpdateSnapshot` | unchanged |
| Transport lost with a resumable session | `Detach` | Detached |
| Resume succeeded on a new socket (new `BindingRef` on the new `ConnectionRef`) | `SuccessfulResume` | Active, same slot and snapshot |
| Replacement, unregister or terminal cleanup started | `BeginRetirement` | Retiring |
| Pump observed terminated and teardown evidence collected | `CompleteRetirement` | Retired; the slot is free |
| The account's claim moved to this node | `ReplaceClaim`, then `AdoptBinding` per live binding | unchanged |

Before answering a delayed bind, the actor re-reads
`AccountResourceSlot::current_bind_success_receipt`, so a binding retired or
replaced in the meantime is not acknowledged. Offline and
pending-subscription flushes call `AccountResources::mark_presented` first
and present only when it returns `true`.

## What is built

- **Rows.** `account_resource_bindings` holds one row per full JID and
  `account_resource_operations` the committed outcome of each operation
  identity (`waddle-server/src/account_bindings`). An owner initializer
  creates them in the owner phase; the runtime role attaches and verifies.
  `AccountBindingStore::execute` rebuilds the slot from the row under its
  lock, applies the pure transition and writes the result, so the store
  and the coordinator share one set of rules.
- **Service.** `AccountCoordinators` (`waddle-server/src/account_coordinator`)
  runs one actor per bare JID with a bounded mailbox, created on demand
  under the account's current claim and stopped when the account holds no
  binding and has been idle. It commits a mutation in the store first and
  applies it to its in-memory slots second. A store failure that leaves a
  commit outcome unknown drops the in-memory slots, which are reloaded from
  the rows before the next request.
- **Connection contract.** A connection actor calls the trait
  `AccountBindingAuthority`: `slot_status`, `execute`, `bind_success`,
  `mark_presented` and `unregister`.
- **Selector.** `AppState::account_authority()` returns the
  `AccountAuthoritySelection` chosen by `WADDLE_XMPP_ACCOUNT_AUTHORITY`
  (`registry` by default, `coordinator`; any other value fails startup).
  The coordinator is selectable only together with the durable
  stream-management authority, because a binding's session reference
  points at the durable session relation. When it is selected, the
  recipient inventory, bare-JID target selection and the roster and
  block-list push sets are read from the coordinator.

No connection sends the binding messages yet, so a selected coordinator
has no bindings and reads as an account without resources.

## Session reference and release order

A resource row names the stream-management session of its current binding
by session id and generation. A commit that sets or changes the reference
is refused unless the durable session row exists and is bound at that
generation, and the session id is also a foreign key checked at commit. The
session therefore outlives every row that names it: **the resource row
releases first, the session row is deleted last.**

- **Replacement and close with a connection.** The displaced or closing
  connection retires its binding. `CompleteRetirement` frees the row and
  clears the reference in the same commit.
- **Expiry and close of a detached session.** No connection exists. The
  session authority marks the session terminal in its own transaction,
  which does not touch the resource row, and then calls
  `AccountCoordinators::release_session` for each full JID that
  `AccountBindingStore::resources_for_session` returns. The coordinator
  retires the detached binding with the caller's custody and teardown
  dispositions. A binding that still has a connection is reported as
  `HeldByConnection` and left to it.
- **Deleting the session row.** The session authority deletes a terminal
  row only when `resources_for_session` is empty. A delete that fails the
  foreign key at commit means a row still names the session: it is retried
  after the release, not treated as a fault.
- **Resume.** The session authority binds the new generation first; the
  connection then sends `SuccessfulResume` naming that generation.

## Decisions the service makes

- **Pending replacement.** A bind for a resource that has a binding is
  answered `ReplacementPending` with the displaced `BindingRef` and the
  slot revision. The resource is reserved for that binding until a
  deadline; a third binding is refused meanwhile. When the displaced
  binding has retired, the replacing connection binds again under a new
  operation identity. The coordinator does not start the displaced
  binding's retirement, because only its connection knows what became of
  the work it accepted.
- **Rejections are not stored.** The store records committed operations.
  A rejected operation is recorded in the coordinator's journal only, and
  is decided afresh after a reload, which is safe because it changed
  nothing.
- **Free rows.** A row whose retirement completed is kept while its
  coordinator runs and deleted when the account needs the slot, when the
  coordinator is reclaimed and when a coordinator loads the account.
- **Repair evidence.** Repair completes a retirement only with the
  repairer's statement of the directed-presence and presentation-flush
  outcome. An unknown outcome is refused and the binding stays
  non-routable.
- **Takeover.** A coordinator that loads rows written under a previous
  claim identity replaces the claim on every row and adopts a prepared,
  published or active binding whose connection is live. Liveness is local:
  a connection is live when it was accepted under this node's current
  lease. Every other binding stays under its previous binding claim, is
  not a destination, and is reported so it can be retired. A detached
  binding is among them: the slot model has no adoption for a detached
  binding, so a session detached under a previous claim cannot resume.
- **Over the slot bound.** Rows that hold a binding are always loaded,
  even beyond the account's slot bound. Such an account serves its stored
  bindings and refuses a bind of any new resource until enough retire.
- **Presence payloads.** A row stores the selection metadata, not the
  presence extension payloads. They live in the coordinator's memory and
  are empty after a reload until the resource updates its snapshot.
- **Presentation marks.** A presentation mark whose commit outcome is
  unknown resolves to "already presented" when it is repeated.

## Choices where the notes differ

- **Fence.** `2046-local-binding-id.md` fences registry mutations with a
  node-local `LocalConnectionBindingId`; D3 names `BindingRef`. The
  coordinator uses `BindingRef` only. The local ID stays for remote-hosted
  mirror entries until the registration snapshot carries a `BindingRef`,
  and is deleted with them.
- **States.** The task framing for this work names four states
  (provisional, active, retiring, retired). The merged model has six. The
  six are kept: Prepared and Published are both "provisional" but differ in
  whether an endpoint exists to retire, and Detached is what makes resume
  preserve the slot.
- **Presentation flags.** `ConnectionEntry` holds them per entry and
  `DetachedSession` omits the offline flag. They move to the coordinator
  under the session-or-binding key, with no second copy in the receipt
  state of the offline and subscription flushes.
- **Observation versus authority.** `2046-account-transition-observer.md`
  compares the model with `UserActor` decisions in test support. That stays
  a test facility. The coordinator is a separate component that becomes
  the authority at activation; it is not derived from the observer.

## Activation and rollback boundary

The connection actor (D), the account coordinator (E) and durable stream
management (F) switch together, at one point:

- **Before**: the connection registry and `UserActor` are written by the
  WebSocket loop and are what routing reads. The coordinator may be fed and
  compared, and its reads sit behind a selector that defaults to the
  registry.
- **Switch**: every supported transition (bind, replacement, rebind after a
  failed resume, detach, resume, cleanup) sends its coordinator message and
  commits its durable rows; the selector reads the coordinator; the registry
  is no longer written.
- **Rollback** is possible only until a binding has been activated by the
  coordinator without a registry entry. After that the registry cannot be
  made authoritative again without disconnecting those bindings, so
  rollback past that point is a restart on the previous release, with the
  durable rows as the source for resume.

Deletion of `dual_registration`, the `UserActor` resource map and the
registry's ownership of `connections` and `bind_slots` follows the switch
and is listed in `docs/runbooks/retirement-inventory.md`.
