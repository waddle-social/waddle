# P2.1 pinned receipt caller migration

Status: draft, dark only. Base: main `8b4c3a66` after #2265. This groups the
three legacy physical-attestor calls in `ReceiptOperationAuthority` into one
lexical pinned transaction chain. It does not select restricted startup,
enable a serving caller, or migrate other `attest_transaction` users.

## Contract

The test-selected PostgreSQL authority consumes an exact pinned main-pool
lane. It retains a non-escapable transaction from the first catalog-S lock
through pinned lineage/physical proof, complete V1030 migration ledger,
epoch/manifest proof, receipt profile and journal verification, fixed receipt
operation, and acknowledged completion. No earlier checkout hook or separate
transaction authorizes the receipt. The pinned branch never calls the legacy
attestor. The legacy PostgreSQL and SQLite paths retain their behavior.

The fixed operation ID and immutable receipt tuple are retained before any
cancellable await. A lost COMMIT response or timeout remains indeterminate;
only exact positive readback establishes a historical receipt transaction.
Absence never proves rollback or permits source-effect replay. Cancellation
or uncertain transaction completion discards the exact backend. The local
database proof cannot establish termination of the original server backend
or fencing of privileged DDL, role, primary, and epoch mutators.

## Acceptance

- PostgreSQL 17 on the actual restricted role and exact pinned main lane:
  success/replay/readback, wrong pins and physical/catalog/ledger/epoch/profile
  refusals before effects, same PID/XID and held locks, catalog contention,
  cancellation and lost COMMIT response with positive-only reconciliation.
- Legacy PostgreSQL and SQLite receipt regressions; no general pinned
  `Database` escape, startup selection, or `into_parts` bypass.
- Formatting, strict scoped Clippy with baseline diagnostics recorded, and
  independent adversarial review of the integrated head.

The full all-store role matrix, old-login/backend cutoff, protected source
effect result custody, restricted startup and #2046 release checks remain
separate gates. GitHub Actions and workflow files stay disabled until #2046
implementation is finished.
