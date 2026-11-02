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
actual #2116 interface and guard migration when they are ready.

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
   than allocating another. Proposed range: zero for an empty counter and
   positive, nonwrapping `u64` sequences through `18446744073709551615`, with
   an explicit exhaustion error. PostgreSQL storage must represent and check
   that entire range; do not narrow it silently to signed `BIGINT`. Confirm
   that gap-free committed allocations are worth the lane lock and specify the
   treatment of deleted entries and cursors before implementation.
3. **Reference granularity.** Every entry refers to a canonical `MessageKey`
   and an exact existing effect or delivery identity, with a closed typed
   variant distinguishing their roles. The origin's acceptance request can
   reference its canonical dispatch effect before the owner has frozen a room
   manifest. The owner then freezes the manifest and child keys; child work
   refers to the exact delivery keys, never an audience rediscovered during
   consumption. Require message/reference/target agreement; a delivery key
   alone is not proof that it belongs to the supplied message. Decide the exact
   acceptance-request and child variants against #1948 before coding them.
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
- An **entry** is uniquely identified by lane and positive sequence. It stores
  the canonical message and exact effect or delivery reference, its typed role,
  and one disposition. It stores no stanza or duplicate frozen manifest.
  Foreign-key and repository checks must establish the same message and exact
  reference, without a cascade that can remove live custody. In particular,
  `ingress_deliveries` keys a delivery separately from its message; a lookup or
  foreign key on the delivery key alone must not authorize a mismatched pair.
- A bounded exact-operation identity is known before attempting commit and is
  independent of the chosen lane. Propose a unique index on the entry itself
  over its role, message and exact effect/delivery identity, plus any canonical
  discriminator needed to distinguish legitimate separate obligations. Bind
  that operation immutably to its lane and target; an attempted retry on a
  different lane must conflict, not allocate another entry. A lane-scoped index
  alone cannot enforce this. Use fixed-width keys or a bounded digest with
  full-value equality and collision failure; handle nullable variant columns
  explicitly so SQL null uniqueness cannot admit duplicates. Different child
  deliveries need their own canonical keys, not invented mailbox identities.
  Validate each reference against its recorded canonical obligation before
  appending, and fail closed on invalid stored discriminants. Resolve each
  operation's uniqueness scope during the design gate. A separate bounded
  target-first lookup supports later polling; its layout is subject to P2.4.
- Dispositions are closed: pending, accepted, consumed, dead-lettered and
  policy-discarded. The review must define each legal transition and its exact
  actor, compare-and-set fence and proof. A dead letter retains recoverable
  custody until an authorized repair or final policy disposition; it is not
  automatically successful delivery. Advancing a cursor past it requires an
  explicit ordering and policy decision. Terminal transitions cannot erase
  the evidence required to resolve a prior acceptance while retry is possible.

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
   responsibility together. For a room, the owner checks its exact current
   claim and occupancy/lifecycle authority and, in one authoritative
   transaction, freezes the final canonical manifest, authoritative room stanza
   ID and complete child keys and changes the existing request's disposition to
   accepted. The row refers to that canonical evidence; another table, relay
   ACK or in-memory flag must not independently declare acceptance. A rejected
   request cannot fabricate an accepted manifest. The origin advances `h` only
   after it can positively verify this committed authority. The actual
   acceptance transaction and wire behavior are #1948/P2.3 work.
2. **Uncertain commit:** the stable operation identity is available before the
   write. After a lost reply or indeterminate commit, a successful authoritative
   lookup of the exact identity and matching canonical evidence can recover the
   same accepted result. An absent, unavailable or conflicting lookup is not
   positive commit proof and cannot advance `h` or justify replacing the
   manifest. Retry and refusal semantics must be specified with #1948.
3. **Consumption:** the target checks the exact current claim and row state;
   its transaction commits the disposition with the corresponding canonical
   receipt or durable same-key downstream custody. A socket write or relay ACK
   is not a durable SM append. Stale owners, changed targets and late responses
   cannot retire successor obligations. The existing canonical receipts remain
   the effect-completion authority.
4. **Retention:** adding, settling or removing mailbox custody and the existing
   collector synchronize on the canonical message. A live or repairable entry
   keeps the message, identity and evidence available. Once every canonical
   obligation, SM reference and retained mailbox entry permits collection,
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
| Idempotency — P2.2 | Race and retry the same pre-known operation, including concurrent attempts on different lanes; only one row and sequence commit, and a different lane or target conflicts. Different content, role or reference cannot masquerade as that operation. Legitimate distinct child keys remain distinct. Test maximal JIDs, every lane discriminator, encoding round trips and an injected digest collision that fails closed. |
| Fences and handoff — P2.2/P2.3 | Change origin and target owner epochs and node incarnations while an entry is pending and while acceptance or consumption is paused. The successor uses the stable lane, and stale updates affect zero rows or return a typed fence failure. Include room lifecycle/occupancy changes, multiple-lane lock order, SM resume and join repair that changes origin. |
| Epoch and roles — P2.2/#2116 | Under the reviewed restricted role and real migration, verify both new tables in the manifest and DML/truncate guards. At the activated test epoch, unproven, stale and unsupported-epoch writes fail; properly prepared writes succeed. Exercise owner transition, schema shadowing and migration ordering as required by #2116, rather than inferring those guarantees from an epoch-zero write. |
| Acceptance — #1948/P2.3 | Pause before owner commit and prove the origin cannot advance `h`; then commit the frozen manifest and all child identities before acknowledgement. Change membership around acceptance and retry: recover the original manifest, stanza ID and child keys. Owner rejection, unavailable owner and stale claims never manufacture acceptance. |
| Unknown commit — P2.2/#1948 | Lose the reply or cancel after an owner commit, and recover by the exact authoritative operation lookup without a second row, manifest or allocation. Exercise unavailable, absent and mismatched lookup results; none authorize success. Preserve proof across owner restart and handoff. |
| Consumption — P2.3 | Pause before and after a target transaction commits its exact receipt or durable custody; retry and ownership transfer do not duplicate or retarget the effect. A relay ACK or socket write alone cannot settle the canonical obligation. Pending children may remain after owner acceptance. |
| Cursor and dispositions — P2.2/P2.3/P2.4 | Read bounded pages strictly after the exclusive cursor. Test concurrent appends, retained gaps/deleted entries and every allowed/forbidden transition. A dead letter or policy discard cannot silently skip an ordered obligation; P2.3/P2.4 must establish the applicable advance and repair policy. |
| GC and lock order — P2.2/P2.4 | Race append with collection in both lock acquisition orders and verify either durable custody survives or append fails cleanly, without a dangling reference, deadlock or resurrection. Race settlement/cleanup with GC; retain live and repairable rows and exact positive-lookup evidence, restart the full tail after final settlement, and collect only at its boundary. Test no-cascade and wrong-parent references. |
| Migration — P2.2 | Fresh install and supported upgrade preserve existing data; ledger checksums and guard inventory match; unsupported old binaries and partial installation fail according to #2116's reviewed migration contract. No runtime writer or wire-format change is introduced. |

P2.2 acceptance requires the reviewed choices recorded on #1986, a dark
migration and typed transactional repositories, the P2.2 portions of the real
PostgreSQL matrix, and a retention and lock-order review on the integrated diff.
The P2.3, P2.4 and #1948 rows describe their later contract tests; they do not
move those implementations into P2.2. This document alone satisfies none of the
implementation gates.
