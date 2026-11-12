# Ingress epoch guards

The ingress foundation tables are expand-only at protocol epoch 0.  At that
epoch the V1009 triggers deliberately allow every write so a rolling deploy
can coexist with binaries that do not know this feature.

## Epoch-proof contract

At a nonzero epoch, writes to every guarded table require two
transaction-local GUCs. The guarded set is the 15 tables enrolled in
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
| `ingress_owner_rejections` | V1028 |
| `ingress_mailbox_lanes`, `ingress_mailbox_entries` | V1031 |

The two GUCs are:

```sql
BEGIN;
SET LOCAL waddle.protocol_epoch = '2'; -- the live epoch
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

## Epochs

| Epoch | Meaning |
| --- | --- |
| 0 | Guards are inert; every write is allowed. |
| 1 | Guarded writes require the transaction-local proof. |
| 2 | As 1. On clustered PostgreSQL, remote room acceptance uses the durable mailbox and clustered room manifests are sealed, unless `WADDLE_INGRESS_REMOTE_ROOM_ACCEPTANCE=relay`. |

A server implements epochs 0 through 2 and refuses a higher live epoch
inside each ingress transaction. A server built before the mailbox implements
epochs 0 and 1 and refuses a live epoch 2 the same way.

## Activation

The restricted-runtime owner phase advances the epoch to 2 as its last step,
one step per transaction, and stamps each step with the deployment's lineage.
A rerun finds the epoch at 2 and changes nothing. It is part of a
stopped-pod cutover:

1. Stop every server pod.
2. Rotate the credential of the login the old version used and terminate its
   remaining backends; verify none remain and `pg_prepared_xacts` is empty.
3. Run the owner job. It prepares the schema, grants the runtime role and
   activates the epoch.
4. Start the new version on every node with the same
   `clustering.remoteRoomAcceptance`.

From step 3 on an older server cannot commit an ingress transaction. Rollback
is **roll-forward only**: the epoch never moves down, a pre-V1008 catalog
refuses the newer migration ledger, and manual ledger surgery is not a
recovery path.

Messages committed before the activation keep the relay's behaviour. A
groupchat message that was dispatched to a remote room owner at epoch 0 or 1
has already been acknowledged to its sender; it gets no mailbox entry and no
recorded owner decision, and its outstanding dispatch is finished by the
ingress maintenance pass as before.

A node that selects the mailbox refuses to start when the mailbox tables
hold rows the cutover cannot explain: an entry whose message has no source
binding or owner decision, or an unfinished entry while the epoch is below 2.
The startup error names the counts. Do not edit the rows to pass the check;
find the writer that produced them.

A PostgreSQL deployment that does not run the owner phase stays at epoch 0
and keeps relay acceptance.

## Owner limitation

PostgreSQL table owners can disable triggers or replace their functions. The
epoch guard is a privilege boundary only where the server runs as the
restricted runtime role and a separate role owns the schema, which is also
the only deployment shape in which the owner phase activates the epoch. Where
the application role owns the migration-created objects, the epoch stays 0
and the guard is inert.
