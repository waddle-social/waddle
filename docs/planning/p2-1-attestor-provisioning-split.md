# P2.1 dark attestation installer split

Status: dark component stacked on #2154. Normal startup, CNPG configuration, runtime credentials, pools and epochs are unchanged.

## Provisioning and trust boundary

A separate operator must provision three distinct roles: a restricted NOLOGIN definer, an installer LOGIN allowed to `SET ROLE` to that definer without INHERIT or ADMIN, and a restricted runtime LOGIN with no role memberships. The operator also provisions a definer-owned trusted schema, grants the definer USAGE and `_lineage` SELECT in the target schema, and grants it EXECUTE on `pg_catalog.pg_control_system()`. The installer creates no roles and grants no control-function or lineage privileges.

The runtime and installer must not own the database, target schema or lineage table; create in the target/trusted schema or database; create TEMP relations; write any lineage column; or execute `pg_control_system()` directly. A stock PostgreSQL 17.11 fixture granted PUBLIC EXECUTE on that system function by default. The isolated test database explicitly revokes it before granting the definer access. Real provisioning must assess and perform that database-wide change outside the installer.

The installer checks the real session and current login, role attributes and membership graph, ownership, effective privileges, target schema and trusted-schema ACLs. It forces READ COMMITTED before taking the existing lineage advisory lock and a database-local installation lock for the trusted schema, in that order. Only after this preflight does it switch locally to the definer and read the qualified lineage and live physical identity. It refuses a standby but cannot prove primary fencing or failover durability.

## Installation and recovery

The function, restricted ACL and operation marker commit in one transaction. The marker is a comment on the function containing the caller-supplied operation UUID and a versioned SHA-256 of the expected identity, roles, schema and exact source. It contains no credentials. Before calling an existing function, the installer checks the marker, exact SQL source, argument and result metadata, language, owner, security mode, configuration and ACL. An unexpected function, altered definition or different operation is a conflict and is never replaced. PUBLIC execution is revoked before commit.

A same-UUID, same-specification retry on the same fenced primary can safely converge while another cooperative attempt is queued: every attempt takes the same resource lock before inspecting or mutating the function, and checks the committed marker before acting. The lock is local to a database; it does not serialize external CNPG role reconciliation or legacy installers. A successful retry proves the requested final state, not the historical result of a previous COMMIT.

A failed or timed-out COMMIT reports `CommitUnknown` with the operation UUID. A confirmed COMMIT followed by failed fresh readback reports `PostCommitUnverified`. Fresh inspection returns `Committed` only after exact validation and identity readback; absence is `Indeterminate`, never proof of rollback. A timeout while acquiring the shared lock is also indeterminate. No automatic destructive repair, negative rollback assertion or tombstone protocol is supplied.

## Evidence and remaining gates

Disposable PostgreSQL tests use distinct administrator, restricted installer, NOLOGIN definer and runtime logins. They exercise role and direct/column-grant drift, inherited default ACLs, a spoofed existing definer function with a side effect, same-ID retry and conflicting IDs, two installers with repeatable-read defaults queued behind the lineage lock, cancellation and timeout during a lock wait, and a proxy that discards an actual COMMIT acknowledgement after the marker is visible. The latter resolves with fresh exact inspection and a same-ID retry.

This does not provision roles in CNPG or authenticate the server transport. Physical identity values are not secrets and can be shared by physical replicas. An authenticated transport, independent expected-identity handoff, confirmed authoritative primary and old-primary fencing, operator control of role drift and legacy installers, runtime pool wiring, and complete owner-only startup remain external gates. The pool hook still checks identity on each physical connection and checkout; this change does not make role/catalog audits continuous.
