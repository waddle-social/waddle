# Ingress epoch guards

The ingress foundation tables are expand-only at protocol epoch 0.  At that
epoch the V1009 triggers deliberately allow every write so a rolling deploy
can coexist with binaries that do not know this feature.

## Epoch-proof contract

At a nonzero epoch, writes to every guarded table require two
transaction-local GUCs. The guarded set is the 12 tables enrolled in
`ingress_epoch_guard_manifest`; it is mirrored by `EPOCH_GUARDED_TABLES` in
`server/crates/waddle-server/src/ingress_substrate/substrate.rs`, and a catalog
test (which runs only against PostgreSQL, i.e. when `WADDLE_TEST_POSTGRES_URL` is set)
fails if the manifest, that list, or the live `ingress_*` tables diverge:

| Table | Enrolled by |
| --- | --- |
| `ingress_messages`, `ingress_origin_aliases`, `ingress_sm_refs`, `ingress_deliveries` | V1009 |
| `ingress_sm_streams`, `ingress_effect_intents` | V1010 |
| `ingress_effect_receipts`, `ingress_carbon_receipts` | V1012 |
| `ingress_delivery_receipts` | V1014 |
| `ingress_archive_dispatch` | V1020 |
| `ingress_send_attempts` | V1021 |
| `ingress_effect_descendants` | V1025 |

The two GUCs are:

```sql
BEGIN;
SET LOCAL waddle.protocol_epoch = '1';
SELECT set_config(
  'waddle.protocol_epoch_xid', pg_current_xact_id()::text, true
);
-- protected writes
COMMIT;
```

The xid proof makes the authorization specific to the current transaction;
session-level settings do not authorize a later transaction. `SET LOCAL` is
not enough for the xid because `SET LOCAL` cannot take the required expression.

The guards are statement-level `ENABLE ALWAYS` triggers. Each guard reads the
singleton epoch row with `FOR SHARE`, so an activation update serializes behind
in-flight epoch-0 writes: once the activation commits, the next write observes
the new epoch and requires the proof. The manifest is append-only and records
every protected `ingress_*` table; catalog tests verify its rows and triggers.

Writers must follow one lock order — **epoch row, then message row, then child
rows**. The substrate acquires the epoch row `FOR SHARE` as its first
statement in every write path; a writer that locks a message row before the
trigger's epoch request can deadlock against garbage collection (epoch-first)
with an activation queued between them.

`TRUNCATE` is rejected unconditionally on the protected tables at every
epoch. Its ACCESS EXCLUSIVE table lock is acquired before any trigger runs,
so a truncate trigger cannot participate in the epoch-first order — instead
of coordinating it, the guard forbids it; deletes are row-wise and
epoch-proven.

## Activation checklist: epoch 0 to 1

Do not flip the epoch until all of these are true:

1. Guard-manifest and live-trigger coverage checks are green.
2. There are no old writers that omit the two-GUC proof.
3. Issue [#1689](https://github.com/waddle-social/waddle/issues/1689) has
   provisioned the migration-owner/runtime-role split.
4. A named maintenance window and rollback plan are approved. Rollback is
   **roll-forward only**: a pre-V1008 catalog refuses the newer migration
   ledger, and manual ledger surgery is not a recovery path.

## Owner limitation

PostgreSQL table owners can disable triggers or replace their functions. Today
the application role owns these migration-created objects, so the epoch guard
is not a complete privilege boundary by itself. The future owner/runtime-role
split in #1689 is therefore an activation precondition, not optional hardening.
