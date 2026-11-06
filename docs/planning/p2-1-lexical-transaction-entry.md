# P2.1 lexical transaction entry

Issue: #2046, #2116. This is a staged, dark authority change. Runtime startup remains closed to the version 2 pinned document until every selected store and role path is qualified.

## Plan

1. Add a fixed, pool-backed pinned entry that owns its PostgreSQL transaction through acknowledged rollback. Use the independently provisioned `PinnedRuntimeAuthority`; never derive pins from the checked connection or treat the pool checkout hook as authority for later statements. Preserve the legacy path for existing callers while pinned startup remains closed.
2. Establish one bounded `READ COMMITTED READ ONLY` transaction before dependent reads. Prove physical lineage and catalog identity, then the complete registered V1030 migration ledger under shared lineage and ledger barriers, then prelock and recheck the pinned epoch heap. Keep all barriers on that same transaction through the fixed diagnostic and acknowledged rollback. This slice does not prove epoch row validity or a serving transition fence.
3. Keep the diagnostic fixed and dark. PubSub attachment returns a reusable database handle; proving its initial transaction would not authorize its later operations. The existing 24 transaction callers and direct attestor entries stay unconverted and must fail closed for pinned mode. Do not expose a raw pinned pool or weaken the version 2 startup refusal.
4. Add PostgreSQL 17 positive and negative tests for actual ordering, pin drift, access mode, cancellation/rollback, and relation replacement after a lock wait. Run formatting, focused integration suites, scoped strict Clippy, and independent diff review. Track environmental or unchanged baseline diagnostics separately.

## Gate

An independent architecture review must resolve authority sourcing, pinned catalog/epoch contracts, lock order, transaction lifetime, role and ACL drift, and compatibility before implementation. An independent integrated review must find no actionable issue before merge. GitHub workflows and Actions remain disabled during this staged work.
