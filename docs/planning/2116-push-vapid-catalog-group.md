# P2.1 Push custody and VAPID catalog group

Status: draft architecture gate for #2116 and #2046. Both restricted-startup
refusals remain active; this group cannot select serving authority.

## Objective

Close two remaining single-operation PostgreSQL catalog cutoffs around Push:
the outer notification/Push/VAPID/PubSub custody reader and the legacy VAPID
key provisioning write. Keep the existing SQLite behavior and XMPP wire
behavior. These are separate transactions in one grouped PR.

## Current boundaries

- `verify_attested_push_notification_custody` begins a read-only READ COMMITTED
  transaction, then takes notification-outbox, Push, VAPID, registration and
  activity locks before its attestation and PubSub helper. It lacks an outer
  catalog-S fence; an inner PubSub helper cannot safely acquire catalog-S
  after those locks.
- `VapidStorage::initialize` fences PostgreSQL schema DDL, but the legacy
  `load_or_provision` path reads and inserts the key on separate checkouts.
  The owner-prepared path has a one-transaction first-key and marker flow; it
  does not authorize changing the legacy path's environment-key semantics.
- SQLx physical checkout hooks can attest before a caller transaction begins.
  The catalog fence is cooperative and transaction scoped; privileged actors
  and unfenced old writers remain a separate cutover gate.

## Proposed transaction contract

1. Start the outer custody read through `begin_catalog_fenced_read_only` on
   the attested main pool, with catalog-S as the transaction's first statement.
   Preserve the existing notification-outbox → Push → VAPID → registration/
   activity → attestation → PubSub → Foundation order on that same backend.
   Review each subordinate gate for bounded try/acknowledged-rollback/retry;
   never wait on a lower gate while holding catalog-S. Retain one application
   classification snapshot and use bounded acknowledged rollback.
2. On PostgreSQL, decide VAPID provisioning under catalog-X then Push-X on
   the actual write transaction. The active-key read, any key insert, and
   commit/uncertainty handling must share that transaction. A failed or
   indeterminate commit must not blindly mint another active key. Keep
   secret material zeroized and fail closed on contradictory active rows.
   Preserve SQLite's current path.
3. Before implementation, resolve whether the environment key requests
   deliberate rotation on each boot or merely bootstrap of the first key.
   The current path creates a fresh KID whenever that variable is present;
   a uniqueness assertion or reuse rule would change behavior. Inspect
   callers, data constraints and tests; record the chosen contract and a
   real PostgreSQL concurrent-startup/unknown-commit witness.

## Qualification and exclusions

Require PostgreSQL 17 public-entry tests for catalog-first order, lower-gate
retry, relation and role drift, cancellation, exact backend, bounded rollback,
concurrent provisioning and unknown COMMIT. Run affected SQLite regressions,
formatting, strict Clippy and independent exact-head review. A checkout-hook
rollback cannot alone prove an operation-level reader retry; use a stable
physical witness or narrow that claim.

Multi-session MAM, notification-activity, inbox and pending constructors,
raw old-writer cutoff, provider roles, selected P2.1, production SM and
release qualification remain outside this group. `MUC open()` remains three
commits. GitHub Actions and workflow files remain absent until all #2046
implementation is finished and `cuenv sync -A` restores them.
