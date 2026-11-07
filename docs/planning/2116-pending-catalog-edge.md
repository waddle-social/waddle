# Pending Delivery catalog edge

The dark PostgreSQL catalog-fence pilot must include every owner-side Pending
Delivery schema mutation and direct attachment reader before the cooperative
fence can be used as an integration boundary. This is a scoped catalog change;
it does not activate the serving cutover or establish a whole-bootstrap atomic
transaction.

1. Pin the intended persistent schema before setting a safe `pg_catalog`-first
   search path, and qualify owner DDL and DML. Use a separate catalog-X
   transaction for the base, receipt, V1014 reset, recovery, each cleanup
   deletion batch, and the final cleanup marker. Keep receipt, reset and
   recovery's established advisory key identities and acquire them only after
   catalog-X, retrying from an acknowledged rollback when contended.
2. Retain the receipt and recovery initialization boundaries, the atomic V1014
   marker plus three-column update, and the legacy cleanup's bounded 128-row
   selection/deletion and final marker. An unacknowledged COMMIT is a typed
   indeterminate result, and its checkout must not be reused. Leave SQLite's
   existing behavior intact.
3. Hold catalog-S on the same PostgreSQL backend in a bounded READ COMMITTED
   READ ONLY transaction before attachment relation locks, attestation, shape
   and ACL verification. Preserve the distinction between read and writer
   grants and acknowledge rollback on inspection failures.
4. Add PostgreSQL 17 physical witnesses for catalog-first lock ordering,
   lower-gate contention and progress, schema pinning and phase atomicity, and
   direct-reader exclusion; run focused tests plus formatting and applicable
   server checks during integration.

Cooperation is required of every privileged mutator and role operator. This
slice does not close serving races in the first V1014 claim reset, cleanup's
row-id reuse or behind-cursor insertion, or a checkout hook that runs before
catalog-S. A failed or indeterminate later phase does not undo an earlier
committed phase.
