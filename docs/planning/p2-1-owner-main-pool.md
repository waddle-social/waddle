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
2. Prepare legacy owner MAM on an owned session before sealing the lanes.
   `SqlxMamStorage::open` currently creates a separate ten-connection pool and
   runs `bootstrap_postgres_connection(conn, None)` under its MAM advisory lock.
   Expose that exact legacy operation through a clearly named consuming
   connection API and call it using the unlaned main pool's detached startup
   session. Before sealing, run the existing legacy Push Service schema
   initializer followed by notification outbox preparation on that same pool:
   outbox ancestry adoption consults Push Service publish jobs. Drop both
   temporary unlaned store handles. Serving startup then attaches the outbox
   to the sealed main lane without rerunning its detached-session initializer.
   MAM discards its owned session on error or cancellation; the outbox
   initializer closes its detached gate session on completion or error.
   Preparation must fail this startup attempt on error. A supervisor restart
   can re-enter these legacy initializers;
   ambiguous DDL requires operator reconciliation. This path makes
   **no** independently attested identity or owner/runtime role claim. The
   separate dark `bootstrap_postgres_schema_on_owned_connection` remains
   gated: it requires verified physical identity, owner role, lineage and
   complete ledger on one session, with lineage then ledger session locks held
   before MAM's lock. A completed `bootstrap_global_owner` does **not** retain
   those locks; never call the dark initializer directly after it.
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
- **Implementation gate:** the owner path may use only the legacy-equivalent
  initializer in step 2. Any use of the dark initializer still requires its
  independently trusted inputs and held-lock proof. Neither path may silently
  substitute for the other after an error.

## This PR's acceptance evidence

- PostgreSQL owner bootstrap with inherited locations prepares MAM, Push and
  outbox on the one pre-lane main pool, then seals it at the checked
  `main + ingress` physical maximum. A prepared outbox token binds the
  sealed serving handle to that exact pool. A retained unlaned clone or the
  tested malformed candidate view fails startup. This legacy initializer
  does not attest every preexisting outbox column type.
- At `main=1`, `ingress=1`, hold each lane while the other checks out,
  verify bounded saturation waits and a standalone MAM read on the main lane.
  Inspect serving call sites for child DSN reopens and nested checkout.
- Explicit store overrides fail before PostgreSQL owner mutation. Owner
  preparation cancellation releases its detached session and unlaned clones.
  Restricted startup still fails before listeners.
- Run focused PostgreSQL and SQLite regressions, formatting, scoped Clippy
  and an independent final diff review. The SQLite startup path remains the
  existing path.

## Verification completed

- PostgreSQL 17 shared-owner integration: 4 passed, including lane isolation,
  sealed outbox read/prune, malformed candidate-view rejection, and an
  explicit override rejected before `_lineage` mutation.
- PostgreSQL 17 outbox cancellation regression: 1 passed. SQLite in-memory
  pool regression: 1 passed.
- Library and binary `cargo check`, `cargo fmt --all -- --check`, and
  `git diff --check` passed. Scoped library/binary Clippy with all features
  passed using `-D warnings -A unused_imports -A dead_code` for existing
  unrelated unused/dead-code warnings; no source lint suppression was added.
- Independent read-only architecture and source reviews found no blocking
  regression. The final representation-only box change was reviewed by the
  integration owner. CI and production startup were not exercised.

## Remaining #2116 qualification

Direct and recovery archive/receipt atomicity, indeterminate-commit readback,
inbox/SM/pending write coherence, clustered MAM fencing under concurrent
writers, wrong-owner and unknown-completion migration cases, full SQLite
in-memory/file startup, runtime role isolation, epoch activation, old-writer
fencing and production behavior remain separate gates. Reverting this PR
restores the former owner startup path; this PR must never silently fall back
to a second pool within one process.
