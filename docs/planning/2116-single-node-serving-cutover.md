# Single-node P2.1 serving cutover

Status: **integrated implementation plan; no serving selector**. This follows
the dark pinned catalog proof in #2242. The first target is a PostgreSQL 17
server built with clustering support but configured for one runtime node. The
current `UnconvertedStores` refusal remains until every gate below passes.

## Trust and rollback boundary

The runtime application code and its credential are trusted to call only the
typed API. Database owners, role administrators, and the physical attestor
are separately controlled. Their mutation protocol must hold lineage,
migration-ledger, then epoch locks in the reviewed order, or prove independent
quiescence. Epoch `ACCESS SHARE` does not stop an uncooperative privileged
mutator. The complete `_lineage` and attestor dependency graph needs a
bounded, independent catalog preflight **before the pool's after-connect and
before-acquire attestor hooks run**, under the trusted mutation policy.

V1030 makes an old binary reject the ledger. The cutover therefore fences old
login and reconnection, drains and verifies old backends on the authoritative
primary before changing roles, verifies prepared transactions are disabled and
none remain (or positively reconciles and fences each relevant prepared
transaction), and proves the journal empty and trusted while the owner holds
the transition barrier. An uncertain owner COMMIT retains its
durable operation identity and original-backend/primary fence until positive
reconciliation or a proven terminal outcome; an absent marker or dropped socket
alone is not rollback proof. No rollback plan assumes an old binary can restart
against V1030.

## Ordered packages

1. **Catalog and lock entry.** After the independent pre-attestor check, on
   one exact attested main-pool connection begin bounded READ COMMITTED before
   a snapshot, take shared lineage then shared migration-ledger, and perform
   catalog-only preflight of the pinned epoch heap before any name-based
   `LOCK TABLE ONLY ... ACCESS SHARE`. Recheck its OID/shape after the wait.
   Catalog checks must prove pinned ordinary `ONLY` heap relations, built-in column
   types, owner, ACL, dependencies, rules, triggers, RLS, inheritance, and
   callable functions before reading epoch, `_migrations`, observation,
   PubSub, or guard-marker data or invoking the PubSub helper. Recheck after
   each relevant wait with fresh statements. Bound catalog inventories and
   variable-length trigger, function and expression data before aggregating
   or deparsing them; a catalog-only query can still exhaust client memory.
   Factor V1023 ledger verification under a lexical caller-held shared-ledger
   proof; it must not upgrade to exclusive after epoch. Hostile view, cast,
   operator, child-lock, and catalog-replacement witnesses must fail before
   execution.
2. **One provisioned role and API graph.** Independently pin schema owner S,
   runtime LOGIN R, API owner A NOLOGIN, PubSub helper owner H NOLOGIN,
   attestor owner T, a separate attestor-installer LOGIN I with SET-only T
   membership, monitor M, and a distinct migrator with SET-only S membership.
   Reconcile ingress, observation, MUC,
   PubSub, MAM, claim, Push, notification, room, and outbox privileges as one
   versioned matrix. H owns a fixed nonrecursive `LOCK TABLE ONLY` helper and
   only the exact table rights needed for its lock mode. Remove or make
   noncallable the legacy schema-owner helper before selecting R. Replace
   runtime-wide V1030 journal SELECT with a pinned exact-key definer lookup;
   bound journal admission and retention without losing unknown-COMMIT proof.
   This lookup narrows enumeration; possession of another operation UUID by R
   is not a per-user authorization boundary.
   Resolve V1009's invoker `FOR SHARE` epoch lock in the selected API-owner
   execution context; do not grant epoch `UPDATE (id)` to R or treat the dark
   relation-guard candidate as the selected role profile.
3. **Typed transaction and operation identities.** A non-cloneable authority
   binds the exact pool, checked profile, source occurrence, attempt and
   immutable result. It exposes fixed operations, no raw transaction, SQL
   callback, or pool escape. The 13 epoch-guarded tables and every locking
   read or writer in ordinary ingress, pooled receipts, alias/GC/recovery,
   notification/Push descendants, and room/MUC children must use it. Mint a
   stable source occurrence synchronously and transfer immutable input to
   supervised ownership before the first cancellable await. Do not acknowledge
   upstream acceptance until custody is durably recorded or positively
   reconciled; a crash before custody needs retry of the same occurrence.
   Originless non-resumable input without that retry identity fails closed.
   A missing readback is never rollback proof; no external effect is retried
   from receipt evidence alone. Non-receipt transitions need their own durable
   result and reconciliation protocol.
