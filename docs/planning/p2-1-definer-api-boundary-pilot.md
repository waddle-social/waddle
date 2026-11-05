# P2.1 dark SECURITY DEFINER API boundary pilot

This pilot develops an unregistered PostgreSQL 17 candidate for replacing
caller-writable epoch and transaction GUCs as the ingress authorization
boundary. It does not change startup, the migration catalog, deployed roles,
credentials, or runtime call sites.

## Planned slice

- Add candidate SQL for a dedicated `NOLOGIN` API owner and a narrow,
  operation-specific canonical-message/origin-alias write function.
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
rollback leaves no partial rows. Concurrent first writers must converge on a
single identity or return the existing typed conflict outcome.

The negative case connects as the eventual runtime role, forges both GUCs,
acquires the epoch lock, and still cannot write any guarded table directly.
Only the fixed operation-specific function is executable. The trigger observes
the definer owner's identity as `current_user` while retaining the runtime
`session_user`.

## Cutover boundary

The pilot is not proof of production readiness. Before a credential transition,
all guarded writes and lock-taking reads must be covered by operation-specific
functions, raw writer pools and arbitrary SQL paths must be removed, and old
writer sessions must be drained/fenced before the guard and grants change.
Transaction commit errors remain indeterminate; callers must preserve the
existing transaction outcome/reconciliation rules.
