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
   Preserve contiguous ordered exposure/replay and a durable exposed window
   smaller than 2^32 before validating a wrapping XEP-0198 ACK. Quiesce the
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