4. **SM source and recipient interlock.** Define a fenced, durable single-node
   claim/session authority and restart recovery; the clustering-disabled
   `InProcessClaimStore` cannot satisfy the candidate's database claim check.
   Uniquely publish one endpoint/pump with hydration CAS before routing or
   exposing replay. Every live, relay, batch and detached frame must carry a
   stable frame occurrence identity, nonwrapping absolute ordinal, immutable
   payload, complete source set, claim, binding, session and generation before
   counting or accepting memory work. Use a persisted route guard per canonical
   source and bare recipient, covering all effect kinds for that recipient.
   One reviewed lock order covers principal, exact claim, sorted sources,
   sorted recipient route guards, sorted session gates, attempts, slots and
   terminals, then pending quota and journal. Provisional reads discover the
   full lock set; re-read it under the locks and retry on change. ACK must read
   a bounded candidate prefix before locks, acquire its sorted source/route
   set before the gate, then revalidate the entire contiguous prefix.
   Convert the existing gate-first Rust paths and SQL admission/handoff
   triggers together so no gate-to-canonical inversion survives.
   Preserve contiguous ordered exposure/replay and a durable outstanding window
   smaller than 2^31 before validating a wrapping XEP-0198 ACK. Quiesce the
   exact published pump and in-flight socket output outside the DB transaction,
   then re-enter and revalidate sibling inventory before pending acceptance.
   ACK, transfer, tombstone, late counted drains, pending and canonical GC
   share this fence and retain exact terminal evidence after row deletion.
   Give persistent route guards an enforced quota and safe retirement rule.
   Unknown provenance remains ineligible until positively classified. Drain
   and fence old sockets, queues, raw writers and existing backends before
   selecting the new sink. A relay-envelope shape change needs its mandated
   remote-message version bump.
5. **Whole startup graph.** Attach every enabled store to the attested main
   pool without owner DDL or child URLs. Provide explicit safe single-node MAM
   and durable SM claim paths when clustering is compiled but disabled at runtime. Keep listeners
   unbound and effectful workers stopped until role-backed attachment, recovery and old-backend
   cutoff pass. The first selected slice excludes clustered serving and
   arbitrary-node resume.

## Acceptance evidence

Run PostgreSQL 17 least-privilege and hostile-catalog tests on the actual
selected path, SQLite semantics where applicable, native independent-process
unknown-COMMIT and late-writer races, and a real single-node startup/WebSocket
and XEP-0198 ordered-exposure/wrapping-ACK qualification with every enabled
store. Prove old-login/backend/primary cutoff and
failed-transition recovery. Review the exact integrated diff independently.
The wider #2046 release gate still requires its fault/workload matrix, removal
of replaced machinery, restored workflows via `cuenv sync -A`, and exact
release CI after implementation is complete.

Unresolved catalog, role, source, lock-order, or recovery findings block the
affected selector. A dark fixture or schema check alone does not authorize
serving.

## Next grouped implementation after #2243

The #2243 role graph, pinned MAM read, and recipient route guard are merged as
unregistered candidates. The next group can implement these independent dark
packages while both startup refusals stay closed:

1. **Versioned all-store role profile.** Reconcile the H-owned PubSub helper,
   A/H schema usage, room observation, MUC room, and every enabled store under
   one independently pinned S/R/A/H/T/I/M/migrator function and ACL graph.
   Exercise the real combined PG17 owner-prepared schema with negative
   alternate-definer, PUBLIC, membership, grant-option, TEMP, and catalog-drift
   cases. The legacy detached SM ingress writer is incompatible with R's
   guarded-table denial; a profile that leaves it live cannot be selected.
2. **Catalog mutation and pre-attestor fence.** Define an enforceable
   all-privileged-mutator protocol, or prove their quiescence, before a pool
   hook can execute an attestor. Cover the complete bounded lineage/attestor
   dependency graph and every concurrent-index maintenance transaction from
   start through reader rollback. A heap or index ACCESS SHARE lock alone does
   not prevent `DROP INDEX CONCURRENTLY` or `REINDEX INDEX CONCURRENTLY` from
   changing a pinned index identity. Test a cooperative multi-session barrier
   and refuse startup when the external ownership premise is not established.
