# P2.1 pinned pool-hook and installer transition

Status: **plan; no serving selector**. This grouped continuation starts from
main `600d1af0` after #2239. The first target is PostgreSQL 17, built with
clustering support and configured for one runtime node. The opt-in dark
attestor and exact-key receipt lookup are present, but the current pool hooks
and installer pre-reads still use earlier checks. `UnconvertedStores` remains
fatal before listeners.

## Architecture gate

A checkout proof ends before the borrower starts its transaction. It cannot
authorize a later `_migrations`, epoch or store read, and cannot stop a
privileged owner from replacing the attestor after the hook commits. The
operator must supply pins independently of the database being checked and
enforce a direct authenticated primary, old-primary fence, and a catalog/role/
ACL mutation boundary. The JSON assertion records those prerequisites; it
does not establish them. A cooperative advisory lock alone does not fence an
uncooperative owner.

The installer begins as I and may need to create the attestor, so the current
runtime-only function-present preflight cannot be called there. Separately,
several transaction callers lock later-order relations before their attestor
call. Replacing `attest_connection` mechanically would invert the global
lineage -> ledger -> epoch -> store order or introduce nested transactions.
Unknown installer COMMIT remains indeterminate; the same operation ID and
original backend/primary fence survive until positive reconciliation.

## Grouped implementation sequence

1. **Independent staged operator handoff.** Give the installer a pre-install
   bundle with pinned physical/schema/owner/role and `_lineage` heap/index
   identity plus a durable installation operation ID; it cannot require an
   attestor function OID before the first CREATE. After positive installation
   or same-operation reconciliation on the fenced primary, the trusted
   provisioner captures the function OID and issues a versioned runtime
   document with exact `DarkPhysicalAttestationPins`, independently pinned
   `_migrations` heap/index, epoch heap/index and guard-manifest identities,
   and the same physical/provisioning identity.
   Reject v1, missing, zero, duplicate, mismatched or unbounded fields before
   constructing any attested pool. Bind one immutable bundle to main and
   control-plane pools and to each checkout; do not derive it from R's
   connection. Require the externally provisioned, owner-granted R SELECT on
   pinned `_lineage`, without grant option. Keep authentication,
   authoritative-primary and privileged-mutation enforcement as explicit
   operator gates.
2. **Installer lineage-only proof.** Factor the catalog-only lineage, owner,
   role and ACL checks from the function-present runtime proof. On the exact
   I connection, enter bounded READ COMMITTED, take exclusive lineage then
   installer lock, and prove the independently pinned ordinary heap, role
   membership and builtin dependency graph using catalogs only. I has no
   lineage SELECT, so `SET LOCAL ROLE T`, verify the expected session/current
   role OIDs, then take a name-based `LOCK TABLE ONLY ... ACCESS SHARE` as T;
   recheck the full pinned catalog after the wait and only then read lineage/control
   identity. For a new
   installation, prove function absence under that barrier, create its exact
   source and marker atomically, and hand its OID to R only after positive
   commit or same-operation reconciliation on the fenced primary. Migrate
   both installation and locked observation; absence alone never proves
   rollback.
3. **Pinned connection hooks.** On the exact SQLx physical connection, clean
   any inherited transaction, bound lock/statement/idle/total lifetime, and
   start one READ COMMITTED read-only transaction before the first dependent
   read. Set a safe catalog-first path, acquire shared lineage, run the pinned
   function-present preflight, `LOCK TABLE ONLY` the proven heap, recheck after
   the wait, then invoke the attestor. Confirm rollback before lending the
   connection; hard-close on failed rollback, timeout or cancellation. Clear
   retained TEMP state and set a known application path without reading an
   unbounded inherited path. Preserve stricter existing limits and reject
   a non-session-preserving endpoint. Apply the same proof to `after_connect`
   and `before_acquire` on both pools.
4. **Transaction entry and caller migration.** Audit the 24 current production
   `attest_transaction` call sites plus every direct `attest_connection` call.
   Many store attachments already hold later-order relation locks or use
   REPEATABLE READ; no mechanical helper substitution is safe. A borrower starts its
   own lexical READ COMMITTED proof: shared lineage, pinned heap, shared
   ledger and pinned epoch before any later relation lock or data/function
   read. Actual writers need one bounded READ WRITE transaction with a
   caller-held ledger proof against the complete current V1030 migration
   catalog. The existing V1023 helper is READ ONLY and cannot authorize a
   writer or be run as a separate pre-transaction snapshot; factor its
   catalog-first, post-wait checks into a mode-specific write variant while
   retaining the read-only diagnostic. Bound raw ACL size and compression
   before expanding `_migrations` ACL entries, and test a hostile oversized
   ACL before claiming bounded observation. Rework runtime bootstrap's current
   REPEATABLE READ/name-locked entry,
   MAM, SM, relation-guard and other store paths that currently lock first.
   A `PgLease` caller needs a private in-lease stage; do not open a nested
   transaction or attach an earlier checkout observation as authority. Retain
   the main/control lineage-only readiness probe as an explicitly pinned,
   READ ONLY diagnostic with no ledger, epoch or writer authority; its current
   contract permits a missing latest ledger row. Never label that probe a
   store or transaction proof. Leave any unconverted entrant refused.

## Acceptance for this package

- PostgreSQL 17 tests on actual installer and both hook paths cover v1/missing/
  stale pins, wrong main/control bundle, R missing lineage lock grant,
  hostile views/casts/operators/functions, compressed and oversized catalog
  values, replacement waits, poisoned role/path/TEMP/RR sessions, timeout,
  cancellation, failed rollback, reconnect and pool reuse. A hostile sentinel
  must prove rejection before execution. Installer tests cover concurrent
  same/conflicting operation IDs and a lost actual COMMIT response.
- Verify the exact transaction-entry lock order for every migrated caller,
  with a positive READ WRITE PostgreSQL 17 transaction and stale/hostile
  V1030-ledger negatives rather than treating hook success as transaction
  authority. Run focused SQLite regressions only where touched.
- Run formatting, strict scoped Clippy with documented unchanged baseline
  exceptions, diff/structure checks and independent exact-head adversarial
  review. Review the physical mutation and role handoff contracts separately
  before changing a serving selector.

This package must keep `UnconvertedStores`, all production selectors, live SM
writers and listeners unchanged. The full all-store role/ACL matrix, bounded
journal admission and retention, old-login/backend cutoff, durable SM pump
recovery, selected startup/WebSocket/XEP behavior and release qualification
remain #2046 gates. GitHub Actions and workflow files stay absent until the
requested final `cuenv sync -A` restoration.
