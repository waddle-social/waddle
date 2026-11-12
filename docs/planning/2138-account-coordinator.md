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
  points at the durable session relation.

When the coordinator is selected, these reads come from it: the recipient
inventory; bare-JID target selection; the carbon planner's and the carbon
fan-out's per-target policy; presence probes, including the probed
resource's show, status, priority and payloads; presence fan-out to a
subscriber's or a user's available resources; subscription delivery and its
roster pushes; last activity; entity-capability and PubSub fan-out targets;
MUC invite delivery; offline-delivery and inbox-update targets; and the
roster and block-list push sets. A read the coordinator cannot answer is an
error at the recipient inventory and an empty set with a warning elsewhere.

These reads stay on the registry in both modes: the connection cleanup and
registration paths, the cluster route bridge's registration snapshots and
its owner-side receiver, pending-delivery flush and recovery, the
stream-management promotion paths, the extension host's presence query, and
the admin channel command. They read a connection entry together with its
outbound endpoint, or belong to code that writes the registry, and move
with the switch of the writers.

### What a connection sends when the coordinator is selected

With `WADDLE_XMPP_ACCOUNT_AUTHORITY=coordinator` every `/ws` connection
drives the coordinator's slot for its resource
(`W/account_binding.rs`, `W/registration.rs`; `W` is
`waddle-server/src/server/routes/websocket`):

| Event | Calls |
| --- | --- |
| Bind, and a fresh bind after a failed resume | `slot_status`, `Bind`, `PublishEndpoint` before the registry entry is registered; `RegisterActor` and `bind_success` after the account's actor confirmed it, before stream-management finalization and the reply |
| Resume | The durable session commits the resume first, which binds the new generation. `SuccessfulResume` names the slot's detached binding and both generations, then `PublishEndpoint`, `RegisterActor`, `bind_success` |
| Presence, priority, carbons, roster or block-list interest changed, stream management enabled | `slot_status`, `UpdateSnapshot`, after the frame that changed it is handled and before the next one is read |
| Offline flush, pending-subscription flush | `mark_presented`; the flush runs only on `true` |
| Teardown that stored a resumable session | `Detach` |
| Any other teardown, a failed resume, a rebind on the same socket | `unregister`, then `repair` with the teardown dispositions |

Decisions this wiring makes:

- **Pending replacement.** The existing bind preparation force-detaches a
  local incumbent and waits for it to stop before the coordinator is
  asked, so a bind normally finds the slot free. When the coordinator
  still answers `ReplacementPending`, the replacing connection
  force-detaches the displaced binding if a local connection holds it,
  retires it itself if the coordinator reports it expired, and re-reads
  the slot every 50 ms until it is free or the bind-retirement budget of
  ten seconds ends. It then binds again, which is a new operation. A
  timeout fails the bind as an authoritative-registration failure, the
  stanza error a replacement timeout has always produced.
- **A resume without a detached slot.** When the slot holds no detached
  binding of the resumed session (the session was detached before the
  coordinator was selected, or its binding was retired after a resume that
  failed late), the binding is recorded with `Bind` instead. The durable
  session committed the resume and is the authority on it; a slot that
  holds any other binding refuses.
- **Order with the registry.** The coordinator records the binding before
  the registry registers its endpoint, and before the occupancy
  authority's publication guard takes a database connection. A registry
  or actor-tree refusal retires the coordinator's record.
- **Teardown is reported once, at its end.** The coordinator learns of a
  detach or a retirement after the connection's teardown ran, because only
  then is it known whether a resumable session was stored. Until then the
  slot still lists the resource; a stanza selected for it finds no
  endpoint and takes the detached-session or offline path, as it does for
  a stale registry entry. A detach the slot refuses falls back to
  retirement.
- **Teardown evidence.** The connection states that accepted work has a
  recovery owner (stream-management custody or offline delivery), that
  directed presence is retracted by the recovery paths, and that pending
  presentations belong to recovery. A binding that never entered the
  routing index states that it accepted nothing.
- **The bind-success receipt** is read when the binding is confirmed, not
  again when the reply is written.
- **A node that does not own the account** cannot record the binding: the
  coordinator refuses with the claim error and the bind fails. The
  coordinator can be selected only where a socket's node owns the claim of
  its account.

