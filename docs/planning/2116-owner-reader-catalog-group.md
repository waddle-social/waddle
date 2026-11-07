# P2.1 owner and reader catalog group

Status: draft plan for #2116 and #2046. Both restricted-startup refusals
remain active. This group cannot select serving authority.

## Objective

Close the next actual single-transaction catalog edges under one grouped PR:
paired spaces-metadata/channel-space-links owner preparation and attested
verification, PubSub strict owner and attested verification, room-effect and
MUC core owner/reader entries, and strict call-teardown owner/reader entries.
The independent architecture gate must confirm each selected public entry's
checkout and transaction count before implementation. A family is complete
only when every raw, owner and attested path that can reach its protected
relations participates or is explicitly refused.

## Lock and transaction contract

- PostgreSQL owner mutations start one transaction on the backend that will
  execute the protected DDL/DML. Acquire catalog-X before store advisory
  gates, name-based relation reads, role/catalog checks and DDL. Try each
  subordinate gate in the repository's global order; acknowledge rollback
  and discard uncertain BEGIN/rollback connections before retrying.
- Attested readers start a READ COMMITTED READ ONLY transaction on the actual
  read backend. Acquire catalog-S first, then the existing lineage/ledger,
  store and relation locks in a reviewed order. Do not run an attestor or H
  helper before the catalog proof. Recheck catalog identity after any wait.
- Preserve each owner's original backend and unknown-COMMIT semantics. Never
  use a coordinator lock while sibling checkouts perform writes, and never
  claim a raw pool is authorized by another pool's receipt.

## Boundaries

The PubSub compatibility initializer, MUC `open()` composition, MAM, inbox,
pending, notification and other multi-session constructors need separate
custody work unless the architecture gate proves their whole write sequence
can be made one fenced transaction. Raw VAPID key load/insert remains a
separate data-path conversion after #2258's schema DDL. #2260's pinned
receipt worker is test-selected and remains separate. Do not relax either
startup refusal, old-writer cutoff, provider role boundary, production SM
custody, or exact-release gate. GitHub Actions and workflow files stay absent
until #2046 is complete and `cuenv sync -A` restores them.

## Verification

Add focused PostgreSQL 17 actual public-entry witnesses for catalog-first
lock order, lower-gate contention with acknowledged rollback, cancellation,
catalog replacement, exact relation/role identity and post-commit visibility.
Run affected SQLite behavior tests, formatting, strict Clippy and an
independent exact-head adversarial review. Record baseline diagnostics and
unconverted entry points explicitly; dark fixture success is not serving
qualification.
