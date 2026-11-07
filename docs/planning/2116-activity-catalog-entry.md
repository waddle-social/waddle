# P2.1 notification activity catalog entry

Status: draft, dark implementation for #2116 and #2046. Both restricted-startup
refusals remain active; this slice does not select serving authority.

## Objective

Move PostgreSQL notification-activity strict owner preparation, raw constructor
and its direct attested reader onto actual catalog-fenced transactions. The owner currently holds a
detached session gate while table DDL runs on sibling pool checkouts. The
completed marker therefore cannot be treated as one protected transaction.
The direct activity reader currently begins its transaction before acquiring
catalog-S. Preserve the SQLite initialization path and current startup policy.

## Contract

- On the PostgreSQL owner backend, acquire catalog-X before the existing
  two-integer activity exclusive gate, catalog/role reads, table DDL, and the
  completed owner marker. Run protected work on that backend and commit only
  after the completed marker is durable. Preserve the legacy raw PostgreSQL
  constructor's accepted credential policy while fencing its actual DDL
  backend. Validate any existing strict marker before raw mutation and leave
  it incomplete afterward; only strict owner preparation may publish a
  strict completion marker. Do not substitute an i64 advisory key
  for the two-integer gate. A contended lower gate requires acknowledged
  rollback and a fresh catalog transaction before retry. Unknown COMMIT must
  not be reported as success. Capture the configured effective schema with
  explicitly qualified builtins, then use a transaction-local `pg_catalog`
  first path before any operator-bearing catalog inspection. Verify the
  captured strict owner path and identity on that same backend.
- On the actual direct attested reader backend, begin READ COMMITTED READ ONLY
  and acquire catalog-S before the shared activity gate, relation locks,
  attestor and catalog checks. Keep relation identity and ACL checks inside
  that transaction. The `push_registrations` contract shares this reader
  implementation and its direct attach must also be tested when the helper
  is changed; its owner constructor is outside this slice.
- Retain existing data and table shape across fresh, repeated, interrupted and
  legacy-schema preparation. A failed or cancelled preparation cannot publish
  a completed marker for partial work. A completed marker cannot be inferred
  from a different physical schema. A lost COMMIT acknowledgement returns a
  typed indeterminate outcome and no store; a later marker=1 alone does not
  attribute that particular attempt. Retry only after the old backend outcome
  is resolved on the verified original primary and the catalog gate is
  reacquired.

## Verification and boundaries

Use native PostgreSQL 17 public-entry tests for owner/reader gate order,
contention, rollback, cancellation, marker and schema drift; run affected
SQLite regressions, formatting and strict Clippy. Test withheld COMMIT replies
and actual backend lock retention. Obtain an independent exact-head adversarial
review. The SQLx pool checkout hook can still execute before the caller's
catalog transaction, so this is not whole-pool or serving proof. The reader's
60-second client deadline is source-reviewed; the local proxy cases cover
owner COMMIT and pre-fence response stalls, not a withheld reader reply or a
COMMIT request held before PostgreSQL receives it. Privileged custom index
opclasses remain outside this cooperative catalog boundary.
Inbox, pending delivery and MAM each have distinct multi-session initialization
and remain separate follow-ups. Old writers, privileged DDL, role/epoch
rollout, SM custody and exact-release CI remain gated. GitHub Actions and
workflow files remain absent until #2046 implementation is finished.
