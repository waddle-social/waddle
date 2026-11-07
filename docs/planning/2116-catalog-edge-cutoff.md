# P2.1 catalog edge cutoff

Status: implemented dark catalog-edge cutoff for #2116 and #2046. Both
restricted-startup refusals remain in force. This group cannot enable serving.

## Objective

Close three reachable catalog edges left by #2257: raw VAPID schema
initialization, the scoped owner-rejection GRANT, and the room-observation
attested verifier. Review raw and guarded entry points before claiming any
whole store family converted. PubSub, spaces, room effects, MUC, call teardown,
keypool, inbox, pending delivery, notification and MAM remain separate groups;
many cross backends or transactions and need their own custody protocol.

## Protocol and implementation order

1. Review each entry's exact transaction, checkout hooks, relation locks,
   role prechecks, DDL, and cancellation path. Preserve the owner-rejection
   helper's direct runtime correlation session, opened before the owner
   checkout, and its current unknown-COMMIT readback.
2. Put raw VAPID's two DDL statements in one PostgreSQL transaction whose
   actual backend holds catalog-X before the Push gate. Preserve SQLite
   behavior and the raw store's separate key persistence semantics.
3. For the scoped owner-rejection GRANT, acquire catalog-X before lineage and
   ledger on the actual owner transaction, before role/catalog prechecks.
   Roll back and retry after any subordinate gate contention. Keep the epoch
   ACCESS EXCLUSIVE relation lock and post-commit correlation. Inspect-only
   reads need a distinct shared-fence protocol.
4. For the room-observation verifier, acquire catalog-S before its catalog
   reads on the same READ COMMITTED transaction. Its migration check implicitly
   takes ledger-X: the full order is catalog-S, lineage-S, ledger-X,
   observation-owner-S, observation-schema-S. Try lower gates and acknowledge
   rollback before retry instead of waiting while holding catalog-S.
5. Do not guard only a coordinator while another checkout executes DDL. A
   lost COMMIT remains unknown until original-backend and primary custody are
   reconciled. Preserve SQLite behavior and the restricted-startup refusals. Do not
   enable the selected serving authority until every reachable store and
   production SM source/recipient boundary passes its release gate.

## Verification and remaining gates

The PostgreSQL 17 public-entry owner-rejection fixture covers catalog-first
ordering, lower-gate rollback, and commit visibility. It also checks that the
preopened runtime correlation session inherits READ COMMITTED READ ONLY even
when its role defaults to REPEATABLE READ, so it cannot retain an old snapshot
while the owner waits for catalog-X. The raw VAPID fixture observes catalog-X
before both DDL statements. The room-observation fixture exercises catalog-S
and ledger-X contention, acknowledged rollback, and a fresh post-release
completion marker. The existing Push owner fixture still passes.

The room verifier and owner-rejection inspection use a distinct READ COMMITTED
READ ONLY catalog-S path. Raw VAPID key loading and insertion after schema
initialization remain outside this transaction and need a separate conversion.
This group does not convert other store families or enable the selected serving
authority. Old writers, prepared work, external privileged actors, failover,
production SM custody, full selected serving, exact-release CI, and deployment
remain release gates. GitHub Actions and workflow files stay disabled until
#2046 is complete, when `cuenv sync -A` will regenerate them.
