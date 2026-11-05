# P2.1 joint Push and notification custody diagnostic

This #2046/#2116 tranche proposes a dark, opaque proof of prepared Push and
notification state on one physically attested PostgreSQL main pool. It does
not return functional stores, wire restricted startup, start workers,
transition the epoch, or establish that old writers have stopped. The fatal
`UnconvertedStores` guard remains before listeners.

## Plan

1. Use the independent architecture gate to define the exact cross-store
   custody invariant and lock order. Treat unresolved ancestry or uncertain
   sends as retained obligations, never as completed or safe to discard.
2. Add a sequential owner-only preparation path for Push schema, notification
   outbox, ancestry adoption, and VAPID key custody. Each phase commits on
   its own and must be restartable; do not present it as one atomic migration.
3. Build a bounded read-only verifier on one supplied attested main-pool
   transaction. Acquire the outbox gate before the Push gate and qualified
   relation locks. Verify physical identity, completed revisions, effective
   role/ACL boundaries, VAPID key consistency, and one-snapshot ancestry and
   unsettled-obligation classification. Existing per-store attachment APIs
   cannot substitute for this joint transaction.
4. Test a one-slot PostgreSQL 17 pool with distinct owner/runtime roles,
   exact lineage and marker failures, per-table grants, an owner/attach race,
   uncertain Push jobs, outbox lineage and Foundation descendants. Verify
   read-only behavior, formatting, strict Clippy and independent final review.

If the data invariant or ownership handoff cannot be made exact, narrow the
PR to independently reviewed owner preparation and report the blocking
contract. Old-writer session drain, direct-primary fencing, V1009 role/epoch
compatibility, functional constructors, clustered ownership and release
qualification remain separate gates. GitHub Actions and workflow files stay
disabled until #2046 is complete, per user direction.

## Local verification and remaining gate

The final combined tree passed the one-slot PostgreSQL 17 custody-classifier
test (1/1), existing PostgreSQL runtime-attachment regressions (4/4), and the
SQLite owner-rejection test (1/1). Server formatting, scoped all-features
Clippy with `-D warnings` (excluding the repository's existing unused-import
and dead-code categories), and a no-default-features server check passed.
Independent read-only final review found no remaining actionable issue after
the exact lineage and Foundation-parent fixes. The Rust structure script still
reports two unchanged test files outside this diff.

These tests exercise the one-statement SQL classifier and each existing store
attachment, but do not exercise the full joint diagnostic against a single
database containing all owner-prepared stores and distinct owner/runtime
roles. The helper remains unwired, returns no serving handle, and cannot be
used as a restricted-startup or rollout certificate. That integrated role and
one-slot transaction case remains a separate qualification gate.
