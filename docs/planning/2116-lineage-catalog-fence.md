# P2.1 lineage catalog-fence group

Status: draft dark implementation plan. Restricted startup continues to refuse
with `UnconvertedStores` before listeners or effectful workers.

## Bounded scope

The three first-party lineage mutators—`ensure_table`, `enroll`, and
`adopt_if_matched`—share `lineage::begin_locked`. On PostgreSQL, acquire the
exclusive catalog mutation fence before the lineage advisory lock on the
same READ COMMITTED mutation transaction, before any catalog read, table DDL
or row write. Use bounded nonblocking attempts; a contended lineage lock
must release the catalog fence by acknowledged rollback before retrying in a
fresh transaction. Keep both locks through the actual PostgreSQL transaction
end. Preserve the SQLite immediate-write behavior.

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

Use PostgreSQL 17 tests to hold a shared catalog reader while each lineage
mutator starts, prove no lineage lock or DDL precedes catalog acquisition,
exercise catalog-first contention and a lineage-only blocker with bounded
rollback/retry and fresh READ COMMITTED snapshots, and verify cancellation
retains no false completion claim. Cover both winner orders and a second
connection. Re-run focused SQLite lineage enrollment/adoption tests. Run
formatting, scoped strict Clippy, diff checks and independent exact-head
review. GitHub Actions and workflow files remain disabled until #2046 is
complete.
