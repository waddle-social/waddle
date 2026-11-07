# P2.1 exact runtime store ACL checks

Status: draft, dark restricted-profile hardening. Base: main `d6b962d4`.
This grouped slice tightens the existing Inbox, SM and Pending Delivery
attachment verifier. It does not complete the all-store role graph, convert
detached SM raw writers or select restricted startup.

## Problem and boundary

`runtime_store_acl` currently treats `false` INSERT, UPDATE and DELETE flags
as absence of a requirement on writable relations. A role with extra table
or column DML can pass attachment, including UPDATE on SM rows that should be
insert/delete only. The current combined fixture grants those extra rights.

Verify both sides of each relation's required effective privilege set:
SELECT is required; each DML flag requires the corresponding privilege when
true and denies it when false, including column-level INSERT and UPDATE.
Keep the existing role, owner, PUBLIC, grant-option, schema, sequence and
executable-object constraints. Reconcile test fixture grants with the
operations the corresponding runtime store actually performs; fail closed
if an operation needs a privilege the profile denies. Do not grant R access
to the 13 protected ingress tables or broaden definer APIs.

## Acceptance

- PostgreSQL 17 positive actual-role attachment for affected Inbox, SM and
  Pending profiles with the exact required rights, plus negative table and
  column grants for every false permission and PUBLIC/grant-option drift.
- Existing SQLite and PostgreSQL attachment regressions, formatting,
  strict scoped Clippy with unchanged baseline recorded, and independent
  adversarial review of the final diff.

This verifier is a point-in-time read under the caller's relation locks.
Privileged role/ACL mutation and old writer sessions still need a continuous
cutover fence. The all-store role matrix, source/recipient custody, pooled
pinned transaction conversion and restricted serving remain open under
#2116/#2046. GitHub Actions and workflow files stay disabled until the full
#2046 implementation is finished.
