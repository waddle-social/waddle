# P2.1 pending-delivery owner bootstrap

This is a dark [#2116](https://github.com/waddle-social/waddle/issues/2116) slice under [#2046](https://github.com/waddle-social/waddle/issues/2046), stacked on the SM owner bootstrap. The independent architecture gate cleared a separate pending-delivery extraction because its initializer includes receipt schema, startup markers, a one-transaction V1014 claim reset, batched legacy MAM cleanup, and recovery-order metadata.

## Plan

1. Expose the entire pending-delivery schema initializer as an owner operation on `Database`, independent of a live pending-delivery storage object. Preserve SQL, statement and transaction boundaries, error mapping, driver behavior, and initialization order. The existing constructor calls that operation at the same point.
2. Keep the PostgreSQL receipt advisory lock and V1014 reset transaction, the separate legacy cleanup batches and marker, and the recovery-order timeout and transaction intact. Keep clustered pending/SM colocation and pending-before-SM startup checks where they are.
3. Test fresh and repeated SQLite and PostgreSQL bootstrap, existing-schema repair and recovery, and owner versus restricted runtime behavior. Run focused pending-delivery tests, Rust formatting, strict Clippy, and the PostgreSQL test inventory.

No runtime credential switch or read-only pending-delivery attachment is included. Existing constructors still use the current startup credential. Other store constructors and the full transaction authority remain separate work.

## PostgreSQL runtime attachment

`DatabasePendingDeliveryStorage::attach_existing` is a separate, dark
prerequisite. For an attested PostgreSQL main lane, it locks the three qualified
pending-delivery tables before taking its read-only repeatable-read snapshot,
then checks the physical identity, table and index definitions, migration
markers, and recovery metadata. It neither prepares nor repairs the schema.

The table locks coordinate with conflicting table DDL during the snapshot; they
do not serialize all catalog changes. In particular, an owner can independently
change the identity sequence or replace the inspected trigger function while
the tables are locked. Before activating this attachment, the owner must keep
schema maintenance quiescent while it runs and for as long as its result is
relied upon, or provide a separate coordination mechanism. The snapshot is not
a durable DDL fence or proof of runtime write permissions or operational writer
readiness.

## Shared-pool startup selection

For an attested PostgreSQL global handle, startup accepts only an inherited
global store selection and attaches the exact main lane without owner schema or
lineage bootstrap. The resulting boundary is registered as an alias of global.
It validates the runtime role and required DML grants in the same bounded
snapshot as the schema, including read-only startup markers and the identity
sequence. It rejects owner-like table privileges, PUBLIC grants and grant
options. Clustered operation additionally requires live claim handles; it
cannot fall back to unfenced pending delivery on this path.

The check is a snapshot, not continuous role/DDL enforcement or proof of a
fenced primary. Other production startup stores and the global runtime handoff
are still required before an attested server can serve.
