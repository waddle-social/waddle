# P2.1 closed pinned Push-family profile

Status: grouped dark implementation plan for #2116 under #2046. The selected
serving path and both restricted-startup refusals remain closed.

## Plan

1. Extend `PinnedSelectedEntry` with a fixed read-only, rollback-only profile
   operation on its existing one-backend PostgreSQL transaction. Acquire the
   catalog-S fence before the attestor, complete V1030 ledger, epoch and guard
   checks. Preserve the same bounded checkout, cancellation and acknowledged
   rollback contract.
2. Pin independent S/R/A/H and H-function identities, the existing twelve
   PubSub relations, and an explicitly versioned Push, VAPID, registration,
   notification outbox and activity relation/ACL inventory. Refactor the
   existing H-aware PubSub verifier so the pinned SQLx path and legacy adapter
   share its catalog checks. Bound the namespace, database, parameter,
   relation, attribute, trigger, constraint, function, and ACL inputs read by
   that verifier before either pass. Revalidate before invoking the exact
   H-owned `NOWAIT` lock helper. Choose the compatible profile with no
   effective monitor SELECT on the H-verified PubSub tables.
3. Exercise the actual owner-prepared PostgreSQL 17 combined fixture on a
   one-slot pinned pool. Refuse wrong role/function/relation OIDs, body,
   search path, missing or excess grants, incompatible monitor rights,
   catalog/helper contention and canceled or unconfirmed observations. Check
   backend identity and acknowledged rollback on the positive path.
4. Run focused PostgreSQL 17 tests, formatting, strict scoped Clippy and an
   independent exact-head review. Keep the plan and PR claims aligned with
   the exact integrated evidence.

The method returns no pool, connection, transaction, store handle, source row,
or custody verdict. It does not read `ingress_effect_descendants`, add an
A-owned classifier or R grant, call the legacy joint custody reader, select
workers, or change XMPP wire behavior. All mutations of catalogs scanned by
the shared H verifier, including temporary-schema creation by other sessions,
and old backends require independent quiescence from before the pool hook
until rollback; a discarded checkout alone does not prove backend exit. GitHub
Actions and workflow files stay disabled until all of #2046 is implemented.
