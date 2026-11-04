# P2.1 inbox owner bootstrap

This is a dark [#2116](https://github.com/waddle-social/waddle/issues/2116) slice under [#2046](https://github.com/waddle-social/waddle/issues/2046), stacked on the pending-delivery owner bootstrap. The independent architecture gate cleared an inbox-only extraction. Runtime attachment, pool consolidation, and epoch activation remain separate work.

## Plan

1. Expose the existing inbox schema initializer as an owner operation on `Database`, independent of a live inbox storage object. Retain the exact SQL, per-call pool acquisition, error mapping, driver behavior, and constructor call point.
2. Preserve SQLite legacy thread-table rebuild and data, PostgreSQL bigint widening, call-thread and notification-recovery repairs, and the advisory-locked transaction that deletes legacy recovery rows before adding the required message key.
3. Keep HTTP's pre-open and post-open colocation checks, its SQLite shared handle, and existing startup order. Test fresh/repeated/reopened SQLite, legacy migrations, and PostgreSQL owner versus restricted runtime; run existing inbox regressions, formatting, strict Clippy, and PostgreSQL test inventory.

The constructor still calls owner bootstrap with its existing startup credential. This slice does not introduce read-only attachment, change credentials, collapse correctness pools, or authorize production activation.

## Shared-pool startup selection

The attested PostgreSQL startup factory attaches the exact inherited global
main lane. It locks the qualified inbox tables before establishing its
repeatable-read snapshot, then checks schema, role separation and required
writer grants in that snapshot. A SELECT-only role, a PUBLIC grant or an
owner-like privilege is refused. SQLite and the standalone snapshot-only
attachment keep their existing interfaces.

The checks do not fence later owner DDL or role changes, audit unrelated
tables, or prove that the complete server can serve. The global attested
startup still has an explicit refusal until its other stores are converted.