### What still writes the registry and the mirrors

The coordinator decides which resources receive a stanza. It does not
deliver one: delivery resolves the outbound endpoint through the
connection registry and the account's `UserActor`. Both are therefore
written under either selection, and with the coordinator selected a
binding is recorded in all three. The reads that need the legacy writes:

- full-JID delivery (`deliver_one_via_actor_at`, `deliver_peer_to_live_only`
  in `server/routes/interpret/routing.rs`), which asks the `UserActor` for
  the resource's endpoint;
- the inbox-update push and the group-DM leave
  (`groupchat_archive.rs`, `admin/channels.rs`);
- stream-management promotion (`sm_promotion/live.rs`);
- the cluster route bridge's receiver, registration snapshots and
  side effects (`clustering/route_bridge`);
- direct sends through the connection registry: presence probe answers and
  directed presence, block-list pushes, MUC invites, PubSub fan-out, Muji;
- connection cleanup and registration, pending-delivery flush and
  recovery, the extension host's presence query and the admin channel
  command.

The registry's presence state, last activity and pending subscription
stanzas are still written by the presence handlers for the same readers.

## The binding publisher

A connection publishes its bindings through one trait, `BindingPublisher`
(`W/connection/binding.rs`, `W` being
`waddle-server/src/server/routes/websocket`). Two publishers implement it:
`RegistryBindingPublisher` (`W/registration.rs`), which writes the
connection registry and its mirrors, and `CoordinatorBindingPublisher`
(`account_coordinator/publisher.rs`, trait implementation in
`W/connection/coordinator_publisher.rs`), which maps the steps onto the
coordinator:

| Step | Coordinator calls |
| --- | --- |
| `publish` | `slot_status`, `Bind`, `PublishEndpoint` |
| `publish_resumed` | `slot_status`, `SuccessfulResume`, `PublishEndpoint` |
| `confirm` | `slot_status`, `RegisterActor`, `bind_success` |
| `withdraw` | none: the coordinator holds no outbound endpoint |
| `retire`, `retire_remote` | `unregister`, then `repair` with the connection's teardown evidence |

Each step reads the slot first and skips what the binding's state shows as
done, so a repeat after a lost reply is safe.

The trait is shaped by what the coordinator needs:

- `publish` is asynchronous and fallible. It answers published, a pending
  replacement naming the displaced `BindingRef` and the slot revision, or a
  typed refusal; an error means the outcome is unknown.
- `publish` takes the `BindingRef` and the resource's `ResourceLifetime`.
  The outbound endpoint belongs to the transport index, so the registry
  publisher is handed the entry it registers when it is constructed.
- `Published` is `Clone`: the coordinator's value is the `BindingRef` with
  the claim identity and revision from `slot_status`.
- A resumed binding is published by its own step, which carries what the
  publisher needs about the committed resume (for the coordinator the
  `SuccessfulResumeReceipt` and the previous `BindingRef`), and is confirmed
  like any other. `confirm` answers confirmed with a receipt, not current,
  or refused.
- Retirement has three steps. `withdraw` takes the binding out of the
  routing index and returns what the index held. `retire` and
  `retire_remote` take the custody and teardown dispositions and report
  whether the binding was retired, was not the current one, or was refused.

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
  published or active binding whose connection is live. Liveness has three
  answers. A connection under this node's current lease is live. A binding
  hosted by another node is judged through its stream-management session's
  claim, the only node-lease read the claim store exposes: when the node
  the connection names holds that claim, a fresh lease means live and a
  lost lease means expired. Everything else is unknown, including a remote
  binding without a session. On a single node a connection of a previous
  process is expired. An expired binding is reported and can be retired
  without its connection; an unknown one is kept, is not a destination and
  is not retired on that evidence. A detached binding is never adopted:
  the slot model has no adoption for one, so a session detached under a
  previous claim cannot resume.
- **Over the slot bound.** Rows that hold a binding are always loaded,
  even beyond the account's slot bound. Such an account serves its stored
  bindings and refuses a bind of any new resource until enough retire.
- **Presence payloads.** A row stores the presence extension payloads of
  its snapshot as one XML document, within the snapshot's bounds on their
  number and size, so a coordinator that reloads an account still answers
  a probe with what the resource announced.
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
