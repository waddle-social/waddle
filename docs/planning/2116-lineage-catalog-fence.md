# P2.1 lineage catalog-fence group

Status: lineage mutator slice implemented. Restricted startup continues to
refuse with `UnconvertedStores` before listeners or effectful workers.

## Bounded scope

The three first-party lineage mutators—`ensure_table`, `enroll`, and
`adopt_if_matched`—share `lineage::begin_locked`. On PostgreSQL, acquire the
exclusive catalog mutation fence before the lineage advisory lock on the
same READ COMMITTED mutation transaction, before any catalog read, table DDL
or row write. Use bounded nonblocking attempts; a contended lineage lock
must release the catalog fence by acknowledged rollback before retrying in a
fresh transaction. Keep both locks through the actual PostgreSQL transaction
end. Finish the pool checkout and any hook before taking catalog-X; do not
open another checkout while holding it. Mark the transaction discard-on-drop
before cancellable work, and close an unconfirmed checkout after a bounded
rollback attempt. Preserve the SQLite immediate-write behavior.

Do not infer rollback from a cancelled COMMIT, absent row or closed client.
`enroll` and `adopt_if_matched` currently mint a new lineage UUID inside the
transaction and have no durable operation/backend result. Their unknown
COMMIT recovery is a separate gate and must not be described as solved by
this lock change.

## Mutation coverage ledger

| Family | State after this group |
| --- | --- |
| Global and store owner lineage `ensure_table`/`enroll`/`adopt_if_matched` | Catalog-first participating transaction |
| Registered `MigrationRunner::run` | Existing bounded catalog-first implementation; retain and regression-test |
| Alternate migration-owner entry | Unconverted |
| Attestor installer, restricted role/ACL mutators | Unconverted |
| Detached MAM, Push, notification outbox and other owner schema constructors | Unconverted |
| Manual/provider owner, superuser, concurrent-index and role changes | Outside application enforcement |

This ledger is not a process-wide mutator closure. No runtime policy flag or
serving selector may rely on it until every reachable first-party path is
converted or explicitly disabled during the serving interval and external
privileged identities are independently excluded. A v3 operator record is a
procedure assertion, not continuing cutoff proof. Old-login reconnection,
existing backends, prepared transactions, original primary and failover
fencing remain open.

## Verification

Focused PostgreSQL 17 integration tests cover all three mutators behind a
held catalog reader, actual DDL/row visibility after release, dual-lock
evidence at a relation wait, lineage-only contention with acknowledged clean
retry boundaries, and concurrent enrollment/adoption outcomes. Focused SQLite
lineage tests cover 14 enrollment/adoption and malformed-state cases.

Cancellation at each transaction edge, lost COMMIT response, pool-hook
contention, the acquisition deadline under saturation, both controlled winner
orders, and a lineage-specific session-fenced `REINDEX CONCURRENTLY` with an
old snapshot are not established by these tests. The migration runner has its
own catalog-fence and REINDEX witnesses; those do not prove this lineage path.
The broader cutoff remains open. GitHub Actions and workflow files remain
disabled until #2046 is complete.
