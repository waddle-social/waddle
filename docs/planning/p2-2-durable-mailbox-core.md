# P2.2 — durable mailbox core: design gate

Status: **proposal for review; not implemented or accepted**. This document
sharpens [#1986](https://github.com/waddle-social/waddle/issues/1986); it does
not authorize schema, repositories, allocation, a relay cutover or production
activation. Its code references are to `main@8ecc94bb` unless otherwise noted.

[#2116](https://github.com/waddle-social/waddle/issues/2116) is a hard
prerequisite for mailbox DDL and executable storage work. Its composable,
main-pool transaction authority and migration-owner/epoch-guard boundary are
not complete. The [#1986 gate](https://github.com/waddle-social/waddle/issues/1986#issuecomment-6562336267)
prohibits registering mailbox DDL or allocating through a raw or parallel pool.
Neither the existing cloneable `IngressUnitOfWork` nor an isolated dark receipt
operation establishes that prerequisite. Re-review this design against the
actual #2116 interface and guard migration when they are ready. The three
contract questions below are also **BLOCKING** before mailbox schema or
repositories: the test matrix does not resolve them by itself.

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