3. **Production source and recipient interlock.** Inventory and carry stable
   source occurrence, complete sibling/source sets, and immutable payload
   through live, relay, batch, detached, drained, snapshot, ACK, transfer,
   tombstone, pending, and GC paths. Move every participating writer to one
   source → per-source guard → recipient guard → gate lock order. Add a guard
   quota and safe retirement rule. Test both race winners and retain counted
   payload and terminal evidence on refusal. No pending acceptance or GC
   selector may use a partial conversion.

Selected startup remains a later integration gate. Cold bind, inherited
listener traffic, port-file publication, effectful workers, and the single-node
durable claim must stay gated until every package composes on the actual
clustering-compiled single-node path. The durable exposed-window limit must
match the XEP-0198 ACK validator's current half-range rule before replay is
selected.

## Grouped selected-boundary work after #2244

#2244 merged opt-in A/H store diagnostics, a test-support cooperative catalog
fence, and a test-support SM recipient-route laboratory. None is selected by
startup or a production writer. The next grouped branch targets the functional
single-node boundary; its draft state is a work ledger, not activation proof.

1. **Privileged catalog and role boundary.** Inventory every privileged DDL,
   ACL, function, index and migration mutator, including concurrent index
   maintenance and pre-attestor pool hooks. Require a nonblocking shared-reader
   protocol that cannot hold a stale transaction snapshot while maintenance
   waits. Deny or durably quiesce any old login, backend, prepared transaction
   or mutator that cannot participate. Pin the complete bounded attestor graph
   before its first privileged read. Refuse restricted startup unless the
   external ownership and mutation premise is independently established.
2. **Whole-store runtime role and transaction boundary.** Reconcile the
   S/R/A/H/T/I/M/migrator role and function graph, the V1009 A-owned
   `UPDATE(id)` path, schema/table/column/default ACLs and every enabled
   store. Attach only owner-prepared stores to the attested main pool in one
   READ COMMITTED transaction authority. Preserve positive-only recovery for
   an uncertain COMMIT and forbid raw R DML on guarded tables.
3. **Complete recipient and SM custody boundary.** Replace every live, relay,
   batch, detached, drained and snapshot producer and every ACK, transfer,
   tombstone, pending and GC consumer with a single complete-source,
   recipient-wide lock order. Persist the frame occurrence and sibling set;
   conservatively refuse unknown provenance. Quiesce the published socket
   pump before accepting pending. Prove both winner orders with different
   canonical sources, retained counted payload on refusal and a durable
   terminal after consumption. Reconcile the replay window with the ACK
   validator's half-range rule.
4. **Selected single-node startup and qualification.** Keep both existing
   refusal gates closed until the preceding components compose on the actual
   clustering-compiled single-node path. Gate cold and inherited listeners,
   port-file publication and effectful workers behind the durable claim,
   complete store attachments, reconciliation and recovery. Then exercise
   real PostgreSQL/WebSocket/XEP-0198, failed transition, restart and fault
   schedules before selecting serving.

An independent architecture review must challenge this plan before selected
implementation. Every merged intermediate commit must either remain dark or
prove the full selected path; a partial store or writer conversion cannot
relax either startup refusal. GitHub Actions and workflow files stay disabled
until the whole #2046 implementation is finished, then `cuenv sync -A` and
exact-release CI are required.

## Dark #2245 boundary limits

The v2 A/H/MAM/effect profile is an opt-in partial diagnostic. It neither
registers a serving profile nor reconciles every enabled store and definer
chain. Its MAM inspection uses a separate transaction and is not one atomic
all-store receipt; privileged mutators and MAM writers must be quiescent.
The same-session catalog-fence observation holds its shared advisory
lock from before the pinned attestor's reads through acknowledged rollback;
it does not enter the pool hook. The attestor also takes a blocking lineage
lock, so a mutator with an incompatible catalog/lineage lock order can still
create a cycle or force a bounded failure, even if it uses the fence. Every
privileged mutator and old backend must participate with a consistent order or
be independently quiesced before a selected startup can rely on this fence.

The SM snapshot laboratory owns its transaction through a complete apply,
but its caller must supply an authoritative captured occurrence or generation
before a stale input can replace a newer durable frame. Borrowed append and
promotion interfaces require the caller to roll back on cancellation; an
interrupted partial write can leave untracked state that conservatively
blocks pending acceptance. The candidate is not wired to the production
writer, socket, pending selector, or GC. The durable custody candidate now
limits `appended - peer_acked` to less than 2^31, matching the live validator,
but replay selection and restart qualification remain open.

