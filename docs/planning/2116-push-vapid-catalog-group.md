# P2.1 Push custody and VAPID catalog group

Status: architecture-gated dark implementation plan for #2116 and #2046.
Both restricted-startup refusals remain active; this group cannot select
serving authority.

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
  The owner-prepared path has a one-transaction first-key and marker flow.
  The legacy environment path currently inserts another active key at every
  boot, conflicting with strict attachment's one-active-key invariant.
- SQLx physical checkout hooks can attest before a caller transaction begins.
  The catalog fence is cooperative and transaction scoped; privileged actors
  and unfenced old writers remain a separate cutover gate.

## Proposed transaction contract

1. Start the outer custody read through `begin_catalog_fenced_read_only` on
   the attested main pool, with catalog-S before protected or snapshot-forming
   work after the transaction-mode `SET` statements.
   Preserve the existing notification-outbox → Push → VAPID → registration/
   activity → attestation → PubSub → Foundation order on that same backend.
   Use bounded try/acknowledged-rollback/restart at each subordinate gate in
   existing order: outbox shared, Push exclusive, activity shared and PubSub
   exclusive. Do not acquire them all up front or wait on a lower gate while
   holding catalog-S. Keep transaction attestation before the PubSub helper,
   one application classification snapshot and bounded acknowledged rollback.
2. On PostgreSQL, decide VAPID provisioning under catalog-X then Push-X on
   the actual write transaction. Recheck schema, TEMP shadows, existing Push
   and VAPID markers, and table shape on that backend. The active-key read,
   any insert and Push-marker invalidation must share that transaction; a
   preceding raw DDL transaction does not cover these facts. Fully validate
   stored key material before reuse. Fail closed on multiple active rows,
   retired-only history, malformed environment input, marker inconsistency or
   an environment key that differs from the one active key. An environment
   key bootstraps the first key; later matching boots reuse it. This is an
   intentional PostgreSQL behavior change; SQLite remains as it is.
3. Never return a new signer before acknowledged COMMIT. On lost COMMIT,
   return a distinct indeterminate outcome and discard the checkout. A later
   attempt may reconcile only after reacquiring catalog-X/Push-X on the same
   fenced primary and rereading durable rows. Socket closure alone is not
   rollback proof. Do not expose secret material in errors or logs.

## Qualification and exclusions

Require PostgreSQL 17 public-entry tests for catalog-first order, lower-gate
retry, relation and role drift, cancellation, exact backend, bounded rollback,
concurrent provisioning and unknown COMMIT. Run affected SQLite regressions,
formatting, strict Clippy and independent exact-head review. A checkout-hook
rollback cannot alone prove an operation-level reader retry; use a stable
physical witness or narrow that claim.

The independent architecture gate approved this grouped dark scope only with
the ordered per-gate retry and revised first-key/uncertain-commit contract
above. It did not approve a serving or release claim.

The raw `load_or_ephemeral` key reader remains a separate unfenced entry.
Multi-session MAM, notification-activity, inbox and pending constructors,
raw old-writer cutoff, provider roles, selected P2.1, production SM and
release qualification remain outside this group. `MUC open()` remains three
commits. GitHub Actions and workflow files remain absent until all #2046
implementation is finished and `cuenv sync -A` restores them.
