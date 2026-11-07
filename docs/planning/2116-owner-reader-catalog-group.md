# P2.1 owner and reader catalog group

Status: grouped dark implementation for #2116 and #2046. Both restricted-startup
refusals remain active. This group cannot select serving authority.

## Objective

Fence the next bounded owner and reader transaction entries under one grouped
PR: paired spaces-metadata/channel-space-links preparation, raw initializers
and attested verification; PubSub legacy and strict owners and all direct
attested reader variants; room-effect strict/raw owners and readers; MUC core
strict/raw bootstraps; and the destroy-completion owner/reader used by the MUC
composition. The outer push-notification custody reader remains uncovered:
its notification → Push → VAPID → PubSub order needs a separate catalog-first
entry change. Consolidate call-teardown's
three-checkout PostgreSQL compatibility initializer into one actual fenced
transaction before claiming its owner family converted; its strict owner and
attested reader can share the same PR.

The independent architecture gate found these entries are distinct
transactions. Fencing each MUC `open()` constituent does not make its three
commits atomic, and this work does not claim a complete store-family or
selected-serving conversion until every raw/owner/attested path is covered.

## Lock and transaction contract

- PostgreSQL owner mutations start one transaction on the backend that will
  execute the protected DDL/DML. Acquire catalog-X before store advisory
  gates, name-based relation reads, role/catalog checks and DDL. Try each
  subordinate gate in the repository's global order; acknowledge rollback
  and discard uncertain BEGIN/rollback connections before retrying.
- Attested readers start a READ COMMITTED READ ONLY transaction on the actual
  read backend. Acquire catalog-S as its first statement, then the existing
  lineage/ledger, store and relation locks in a reviewed order. No operation
  attestor, PubSub H helper, or protected catalog read runs before that
  transaction's catalog proof. Existing SQLx checkout hooks can run their
  attestor before the caller begins; that separate hook-custody gap remains.
  Recheck catalog identity after any wait. Remove later REPEATABLE READ
  switches from converted readers rather than pinning an old snapshot.
- Preserve each owner's original backend and unknown-COMMIT semantics. Never
  use a coordinator lock while sibling checkouts perform writes, and never
  claim a raw pool is authorized by another pool's receipt.

## Boundaries

The MUC `open()` composition still commits core, destroy and effects
separately. MAM, inbox, pending, notification and other multi-session
constructors need their own custody design; a catalog lock on a coordinator
does not cover sibling checkouts. Raw VAPID key load/insert remains separate
after #2258's DDL. #2260's pinned receipt worker is test-selected and
separate. PubSub's outer push-notification custody reader cannot acquire
catalog-S inside the PubSub helper after notification/Push locks. Call
teardown's compatibility PostgreSQL initializer cannot remain three
unfenced checkouts if this PR claims its raw owner covered. Privileged
mutators that bypass the cooperative catalog protocol remain an external
cutover gate. Do not relax either startup refusal, old-writer cutoff,
provider role boundary, production SM custody, or exact-release gate. GitHub
Actions and workflow files stay absent until #2046 is complete and
`cuenv sync -A` restores them.

## Verification

Add focused PostgreSQL 17 actual public-entry witnesses for catalog-first
lock order, lower-gate contention with acknowledged rollback, cancellation,
catalog replacement, exact relation/role identity and post-commit visibility.
Run affected SQLite behavior tests, formatting, strict Clippy and an
independent exact-head adversarial review. Record baseline diagnostics and
unconverted entry points explicitly; dark fixture success is not serving
qualification.

The PubSub and paired-space attested reader contention witnesses cannot by
themselves distinguish an SQLx checkout-hook rollback from an operation-level
catalog retry. They establish public-entry refusal of committed drift. The
MUC/destroy reader wrapper observes completion after catalog-X release but
cannot prove that the operation attempted catalog-S before that release.
Source ordering and the call-teardown blocked-relation witness support the
transaction-order review. These limits are not serving proof.

## Local verification

On the integrated tree, PostgreSQL 17 and SQLite focused suites passed:
PubSub runtime attach 4/4, spaces metadata 11/11, call-teardown physical
attestation 3/3, and room-effect runtime attach 1/1. Strict server Clippy
with `-D warnings` still reports 21 pre-existing unused/dead-code diagnostics;
no new diagnostic arose from this group. GitHub Actions and workflow files
remain disabled, so this is not exact-release CI or clustered qualification.
