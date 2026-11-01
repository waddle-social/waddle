# P2.1 pooled receipt isolation plan

1. Set PostgreSQL `READ COMMITTED` immediately after `record_receipt_pooled` begins its own transaction, before its epoch read. Leave SQLite unchanged.
2. Add a PostgreSQL regression using a pool with a `REPEATABLE READ` session default. Hold an epoch transition, start the receipt write, release the transition, and verify the receipt commits with the new epoch proof.
3. Run formatting, strict Clippy, and focused tests. Keep V1009, relation guards, roles, and other writer entrypoints outside this PR.

This slice prepares one writer path; it does not activate or complete the P2.1 protocol.