## Next grouped composition after #2245

This branch groups the remaining #2116 single-node selection prerequisites.
The accepted architecture gate may narrow or reorder the packages before
implementation. Every intermediate change stays dark while a selected-path
proof is incomplete; both restricted-startup refusals remain closed.

1. Establish one enforceable privileged catalog mutation protocol and a
   same-physical-backend reader fence from before the first attestor read
   through protected operation completion. Every old backend and privileged
   DDL, ACL, role, function, migration and concurrent-index mutator must
   participate or be independently quiesced. Resolve the catalog/lineage
   lock order and prove bounded failure cannot mint a serving receipt.
2. Compose independently pinned S/R/A/H/T/I/M/migrator authority with every
   enabled store on the actual clustering-compiled single-node main pool.
   Reconcile V1009, MAM, room effects, PubSub/MUC/observation, Push,
   notification, spaces, links, inbox, SM, pending and GC paths. Check the
   exact role/function/table/column/default ACL graph under one transaction
   and return serving handles only after its positive commit/reconciliation.
3. Give each SM frame an authoritative durable occurrence or generation so a
   stale caller snapshot cannot replace a newer committed frame. Convert
   live, relay, batch, detached, drained, ACK, transfer, tombstone, pending
   and GC paths to the complete-source recipient-wide lock order. Own every
   transaction through mutation and reconciliation, including cancellation
   and unknown COMMIT. Cut off old raw writers and old socket output before
   selecting the new sink.
4. Open restricted startup only after the preceding proofs compose with a
   durable single-node claim, old-backend cutoff, cold/inherited listener
   publication fence and failed-transition recovery. Qualify the exact path
   with PostgreSQL 17, native independent-process races, real WebSocket and
   XEP-0198 wrap/replay, restart and fault schedules. Keep the wider #2046
   release matrix and replacement deletion as separate completion gates.

The single-node claim gate needs a database-enforced deployment singleton
slot; independent processes can otherwise register different node IDs.
Current registration can replace a live same-ID epoch, fresh acquisition can
proceed without a live node row, and existing ingress/SM/MUC write fences
check only the claim row. Do not wrap that store as selected singleton
authority. Define and test a node-slot, lease, claim and durable-write lock
order with post-wait freshness, bounded heartbeats and exact old-backend
reconciliation. Test a nested write while heartbeat waits before adopting a
node-row lock protocol; its liveness is not yet established. Reconcile
claim-only orphans and unknown registration/takeover commits before serving.

A snapshot revision checked only by the dark candidate does not fence legacy
append, ACK or deletion, nor a local frame counted after capture. Production
selection requires every competing path to advance the same authority and
the actor to serialize capture with outbound publication. An uncertain
snapshot commit freezes that session for bounded reconciliation; exhaustion
closes the stream rather than leaving an ACK request pending indefinitely.

The final PR description must match the exact integrated scope and evidence.
GitHub Actions and workflow files stay disabled while #2046 implementation is
open; restore them with `cuenv sync -A` and run exact-release CI only when the
entire issue is ready.

## Grouped serving-authority work after #2246

#2246 merged dark same-backend catalog-fence and snapshot-revision labs. Neither
is a production selector. This grouped branch develops unselected candidates
for the singleton claim, SM revision participation, and pinned catalog read.
The following steps are the remaining selection plan, not a claim that the
selected serving path is complete. Both restricted-startup refusals remain
closed until the exact combined path passes its acceptance gate.

1. Specify and implement a database-enforced deployment singleton slot, node
   lease, claim and guarded-write protocol. Registration may not replace a
   live same-ID epoch; acquisition needs a live node and post-lock freshness.
   Prove lock order and liveness under concurrent heartbeat, takeover and
   nested writes. Reconcile unknown commits and orphaned claims before serving.
2. Make production SM capture, append, full snapshot, ACK, deletion, pending
   and GC advance one durable session revision. Serialize actor capture with
   outbound publication, retain exact generation/occurrence and positive-only
   unknown-COMMIT readback, and cut off legacy writers before selecting the
   new path. Prove both race winners on PostgreSQL and SQLite where applicable.
3. Compose the selected claim and SM authority with the main-pool catalog
   fence, exact runtime role graph and every enabled store. Keep the old
   startup gates closed until trusted mutator/old-backend cutoff, complete
   store attachment, cold/inherited listener publication and recovery have
   a single integrated proof. Do not infer selected safety from a dark lab.
