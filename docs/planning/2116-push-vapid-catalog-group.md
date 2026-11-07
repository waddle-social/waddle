# P2.1 Push custody and VAPID catalog group

Status: locally verified dark implementation for #2116 and #2046. Both
restricted-startup refusals remain active; this group cannot select serving
authority.

## Objective

Close two remaining single-operation PostgreSQL catalog cutoffs around Push:
the outer notification/Push/VAPID/PubSub custody reader and the legacy VAPID
key provisioning write. Also remove the VAPID fallback's database access when
lineage attestation fails. Keep supported SQLite and XMPP wire behavior. The
custody read and provisioning write are separate transactions in one PR.

## Starting boundaries

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
- The failed-lineage startup branch calls `load_or_ephemeral`, whose current
  `try_new` runs schema DDL and can invalidate the Push marker despite its
  no-write documentation. Its unready object graph needs only an ephemeral
  signer, not a database read or write.

## Implemented transaction contract

1. Start the outer custody read through the opt-in idle-bounded catalog helper on
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
   Both opt-in catalog helpers cap the server's idle transaction lifetime
   before acquiring catalog-S/X, preserve tighter existing settings and pair
   that server bound with a client deadline. The bound applies to these
   operation transactions, not the earlier SQLx checkout hook.
4. On the failed-lineage branch, construct an ephemeral signer without a
   Database, root key, environment input or SQL. It is an unpersisted
   placeholder for a permanently unready node, never serving authority.

## Verification and exclusions

Require PostgreSQL 17 public-entry tests for catalog-first order, lower-gate
retry, relation and role drift, cancellation, exact backend, bounded rollback,
concurrent provisioning and unknown COMMIT. Run affected SQLite regressions,
formatting, strict Clippy and independent exact-head review. A checkout-hook
rollback cannot alone prove an operation-level reader retry; use a stable
physical witness or narrow that claim.

The independent architecture gate approved this grouped dark scope only with
the ordered per-gate retry and revised first-key/uncertain-commit contract
above. It did not approve a serving or release claim.

This VAPID fallback repair does not make all failed-lineage startup read-only:
registration, Push, outbox and activity constructors still initialize their
stores around it and remain separate work.

Multi-session MAM, notification-activity, inbox and pending constructors,
raw old-writer cutoff, provider roles, selected P2.1, production SM and
release qualification remain outside this group. `MUC open()` remains three
commits. GitHub Actions and workflow files remain absent until all #2046
implementation is finished and `cuenv sync -A` restores them.

### Local verification

On the integrated tree, PostgreSQL 17 was required for the relevant cases:
PubSub/Push custody integration 6/6 (including owner-prepared positive read,
all four ordered gates and two lost-reply proxy schedules); VAPID first-key
integration 10/10 in default parallel execution (including one subprocess
dispatcher and a URL precedence test); strict Push/VAPID owner and attach
library tests 2/2. XEP-0357 Push regressions passed 7/7, and VAPID storage
unit tests passed 10/10. The no-default server build, workspace formatting
and diff checks passed. Independent source review found no actionable issue.
Strict server Clippy with `-D warnings` remains blocked by the same 21
pre-existing unused/dead-code diagnostics; this group introduced none.

The loopback proxy witnesses ran against a local PostgreSQL 17 target. They
observe exact backend catalog-lock retention and later server-side release;
client socket closure alone was not counted as release. They do not prove
cross-primary failover reconciliation, every checkout hook, or whole-startup
no-write behavior. Actions and workflow files remain disabled, so no hosted
CI, exact-release or deployment qualification is claimed.
