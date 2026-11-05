# P2.1 serving owner startup on one PostgreSQL main pool

Plan for #2116 under #2046. This slice changes the **serving** PostgreSQL owner
startup: the main stores and ingress authority use one physical main pool with
separate main and ingress admission budgets. The optional control-plane pool
remains separate. SQLite keeps its existing shared handle. This is pool and
transaction-boundary progress, not restricted runtime authority or activation.
Rust paths below are under `server/crates/waddle-server/src/` unless specified.

## Implementation order and ownership

1. In `db/database.rs`, `db/pool.rs`, `db/backend.rs` and `main.rs`, add a private
   owner bootstrap path that opens one PostgreSQL pool with a physical maximum
   equal to checked `main_connections + ingress_connections`. Keep its handle
   private while global owner migrations and MAM preparation run; only then
   seal it into disjoint, non-borrowing main and ingress admission lanes and
   construct `DatabasePool`. Validate inherited-only store locations before
   owner mutation. Reject zero, overflow and capacity violations.
   Do not hand an unlaned clone to an actor or serving store. Retain the
   separately budgeted control-plane pool and the existing SQLite path.
2. Prepare MAM on an owned session before sealing the lanes. The existing
   `SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection` is usable
   only after a verified physical identity, owner role, lineage and complete
   migration ledger have been established on that session, with lineage then
   ledger session locks held before MAM takes its own lock. A completed
   `bootstrap_global_owner` call does **not** retain those locks. Build this
   private pre-lane gate with independently trusted inputs, or use the
   existing `bootstrap_dark_postgres_mam_owner` protocol on a temporary
   privileged session that closes before serving; never call the
   connection bootstrap directly after global migrations. Its autocommit DDL
   can leave a partial or unknown outcome. On error or cancellation discard
   the owned session and fail startup; any retry needs backend fencing and
   catalog/counter reconciliation, not an automatic replay.
3. In `mam_runtime/database_storage.rs`, add an explicitly owner-only
   `MamStorage` attachment over the sealed `Database` main lane. Keep the
   existing attested attachment and its capability checks strict. Reject an
   attested handle in the owner constructor, verify the prepared schema and
   owner, and use `Database::begin()` plus the transaction's pinned PostgreSQL
   connection for each standalone operation. Reuse the schema-qualified
   connection-taking MAM functions in `waddle-xmpp`; do not export or clone a
   general `PgPool`. Clustered fenced writes must retain claim-before-archive
   lock order and require the selected claim and MAM schema to match. Keep
   SQLite's portable fallback.
4. In `ingress/contract.rs` and `server/http.rs`, give normal PostgreSQL
   `IngressAuthority` the reserved ingress lane instead of reopening its DSN.
   In `server/http.rs` and `server/lifecycle.rs`, replace the serving inbox,
   MAM, PubSub, spaces and channel-link `open(URL)` branches with same-handle
   owner preparation and attachment. Keep their lineage aliases tied to the
   shared Global store. Require inherited global location for each shared
   store; reject explicit or fallback URLs before opening a child connection.
   Audit every startup constructor and raw pool escape in the resulting graph.
   SM and pending delivery already use the supplied global handle and must
   continue to do so.

## Transaction and safety gates

- `MamArchiveRepository` already writes archive rows on the ingress unit of
  work's pinned connection. Keep receipt, archive, inbox, SM and pending
  effects inside that caller-owned transaction. A standalone `MamStorage`
  method opens its own main-lane transaction and must never be called while
  holding an ingress transaction to create a second checkout or a false
  atomicity claim. Do not wait on a network operation while a transaction or
  admission permit is held.
- With `main=1` and `ingress=1`, the physical main pool has two slots. A
  one-slot total pool cannot provide both disjoint reservations. Main-lane
  saturation must not prevent a bounded ingress checkout; the converse must
  also hold. Preserve acquisition and statement bounds, and reconcile a
  commit-indeterminate result by authoritative readback before effects.
- Keep `RestrictedStartupError::UnconvertedStores` before listeners. Epoch
  remains at its installed legacy state; the V1009 row guard, runtime role
  split, relation-guard transition, old-session/writer exclusion and owner-only
  room-effect, Push, notification and clustered SM paths remain separate
  blockers. The owner login is not a restricted authority. Do not edit CI,
  workflow files, deployment configuration or XMPP wire behavior in this PR.
- **Implementation gate:** do not cut over owner MAM startup until the trusted
  bootstrap inputs and held-lock proof in step 2 are available. If they are
  absent, stop the affected serving conversion; a schema snapshot or completed
  global migration is insufficient evidence for the dark initializer.

## Acceptance evidence

- PostgreSQL startup with inherited locations serves through one exact main
  backend plus its reserved ingress lane, with no serving child-pool DSN
  reconnect. Wrong/explicit store locations fail before mutation. Check the
  configured physical maximum and separate control-plane capacity.
- Exercise a `main=1`, `ingress=1` fixture: hold the main permit while a
  direct ingress archive/receipt transaction commits, and hold the ingress
  permit while a standalone MAM operation completes. Prove the ingress path
  does not request a main-lane permit, and standalone MAM does not request an
  ingress permit. Assert bounded timeout behavior when each lane is full.
- For direct and recovery paths, prove archive and receipt visibility only
  after the same commit, rollback of both on failure, and authoritative
  readback after an indeterminate commit. Cover inbox, SM and pending writes
  in the same transaction where their contracts require it. Check clustered
  MAM fencing and lock order under concurrent writers.
- Verify owner MAM preparation and attachment across fresh, already prepared,
  malformed and wrong-owner schemas; test cancellation/unknown completion as
  a startup failure without automatic retry. Verify the attested constructor
  still refuses the owner-only path and restricted startup still fails before
  listeners.
- SQLite in-memory and file-backed startup retain the same pool and their
  current transactional behavior. Reverting the implementation can restore
  the former owner startup path; a failed startup must not silently fall back
  to a different pool within one process.

Code review and targeted PostgreSQL/SQLite tests establish this serving-path
slice only. They do not establish runtime role isolation, epoch activation,
old-writer fencing or production behavior.
