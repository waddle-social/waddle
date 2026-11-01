# P2.1 owner bootstrap and read-only attachment

This is a dark implementation slice of [#2116](https://github.com/waddle-social/waddle/issues/2116) under [#2046](https://github.com/waddle-social/waddle/issues/2046). It separates the existing global lineage and migration bootstrap from a runtime check that does not mutate the database. Normal startup retains its current order and credentials until every store initializer and role grant has been reviewed.

## Contract

- The owner operation preserves the current sequence: create/check `_lineage`; enroll or adopt only under the configured operator action; verify lineage; then run the append-only migration ledger. It preserves the clustered missing-lineage refusal and reports whether adoption matched.
- The runtime verifier requires an existing, committed lineage row and a complete, valid migration ledger. It performs no `CREATE`, `ALTER`, `INSERT`, `UPDATE`, or backfill. A missing table, missing checksum, pending migration, unknown row, identity mismatch, or unauthorized read refuses attachment. PostgreSQL verification runs in a read-only transaction.
- The two operations are explicit APIs with distinct names. This slice does not change `main.rs` startup, provision database roles, flip epoch, use the physical attestor, or grant the current runtime login fewer privileges.

## Verification and later gate

Use disposable PostgreSQL with distinct owner and restricted LOGIN roles. Verify owner bootstrap commits before restricted attachment; the restricted login cannot run owner bootstrap or alter the ledger/lineage; wrong deployment or database schema is rejected; and PostgreSQL catalog snapshots prove the verifier made no DDL. Cover missing and pending ledger states. Preserve existing SQLite behavior. Record the exact required test inventory and strict Rust checks.

After this slice, store constructors and migration startup still perform DDL. A later independently reviewed change must move them behind owner bootstrap, define operator-produced expected physical identity, establish the epoch grant/guard contract, and select a safe rollout before runtime credentials can change.
