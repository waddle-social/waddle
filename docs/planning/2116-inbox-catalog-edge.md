# P2.1 inbox catalog edge

Status: dark implementation plan for #2116 and #2046. Both restricted-startup
refusals remain active, and no serving authority is selected.

## Objective

Move the PostgreSQL inbox owner bootstrap and direct attested inbox readers
onto their actual catalog-fenced backends. The current owner performs DDL on
per-call checkouts, uses a separate bigint-widening checkout, and starts a
distinct transaction for legacy recovery-row deletion. A coordinator lock
would not cover those writes. SQLite keeps its existing rebuild and migration
behavior.

## Transaction contract

- PostgreSQL owner bootstrap begins one bounded READ COMMITTED transaction on
  the backend doing every CREATE, ALTER, index and column probe. Acquire
  catalog-X before the existing one-argument
  `pg_advisory_xact_lock(hashtext('groupchat_notification_recovery.message_key'))`
  interlock. Try that exact bigint-key namespace and acknowledge rollback
  before a fresh catalog attempt on contention. Pin the intended persistent
  schema and use a builtins-first path before any untrusted catalog expression.
  Do not call the old per-checkout widening helper or nested transaction from
  inside the fence.
- Preserve the conditional delete of legacy recovery rows before adding the
  required `message_key` column. The delete and ALTER remain indivisible, now
  with all PostgreSQL DDL in the same commit. Existing partial schemas from
  older runs remain repairable. A lost COMMIT reply returns an identifiable
  indeterminate result and no successful store; retry requires resolution of
  the original backend on the same authoritative primary, then reacquiring
  the fences.
- Direct attested inbox attachment starts its actual main-pool backend under
  bounded READ COMMITTED READ ONLY catalog-S before relation locks, attestation
  and shape/ACL checks. Keep checks and identity revalidation in that
  transaction and require acknowledged bounded rollback before returning.
  The two public attachment modes must retain their distinct runtime-grant
  requirements.

## Evidence and limits

Use native PostgreSQL 17 public owner/open and attach witnesses for catalog
lock order, exact backend, old hash-gate contention, migration rollback,
partial-schema repair, int4 widening, legacy-row deletion, later keyed-row
retention, cancellation and unknown COMMIT. Run affected SQLite rebuild and
recovery tests, formatting, strict Clippy and independent exact-head review.

This slice does not establish deletion safety from V1014 alone: old writers can
still add legacy rows and that migration excludes store-owned tables. It also
does not establish a complete inbox owner marker, all relation/index shape,
pre-attestor checkout-hook custody, privileged/old-writer cutoff, role/epoch
rollout, selected serving, or release qualification. GitHub Actions and
workflow files remain absent until #2046 implementation is complete.