4. Run focused PostgreSQL 17 and native WebSocket/XEP-0198 tests plus the
   required server gates on the exact proposed SHA. Obtain independent
   architecture and final-diff reviews. Update this plan and the issue
   checklist to match what actually lands. Keep workflows absent until all
   #2046 work is ready for `cuenv sync -A` and exact-release CI.

### Candidate boundary in this grouped branch

- The owner-prepared singleton slot and lease candidate is unselected. Its
  PostgreSQL tests prove local registration, heartbeat, takeover, and guarded
  transaction schedules. Selection still needs bounded holder/backend
  lifetime, composition with every durable writer, old-backend cutoff, and
  claim recovery on the actual serving path.
- The revisioned SM storage candidate has an exact operation receipt and
  positive readback. It refuses unsupported tombstone, prune, and
  acknowledgement-confirmation operations. Legacy append, ACK, snapshot,
  pending, GC, and actor publication still need a joint conversion; the
  production SM selector remains unchanged.
- The pinned catalog candidate exposes only a closed read-only epoch
  observation. It does not provide receipt writes or ordinary units of work.
  Selection needs a provisioned writer authority, cooperative catalog
  mutator/old-backend cutoff, and the full same-transaction role and epoch
  contract. No startup path selects any of these three candidates.

## Selected authority work after #2247

#2247 merged the candidates above without changing the serving selector.
The next grouped implementation must join them at a single database effect
boundary before either restricted-startup refusal can be removed.

1. Specify the runtime role and callable-function boundary. At the #2247
   baseline, R had 67 direct ingress API EXECUTE grants, including underlying
   receipt writers that bypass the P21 operation journal. This branch's dark
   P21 role transition revokes the two unjournaled receipt writers; the
   remaining callable ingress surface still needs a selected-path proof. A
   Rust-only wrapper around one receipt function cannot close this surface.
   Make every permitted write
   entry enforce the same authority or prove an exact old-login/backend
   cutoff and closed caller set. Verify owner, function, relation, trigger,
   grant, role-membership, journal and epoch pins on the serving backend.
2. Bind the deployment singleton slot, live node incarnation, claim, catalog
   fence, protocol epoch, SM revision and effect receipt to one bounded
   transaction. A separate long-lived guard transaction cannot authorize a
   later mutation on another backend. Bound statement, lock, idle and total
   lifetime; close uncertain backends and reconcile exact operation identity
   after an indeterminate COMMIT. Do not infer rollback from a timeout.
3. Convert every competing live, detached, batch and scheduled writer and
   their pending/GC/ACK/tombstone paths to that boundary. Serialize actor
   capture with outbound publication and define positive settlement for every
   retained custody interval. ACK validation needs a durable contiguous
   authorized write-attempt frontier; that frontier is not proof of peer
   receipt. Cut off old-role/raw writers and transports before selection.
4. Prove the combined role, claim, catalog, SM and all-store path on the
   actual main pool with PostgreSQL 17 independent-process races, SQLite
   compatibility, native WebSocket/XEP-0198 and exact startup/rollback
   schedules. Only then open the restricted-startup gates. Keep workflows
   absent until #2046 implementation is ready for `cuenv sync -A` and
   exact-release CI.

### Dark hardening boundary in this grouped branch

- The singleton candidate bounds a returned slot guard with PostgreSQL idle
  and total transaction deadlines, and identifies an unconfirmed rollback's
  original backend/transaction. Registration, renewal, revocation and a
  cancelled acquisition still lack a complete bounded-backend reconciliation
  contract. This guard cannot authorize a writer on a different transaction.
- The revisioned SM candidate records generation-local acknowledged, exposed
  and appended ordinals. It refuses ACK beyond the authorized write-attempt
  frontier and refuses a duplicate wrapping settlement-window key before
  deleting replay rows. Exposure is not peer receipt. Old writers, exact
  source/recipient claims, terminal settlement, absolute-ordinal ACK windows,
  actor publication and ambiguous COMMIT recovery remain unconverted.
- The P21 role candidate gives its common ingress helper a transaction-scoped
  catalog fence, removes R's two unjournaled effect-receipt insertion routes,
  and checks helper/function identity and denial in its dark receipt pilot.
  The pilot still uses the legacy attested transaction entry rather than the
  pinned selected pool; trusted catalog-mutator cooperation, server-side
  transaction lifetime and old-backend cutoff remain prerequisites.

No production selector or restricted-startup refusal changes in this group.
