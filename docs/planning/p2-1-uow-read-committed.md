# P2.1 ordinary ingress transaction isolation

This is a dark #2116 preparation slice under #2046, based on current main `9242346e`. It makes PostgreSQL ingress unit-of-work transactions explicitly `READ COMMITTED` before their first epoch, timeout, or lineage read. It does not change the V1009 guard, epoch value, roles, migration checksums, or startup pool wiring.

## Contract

- `IngressUnitOfWork::begin` and `begin_with_timeouts` use `READ COMMITTED` on PostgreSQL regardless of session defaults. The existing recovery entrypoint retains the same semantics. SQLite keeps its current transaction path.
- Set isolation immediately after transaction acquisition. Preserve the epoch lock as the first locking statement and the existing acquisition/statement timeout and error boundaries.
- Under a pool whose session default is `REPEATABLE READ`, hold a valid epoch 0-to-1 update uncommitted, start an ordinary ingress transaction, confirm it waits on the epoch lock, commit the transition, then prove the ordinary transaction observes epoch 1 and commits. The pre-change path should fail with PostgreSQL serialization rather than merely asserting the isolation label.
- Verify the session default remains unchanged after transaction completion, and retain the recovery-path assertion. Run focused PostgreSQL and SQLite tests, formatting, and strict Clippy.

## Limits

This changes only transactions opened by `IngressUnitOfWork`. Pooled receipt writers, caller-owned raw transactions, roles, the proposed relation-lock migration, mixed-version transitions, and production activation remain separate gates. A passing local PostgreSQL test establishes this entrypoint's behavior, not full P2.1 authority.
