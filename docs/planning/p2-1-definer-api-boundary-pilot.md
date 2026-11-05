# P2.1 dark SECURITY DEFINER API boundary pilot

This pilot develops an unregistered PostgreSQL 17 candidate for replacing
caller-writable epoch and transaction GUCs as the ingress authorization
boundary. It does not change startup, the migration catalog, deployed roles,
credentials, or runtime call sites.

## Planned slice

- Add candidate SQL for a dedicated `NOLOGIN` API owner and a narrow,
  operation-specific canonical-message/origin-alias write function, including
  the optional immutable source-principal binding supported by the staged
  source-aware repository path.
- Give the function a fixed safe `search_path`, schema-qualified table and
  helper references, and no dynamic SQL or caller-selected relation names.
- Keep the relation guard `SECURITY INVOKER`; it accepts writes only while
  `current_user` is the dedicated API owner. The API function establishes the
  existing epoch/xid transaction-local values inside the caller's transaction.
- Model runtime access with function `EXECUTE` only for guarded writes: no
  guarded-table DML and no membership that allows `SET ROLE` to the owner.
- Add a disposable PostgreSQL 17 fixture for composed writes, replay/conflict,
  rollback, concurrent alias creation, forged-GUC direct-DML rejection, and
  role/ACL/catalog assertions.
- Keep the artifact dark: no migration registration, startup installation,
  activation, credential rollout, or broad Rust caller cutover.

## Acceptance evidence

The positive case must execute inside an already-open outer transaction and
write the canonical message and alias atomically. Replaying the same alias
returns the same canonical identity; a conflicting alias or a later outer
rollback leaves no partial rows. The function asserts an optional source
principal against a locked, unexpired session row; if a stored source binding
exists, its principal must match, while a legacy unbound row remains replayable.
Alias hash hits must match the stored sender/target/origin tuple. Digest changes
return the stored key and digest as a conflict, and concurrent first writers
must converge on one identity with an unreferenced candidate removed.

The operation bounds the SQL write shape and checks a supplied source tuple
against a durable session; it does not authenticate the caller or bind that
tuple to the shared PostgreSQL login. The application must supply the tuple
from an already-authenticated ingress UOW. The tuple may be omitted for legacy
unbound flows. PostgreSQL requires `UPDATE` on at least one session column for
the function's `SELECT ... FOR SHARE`; the API owner receives that narrow
column privilege only for row locking and does not update session data.

The function accepts the existing UOW's precomputed alias hash; it validates
the full tuple on replay but does not recompute SHA-256 in SQL. The hash remains
an input from the Rust alias-key derivation boundary. The fixture uses an
explicit first-lock sequence in the same outer transaction; it does not claim
integration with `IngressUnitOfWork::begin_with_relation_guard`, whose current
catalog verifier expects a complete all-table candidate.

The negative case connects as the eventual runtime role, forges both GUCs,
acquires the epoch lock, and still cannot write any guarded table directly.
The function's EXECUTE ACL is restricted to the runtime role and function
owner; the API owner is `NOLOGIN` and has no role memberships. Only the fixed
operation-specific function is executable. The trigger observes the definer
owner's identity as `current_user` while retaining the runtime `session_user`.

## Cutover boundary

The pilot is not proof of production readiness. Before a credential transition,
all guarded writes and lock-taking reads must be covered by operation-specific
functions, raw writer pools and arbitrary SQL paths must be removed, and old
writer sessions must be drained/fenced before the guard and grants change.
The fixture applies the two-table guard replacement and runtime DML revoke in
one epoch-exclusive transaction, then tests the transition barrier. This proves
the fixture contract only; it does not drain production sessions or solve
failover. The fixture administrator performs the prototype function ownership
transfer; production role provisioning and ownership transfer authority remain
rollout prerequisites. Transaction commit errors remain indeterminate; callers
must preserve the existing transaction outcome/reconciliation rules.
