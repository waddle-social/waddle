# P2.1 pinned attestor and receipt authority

Status: **dark candidates; no serving selector**. This grouped continuation follows the
dark catalog, ledger and SM lock-order preparation merged in #2238. The base
is main `8452ad37`. The first target remains a PostgreSQL 17 server compiled
with clustering and configured for one runtime node.

## Boundaries

The current pool hooks invoke the physical attestor before an independent
catalog proof. The prior dark receipt profile granted runtime whole-journal
SELECT; the exact-key profile here rejects it and requires a pinned lookup.
Neither path authorizes restricted serving. The source
function, catalog provider, operator handoff, role administrators and old
backends are separate trust boundaries. A cooperative advisory lock cannot
fence an uncooperative owner or a function replacement by privileged DDL.

The grouped work retains `UnconvertedStores` until the complete role,
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

## Role and handoff map for the later serving transition

These are distinct PostgreSQL principals, not names inferred from the runtime
connection. The operator must hand the pre-transition identity bundle to the
fencing and attestor verifier independently of the database being checked:
system identifier, database and schema names/OIDs, deployment and lineage
UUIDs, database-owner and S/R/T/I role OIDs, trusted-schema OID, `_lineage`
heap and primary-index OIDs, and the installed attestor function OID and
operation ID. The trusted provisioner then captures `_migrations` heap and
primary-index OIDs and, after V1030 and API promotion, A, epoch, journal table,
record-function and lookup-function OIDs on the same fenced primary. It hands
that second bundle independently to R before the complete profile check; R
must not derive expected pins from its own connection. H, monitor and every
enabled store need a versioned extension to this bundle. The dark tests
provision subsets of this graph; they do not provision it for startup.

| Principal | Intended authority | Required boundary before selection |
| --- | --- | --- |
| S, schema owner | NOLOGIN owner of the target schema and owner-prepared relations | Migrator alone may `SET ROLE` to S; R, A, H, T, I and M have no S membership. |
| R, runtime | LOGIN for the attested main pool | No ownership, role inheritance, schema/TEMP creation, guarded direct DML or whole-journal read. Exact owner-granted SELECT on `_lineage`, `_migrations`, epoch and guard manifest; target/trusted-schema USAGE and attestor EXECUTE; checked store grants and exact ingress-definer EXECUTE. |
| A, ingress API owner | NOLOGIN owner of the fixed receipt and lookup functions | SELECT/INSERT/UPDATE/DELETE on the guarded graph; epoch SELECT and UPDATE(id); sessions and pending-delivery SELECT plus sessions UPDATE(id); journal SELECT/INSERT. R receives lookup and record EXECUTE, with no journal table or column grant. |
| H, PubSub helper owner | NOLOGIN owner of one fixed lock helper | Only the exact table lock rights required by that helper; remove the legacy S-owned callable helper before R selection. |
| T, attestor definer | NOLOGIN owner of the fixed physical attestor | Trusted-schema ownership, bounded lineage read and control-function EXECUTE; no application-store write authority. |
| I, attestor installer | Separate LOGIN and the sole SET-only member of T | May install or inspect the attestor under the reviewed lineage/install lock protocol; cannot assume S or A. No INHERIT/ADMIN option, other direct or transitive T members, or incoming I/R memberships. |
| M, monitor | PostgreSQL `pg_monitor` role in the current ingress profile | Its permitted SELECT grants must be included in the complete role/ACL matrix; R must not inherit monitor authority. |
| Migrator | Separate LOGIN with SET-only S membership | Runs owner DDL under lineage, ledger and epoch mutation protocol; cannot assume T or use the R pool. |

SET-only membership prevents S's rights from being inherited by the migrator
login before `SET ROLE`; it does not make the migrator DML-denied. After
`SET ROLE S`, PostgreSQL grants the schema/table owner's DML, grant and trigger
authority. Treat S and any session that can assume S as privileged mutators.
Before selecting R, every such session must either follow the catalog-first
mutation protocol for its whole transaction or be independently quiesced and
fenced on the authoritative primary. A negative DML test on the unassumed
migrator login is not proof of this boundary.

The current dark attestor checks its own S/R/T/I subset and the receipt
profile checks S/R/A. Other owner-prepared stores verify separate fragments.
No single selected verifier currently joins those fragments with H, M and the
migrator, every enabled store ACL, and the provider's role memberships. The
serving package must pin that complete versioned matrix, provision the missing
grants, and check it after each relevant lock wait before granting a transaction
capability. The role administrator and privileged DDL process must honor the
same trusted mutation or independently proven quiescence boundary.

The dark lookup profile checks journal table, column and function grants. It
does not establish that R lacks separately granted server-file functions or
`pg_read_server_files`: PostgreSQL's [generic file functions](https://www.postgresql.org/docs/17/functions-admin.html#FUNCTIONS-ADMIN-GENFILE)
can bypass in-database table privileges. The serving role matrix must exclude
those direct and inherited capabilities before making a confidentiality claim.

The handoff order is: attest the authoritative primary against the
pre-transition pins; fence old login and reconnection; drain and verify old
backends; hold the owner transition barrier; apply V1030 if absent without
changing its registered bytes or checksum, then verify its committed ledger,
empty owner-only journal and exact catalog. In one owner transition, promote
V1030's existing schema-owned SECURITY INVOKER record function to A and
SECURITY DEFINER, create the separately versioned lookup function, and apply
the exact grants atomically. That transfer needs a trusted role administrator
with authority to change the owner, and a temporary A CREATE-on-schema grant
for PostgreSQL's owner-change rule; function creation runs as A. Revoke the
temporary CREATE grant in the same transaction and verify no temporary role
membership or CREATE privilege remains before R selection. Capture and hand
off the resulting OIDs through
the trusted provisioner, then refresh the full role and catalog profile on R
before enabling the selected pool and listeners. An uncertain transition
COMMIT retains its provisioning operation identity and the original
primary/backend fence until positive reconciliation; a new R connection
cannot certify that historical result. The old binary rejects the V1030
ledger, so rollback is a new forward repair protocol.

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
