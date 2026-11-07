# P2.1 control and owner catalog-fence group

Status: draft implementation plan. Restricted startup continues to refuse with
`UnconvertedStores` before listeners and effectful workers. This work cannot
enable serving or claim a complete privileged-mutator cutoff.

## Objective and scope

Make reachable first-party, single-backend PostgreSQL control-plane and owner
mutations participate in one catalog-first transaction protocol. The candidate
group is attestor installation and its observer, alternate migration owner and
its scoped rejection grants, and the transactional raw/owner constructors for
room observation, PubSub, Push, paired spaces, room effects, MUC, call
teardown, keypool and singleton ownership. Inventory every callable raw and
guarded entry before claiming a family converted. Keep SQLite behavior and
legacy owner startup intact.

The independent architecture gate may narrow this group when a constructor
uses another checkout, an autocommit statement, a session lock, or a detached
coordinator. Detached MAM and notification activity/outbox multi-session
initializers are excluded: a catalog lock on their coordinator would not
protect SQL executed on another backend and could deadlock a pinned pool hook.

## Transaction protocol

Finish checkout and all hooks before obtaining catalog-exclusive. Use a fresh
READ COMMITTED transaction and nonblocking catalog-X as its first protected
action; then take existing lineage, ledger, installer, epoch, schema or owner
locks in their established order. Move role/catalog prechecks inside that
fenced transaction. Keep catalog-X on the exact mutation backend until actual
PostgreSQL transaction end. A lower-lock blocker must trigger bounded,
acknowledged rollback before backoff and a fresh attempt. Mark the checkout
for discard before cancellable `BEGIN`; discard any transaction whose end is
not acknowledged. Do not acquire a second hooked checkout while holding
catalog-X or issue `pg_advisory_unlock_all()` on a guard-bearing session.

An unknown COMMIT remains indeterminate. A closed socket, missing marker, or
apparently idempotent retry does not establish rollback. Preserve operation
identity when present, and require original-backend/primary fencing and
trusted reconciliation before retrying where it is absent.

## Verification and non-goals

Use PostgreSQL 17 tests on actual public entries: a held catalog reader must
prevent lower locks, DDL and grants; lower-lock contention must release
catalog-X with no stranded snapshot; actual commit/rollback visibility and
fresh post-wait observations must match the chosen order. Cover cancellation,
pool size one, hook interaction, and a session-fenced concurrent-index/old-
snapshot witness for the participating path. Run focused SQLite owner
regressions, formatting, strict scoped Clippy and independent exact-head
review. Record every unverified witness explicitly.

Old binaries and login backends, prepared transactions, external
owner/superuser/role administrators, failover, and unconverted multi-session
constructors remain separate release gates. The two restricted-startup
refusals, cooperative-policy refusal, Actions pause and absent workflow files
stay in place until the full #2046 program is complete.
