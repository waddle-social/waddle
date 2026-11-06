# P2.1 pinned attestor and receipt authority

Status: **plan; no serving selector**. This grouped continuation follows the
dark catalog, ledger and SM lock-order preparation merged in #2238. The base
is main `8452ad37`. The first target remains a PostgreSQL 17 server compiled
with clustering and configured for one runtime node.

## Boundaries

The current pool hooks invoke the physical attestor before an independent
catalog proof, and the V1030 dark receipt profile grants the runtime whole
journal SELECT. Neither path authorizes restricted serving. The source
function, catalog provider, operator handoff, role administrators and old
backends are separate trust boundaries. A cooperative advisory lock cannot
fence an uncooperative owner or a function replacement by privileged DDL.

The grouped work will retain `UnconvertedStores` until the complete role,
epoch, old-backend and all-store startup graph is proven. The opt-in APIs may
be compiled and tested without wiring them into the pool hooks or listeners.

## Packages

1. **Opt-in pre-attestor proof.** Require an independently supplied pinned set
   for the target and trusted schemas, their owners, runtime/installer/definer
   roles, `_lineage` ordinary heap and primary index, attestor function and
   installation operation. In one bounded READ COMMITTED transaction, set a
   catalog-first path, take shared lineage before relation locks, inspect
   exact catalog/ACL/function source and bounded expression data, `LOCK TABLE
   ONLY` the proven heap, repeat the check after any wait, and invoke the
   proven function on that same transaction. Reject hostile views, casts,
   operators, compressed oversized values, function replacement and stale
   OIDs before execution. Preserve the independently enforced privileged
   mutation/quiescence requirement. This dark entry does not protect existing
   pool hooks, installer preparation or startup.
2. **Exact-key V1030 journal readback.** Add one A-owned SECURITY DEFINER
   lookup by immutable operation ID with a fixed `pg_catalog, pg_temp` path.
   Grant R only EXECUTE; remove R's whole-journal SELECT from the experimental
   profile and call the lookup through the closed receipt authority. Pin its
   function OID, source, owner, search path and ACL. A missing row remains
   indeterminate, not rollback proof. Keep admission and retention bounds as
   separate serving gates.
3. **Role and handoff map.** Reconcile the target S/R/A/H/T/I/M/migrator graph
   with current store grants and the PubSub helper. Record the exact
   independently provisioned operator pins and grant changes required before
   a pool-hook or startup switch. Distinct installer and migrator LOGINs must
   have SET-only memberships in T and S respectively. The future transition
   must fence old login/reconnection and existing backends on the authoritative
   primary; an uncertain COMMIT retains its original backend/primary fence
   until positive reconciliation. No rollback assumes an old binary can
   restart against V1030.

## Acceptance for this dark group

- PostgreSQL 17 positive and hostile tests exercise the exact opt-in path,
  lookup confidentiality, catalog drift, replacement waits, compressed and
  oversized inputs, unknown-COMMIT readback and rollback/pool reuse. Missing
  PG prerequisites fail the required local test lane.
- Verify formatting, the relevant strict Clippy scope and diff/structure
  checks, and obtain an independent read-only review of the exact combined
  diff. Distinguish unchanged baseline diagnostics from new failures.
- Keep the production selector, current pool hooks and store startup refusal
  unchanged. Do not claim a live role transition, selected-path integration,
  WebSocket/XEP qualification or release CI from dark tests.

The later serving package must integrate the pre-attestor check before every
pool hook and installer read, supply a versioned all-store role/ACL matrix and
bounded journal retention, convert guarded callers to one typed main-pool
authority, and prove old-backend cutoff, durable SM writer/pump recovery and
full startup. The wider #2046 checklist remains open until its release and
activation gates pass. GitHub Actions and workflow files stay disabled until
the requested final `cuenv sync -A` restoration.
