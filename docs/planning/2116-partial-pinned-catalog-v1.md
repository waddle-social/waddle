# P2.1 partial pinned catalog profile V1

This is an unselected, rollback-only diagnostic for #2116 and #2046. It is not
an all-store audit, a transaction capability, a production selector, or
permission to start restricted serving. Existing startup refusals remain.

The caller supplies a pinned pool and a separate immutable operator handoff.
The handoff identifies S (schema owner), R (runtime), A (ingress API owner),
H (PubSub helper owner), T (attestor owner), I (attestor installer), M
(`pg_monitor`) and X (migrator). All eight OIDs must be nonzero and distinct;
only S may coincide with the independently pinned database owner. S/A/H/T
are NOLOGIN and R/I/X are LOGIN; the seven non-monitor roles are NOINHERIT
and lack PostgreSQL superuser, database/role creation, replication and bypass
RLS attributes. The only membership edges involving those seven are X-to-S and
I-to-T with SET only. M may have unrelated memberships. These checks are a
partial role graph, not an inventory of all database or server privileges.

The independently pinned additional relation OIDs are the 13 fixed
`EPOCH_GUARDED_TABLES` and 12 named PubSub/projection tables in
`PinnedPartialProfilePinsV1`, in their documented array order. The diagnostic
checks their names, schema/owner, ordinary permanent heap identity, lack of
inheritance, rules and row security, and the direct table and column ACLs:

- Each guarded ingress table grants A exactly SELECT, INSERT, UPDATE and
  DELETE, with no column ACLs. R has no table or column grant.
- Each PubSub/projection table grants R its fixed V1 table rights and H UPDATE;
  no column ACL is allowed.
- The A, R and H grants listed above are required. S may retain its owner ACL;
  a non-grantable owner-granted M SELECT is allowed but not required. Every
  other non-owner grant, PUBLIC grant, grant option or different grantor on
  these relations fails closed. This does not pin M's full monitoring graph.
- The epoch grants R and A SELECT and A UPDATE(id); the manifest grants R
  SELECT. M SELECT is optional on both. No other non-owner table or column
  grant is accepted.

Inventory counts and ACL byte/compression checks precede ACL expansion. These
store heap checks do not validate columns, indexes, constraints, triggers,
functions, stored rows, sequences, schema/database ACLs, cross-object relays,
or any unlisted store. The inherited pinned physical attestor, complete V1030
migration ledger, and epoch/manifest catalog verifiers independently check
their documented shapes and data. Their results are not enlarged by this
profile. No guarded or PubSub heap is read or locked here; in particular R is
not granted guarded-table privileges merely to make a `LOCK TABLE` possible.

On one pinned backend, the selected transaction takes the cooperative shared
catalog fence first, runs the partial catalog-only profile before its own
attestor or data reads, and rechecks the profile after the lineage and ledger
relation waits before those reads. It rechecks again after the epoch and
manifest barriers before the epoch read. It uses READ COMMITTED READ ONLY and
success requires acknowledged rollback. Cancellation or timeout discards an
uncertain session; client timeout alone does not prove server rollback.
Each profile pass currently issues 82 catalog statements, and the successful
path performs four passes under the existing 20-second overall deadline. The
PG17 witness uses a local Unix socket; remote latency has not been measured.
At sufficiently high round-trip latency, a healthy database will time out and
the diagnostic will fail closed. This slice does not qualify remote availability.

The already-existing pinned pool checkout hook runs its separately pinned
attestor before the selected transaction starts. It is not covered by the new
joint preflight. A trusted provider must independently enforce no login trigger
and keep privileged DDL, role/ACL changes, function replacement and concurrent
index maintenance quiescent from before connection through acknowledged
rollback. The cooperative advisory fence cannot enforce this against an owner,
superuser or nonparticipant. Old-backend/primary fencing, all-store and
function authority, writer conversion, and release qualification remain open.
This slice has no login-trigger witness and does not verify the provider's
preconnection assumption.
