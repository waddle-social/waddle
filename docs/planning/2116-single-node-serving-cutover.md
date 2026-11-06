# Single-node P2.1 serving cutover

Status: **plan; no serving selector**. This follows the dark V1030 receipt
journal and SM recovery work in #2237. The first target is a PostgreSQL 17
server built with clustering support but configured for one runtime node. The
current `UnconvertedStores` refusal remains until every gate below passes.

## Trust and rollback boundary

The runtime application code and its credential are trusted to call only the
typed API. Database owners, role administrators, and the physical attestor
are separately controlled. Their mutation protocol must hold lineage,
migration-ledger, then epoch locks in the reviewed order, or prove independent
quiescence. Epoch `ACCESS SHARE` does not stop an uncooperative privileged
mutator. The complete `_lineage` and attestor dependency graph needs its own
catalog preflight before any executable read.

V1030 makes an old binary reject the ledger. The cutover therefore drains old
processes before changing roles, proves the journal empty and trusted while
the owner holds the transition barrier, and retains a forward-recovery path
for an indeterminate owner COMMIT. No rollback plan assumes an old binary can
restart against V1030.

## Ordered packages

1. **Catalog and lock entry.** On one exact attested main-pool connection,
   begin bounded READ COMMITTED before a snapshot, then take shared lineage,
   shared migration-ledger, and epoch `ACCESS SHARE` in that order. Catalog
   checks must prove pinned ordinary `ONLY` heap relations, built-in column
   types, owner, ACL, dependencies, rules, triggers, RLS, inheritance, and
   callable functions before reading epoch, `_migrations`, observation,
   PubSub, or guard-marker data. Recheck after waits with fresh statements.
   Factor V1023 ledger verification under a lexical caller-held shared-ledger
   proof; it must not upgrade to exclusive after epoch. Hostile view, cast,
   operator, child-lock, and catalog-replacement witnesses must fail before
   execution.
2. **One provisioned role and API graph.** Independently pin schema owner S,
   runtime LOGIN R, API owner A NOLOGIN, PubSub helper owner H NOLOGIN,
   attestor T, monitor M, and migrator. Reconcile ingress, observation, MUC,
   PubSub, MAM, claim, Push, notification, room, and outbox privileges as one
   versioned matrix. H owns a fixed nonrecursive `LOCK TABLE ONLY` helper and
   only the exact table rights needed for its lock mode. Remove or make
   noncallable the legacy schema-owner helper before selecting R. Replace
   runtime-wide V1030 journal SELECT with a pinned exact-key definer lookup;
   bound journal admission and retention without losing unknown-COMMIT proof.
3. **Typed transaction and operation identities.** A non-cloneable authority
   binds the exact pool, checked profile, source occurrence, attempt and
   immutable result. It exposes fixed operations, no raw transaction, SQL
   callback, or pool escape. The 13 epoch-guarded tables and every locking
   read or writer in ordinary ingress, pooled receipts, alias/GC/recovery,
   notification/Push descendants, and room/MUC children must use it. Originless
   submissions receive a stable identity before the first cancellable await.
   A missing readback is never rollback proof; no external effect is retried
   from receipt evidence alone.
4. **SM source and recipient interlock.** Every live, relay, batch and detached
   frame must carry a nonwrapping absolute ordinal and exact source, claim,
   session and generation before counting or accepting memory work. One
   reviewed lock order covers principal, exact claim, sorted sources, sorted
   recipient route guards, sorted session gates, slot/terminal, then pending
   quota and journal. ACK, transfer, tombstone, late counted drains, complete
   sibling inventory, pending acceptance and canonical GC share this fence.
   Unknown provenance remains ineligible until positively classified. Drain
   and fence old raw writers before selecting the new sink.
5. **Whole startup graph.** Attach every enabled store to the attested main
   pool without owner DDL or child URLs. Provide an explicit safe single-node
   MAM path when clustering is compiled but disabled at runtime. Keep listeners
   and workers unready until role-backed attachment, recovery and old-backend
   cutoff pass. The first selected slice excludes clustered serving and
   arbitrary-node resume.

## Acceptance evidence

Run PostgreSQL 17 least-privilege and hostile-catalog tests on the actual
selected path, SQLite semantics where applicable, native independent-process
unknown-COMMIT and late-writer races, and a real single-node startup/WebSocket
qualification with every enabled store. Prove old-login/backend cutoff and
failed-transition recovery. Review the exact integrated diff independently.
The wider #2046 release gate still requires its fault/workload matrix, removal
of replaced machinery, restored workflows via `cuenv sync -A`, and exact
release CI after implementation is complete.

Unresolved catalog, role, source, lock-order, or recovery findings block the
affected selector. A dark fixture or schema check alone does not authorize
serving.
