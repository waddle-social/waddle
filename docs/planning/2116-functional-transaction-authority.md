# P2.1 functional transaction authority for #2116 and #2046

## Goal and boundary

Move ordinary ingress submission, pooled receipt completion, and scheduler/descendant work onto one exact physically attested main pool with a private, lexical transaction authority. Each covered transaction must acquire the epoch barrier, refresh the restricted runtime profile from a fresh READ COMMITTED snapshot in that same transaction, and keep the verified physical/role identity through commit. Unknown commit outcomes retain an exact operation identity for authoritative reconciliation; cancellation must never be treated as rollback proof.

The merged #2216 profile is a non-authorizing diagnostic. Its strict no-view/no-foreign-table/no-rule prerequisite and the scoped owner-grant AX test do not by themselves authorize a serving selector. Provider DDL/role quiescence, trusted catalog mutation protocol, attestor integrity, old-writer drain, all-store custody and release qualification remain separate gates.

## Grouped implementation plan

1. Independently review the transaction API, complete lock order, profile-refresh query and provider mutation assumptions before selecting any caller. Resolve blocking findings first.
2. Add a pool-bound, non-cloneable transaction wrapper. Acquire lineage and migration-ledger locks in the reviewed order, then epoch ACCESS SHARE before protected work. Refresh the complete restricted profile after the epoch barrier in READ COMMITTED, using the same physical connection and transaction; recheck at commit as required by the reviewed contract.
3. Move ordinary UOW entry, pooled receipts, and raw scheduler/descendant paths together behind the same wrapper. Remove or fence raw-pool escapes and legacy split authority for covered operations. Preserve SQLite behavior with explicit backend-specific tests.
4. Test wrong lineage/role/schema and reconnect, role/ACL/epoch drift, concurrent independent transactions, cancellation, lost COMMIT reply, exact receipt reconciliation and all affected callers under DML-denied runtime. Run focused PostgreSQL 17/SQLite suites, formatting, strict scoped Clippy and independent final review.

## Activation gate

Do not enable restricted serving or advance the epoch until provider-controlled catalog mutations, role provisioning, attestor/function integrity, all writer/consumer conversions, old login/backend drain, rollback/forward recovery and release qualification are proven. If those gates cannot be completed in this PR, keep the merged work explicitly dark and track the remaining functional selection under #2116.

## First dark slice: rollback-only fixture entry

The first implementation is deliberately available only with `test-support`.
`RestrictedTransactionProbe` is a fixture facade over a private, non-cloneable
factory. It retains an existing main `Database` and that exact pool's private
capability identity; an independently opened pool with identical settings is
refused. The fixture owner supplies the independent schema, epoch relation,
runtime role and API-owner role OIDs. A prior public observer receipt remains
non-authorizing.

Each invocation owns a clean READ COMMITTED READ ONLY transaction. A total
15-second deadline bounds checkout and preparation; local lock/statement limits
cap waits at 3/5 seconds without loosening tighter session settings. It takes
shared lineage then shared migration-ledger advisory locks, then epoch ACCESS
SHARE before protected relation reads. The separately trusted pool-hook attestor
can read lineage before this transaction; the epoch barrier does not cover that
hook or its installer. After any lock wait, fresh statements recheck physical
and session identity, primary status, supported epoch, exact schema/epoch/role
OIDs, and the complete catalog/function/ACL/structure manifest on that same
connection. The shared verifier restores and verifies the application search
path. Success requires an acknowledged ROLLBACK.

The facade accepts no SQL, callback, transaction accessor, ingress operation or
commit. Its fixed pause is fixture synchronization only and remains under the
total deadline. Cancellation drops the lexical transaction; tests separately
observe the original backend's transaction and lock release before claiming
rollback. No ingress proof GUC is installed. No SQLite behavior is changed.

This proves only the mechanics of a read-only fixture entry. The public
observer stays diagnostic, and the fixture entry cannot select ordinary UOWs,
pooled receipts, scheduler/GC work or serving. Provider catalog/role quiescence,
trusted AX mutation coverage, attestor integrity, same-transaction writer entry,
all-store custody, unknown-COMMIT recovery, old-writer drain and release
qualification remain blocking activation prerequisites. In particular, epoch AS
fences cooperating AX mutations only; a sequence of READ COMMITTED catalog
queries does not freeze uncooperative provider/role mutations.
