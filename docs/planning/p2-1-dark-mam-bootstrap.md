# P2.1 dark MAM bootstrap and attachment

The PostgreSQL owner operation is unwired. An independently supplied
installation identity and migration login are checked on a new direct
backend before the separately provisioned NOLOGIN owner is assumed for the
session. The complete migration ledger and physical lineage are verified on
that backend. Session advisory locks are acquired in lineage, ledger, then
MAM order; no epoch or data-table transaction lock spans the MAM autocommit
DDL. The MAM initializer owns the non-pooled connection while it holds its
lock. The owner operation checks schema and counters in a bounded, read-only
transaction before acknowledging completion.

The caller must independently fence the authoritative primary and old
backends, drain writers, and prevent concurrent privileged schema, role, or
grant changes over global migration and MAM bootstrap. Advisory locks only
serialize cooperating clients. Timeout, cancellation, connection loss, or
initializer failure can leave an unknown or partial outcome: dropping the
socket does not prove the server stopped. Fence the old backend and reconcile
the catalog and counters on the authoritative primary before retrying.

The test-support-only attachment verifies the runtime role and its physical
capability, lineage, complete ledger, and MAM schema and counters in a single
main-admitted repeatable-read, read-only transaction. It creates no tables,
returns no raw pool or connection, and retains the existing reserved ingress
lane. The result is a snapshot; it does not prevent later privileged drift or
attest old-session drainage. Checking surviving rows cannot reconstruct a
deleted-tail high-water mark. Existing MAM storage and startup remain unchanged.
The shared runtime still requires narrowly scoped MAM row privileges for
transactional ingress: table SELECT and INSERT on MAM messages and counters,
plus column UPDATE only on the counter's `next_seq`. The check rejects PUBLIC
ACLs, grant options, destructive table privileges and lineage/ledger writes.
A read-only attachment is not a read-only runtime role. The contract denies
MAM UPDATE and DELETE, so it does not authorize tombstone/retraction or every
generic submission that the dark facade can represent.

The MAM initializer's independent autocommits are not made atomic by V1017's
ledger marker. In particular, an existing non-null ordinal column with absent
or stale counter rows must fail the verifier. Other public database paths,
legacy direct MAM writers, and the in-flight commit barrier remain cutover
prerequisites. SQLite continues to use its existing same-pool initialization.
