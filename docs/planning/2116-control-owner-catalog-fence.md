# P2.1 control and owner catalog-fence group

Status: scoped implementation. Restricted startup continues to refuse with
`UnconvertedStores` before listeners and effectful workers. This work does not
enable serving or establish a complete privileged-mutator cutoff.

## Objective and scope

The converted entries are `run_dark_postgres_owner`, singleton schema
provisioning, physical-attestor installation and its observer, room-observation
raw and owner schema preparation, and Push raw and owner schema preparation,
VAPID owner preparation, and raw and owner notification-ancestry adoption.
These paths use catalog-first custody on the backend that executes their
PostgreSQL mutation. SQLite retains its previous transaction entry behavior.

The owner-rejection grant helper is not fenced by this group. Raw VAPID
initialization still uses separate autocommit checkouts. The
room-observation attested verifier still lacks a catalog-shared read fence.
PubSub, paired spaces, room effects, MUC, call teardown, keypool, inbox,
pending delivery, MAM, and notification activity/outbox are outside this
group. The multi-session initializers require a backend-wide or session
protocol; a catalog lock on their coordinator would not protect SQL executed
on another backend and could deadlock a pinned pool hook. Inventory all raw
and guarded entries before claiming any whole family converted.

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

Focused PostgreSQL 17 witnesses exercise the public converted entries and
observe actual catalog-lock attempts, rollback before retries, lower-gate
exclusion, cancellation, one-slot pools, and commit visibility. Installer
and observer fixtures exercise their dedicated-connection lock custody;
they do not establish hooked-pool behavior. The session-fenced
concurrent-index/old-snapshot witness remains unverified for this group.
Run focused SQLite regressions, formatting, strict scoped Clippy, and an
independent exact-head review before merging; record test results in the PR.

Push schema relations can wait up to 30 seconds after catalog-X acquisition.
This bounded interval can delay catalog readers and exhaust their pool checkout
deadline. It is not a substitute for a global exclusion proof against a
concurrent index build or old snapshot. Unknown COMMIT remains indeterminate.

Old binaries and login backends, prepared transactions, external
owner/superuser/role administrators, failover, and unconverted multi-session
constructors remain separate release gates. The two restricted-startup
refusals, cooperative-policy refusal, Actions pause and absent workflow files
stay in place until the full #2046 program is complete.
