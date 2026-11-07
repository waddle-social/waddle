# P2.1 catalog edge cutoff

Status: draft plan for #2116 and #2046. Both restricted-startup refusals remain
in force. This group cannot enable serving.

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

Use focused PostgreSQL 17 public-entry witnesses for catalog-first ordering,
lower-gate contention, acknowledged rollback, cancellation, and commit
visibility. Run the affected SQLite regressions, formatting, strict Clippy,
and an independent exact-head adversarial review. Record existing baseline
diagnostics separately from new failures. Old writers, prepared work,
external privileged actors, failover, full selected serving, exact-release CI
and deployment remain release gates. GitHub Actions and workflow files stay
disabled until #2046 is complete, when `cuenv sync -A` will regenerate them.
