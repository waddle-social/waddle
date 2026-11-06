# P2.1 lexical transaction entry

Issue: #2046, #2116. This is a staged, dark authority change. Runtime startup remains closed to the version 2 pinned document until every selected store and role path is qualified.

## Plan

1. Add an exact-transaction pinned entry on the caller-owned PostgreSQL transaction. Use the independently provisioned `PinnedRuntimeAuthority`; never derive pins from the checked connection or treat the pool checkout hook as authority for later statements. Preserve the legacy path for existing callers while pinned startup remains closed.
2. Establish one bounded `READ COMMITTED` transaction and its access mode before dependent reads. Prove physical lineage and catalog identity, then the complete registered V1030 migration ledger under shared lineage and ledger barriers, then the pinned epoch relation and row, before acquiring any application store lock. Keep all barriers on that same transaction through dependent work.
3. Convert the first suitable read-only diagnostic callers to the shared entry. Keep the entire remaining caller inventory explicit and unconverted paths fail closed for pinned mode. Do not expose a raw pinned pool or bypass through a direct attestor call.
4. Add PostgreSQL 17 positive and negative tests for actual ordering, pin drift, read-only/write mode, cancellation/rollback, and first caller behavior. Run formatting, focused integration suites, scoped strict Clippy, and independent diff review. Track environmental or unchanged baseline diagnostics separately.

## Gate

An independent architecture review must resolve authority sourcing, pinned catalog/epoch contracts, lock order, transaction lifetime, role and ACL drift, and compatibility before implementation. An independent integrated review must find no actionable issue before merge. GitHub workflows and Actions remain disabled during this staged work.
