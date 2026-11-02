# P2.1 owner-rejection grant laboratory

This is an unwired owner-side experiment. It does not change startup,
credentials, live ingress, retention or migration V1028. The grouped local
collector patch retains rejected source and child as a pair through both
retention tails, then deletes both in one transaction. Its PostgreSQL replay
and GC behavior remains unverified, so it does not authorize activation.

The operation takes the existing independently supplied migration-owner
identity and distinct runtime login. On a fresh owner session it checks the
physical lineage, owner/schema identity, V1028 checksum, relation shape,
mapping constraints and guard functions and triggers. It refuses an unsafe
runtime role, TEMP or schema CREATE privilege, unexpected table/column ACLs,
and effective or grantable privileges beyond SELECT and INSERT. It accepts
the migration's `pg_monitor` SELECT and does not repair unexpected grants. The
only mutation is a qualified SELECT and INSERT grant on the mapping table.
The grant is checked again before commit, so a failed post-grant check rolls
back the transaction; a fresh-session owner probe follows an acknowledged
commit. A missing observation or cancellation does not rule out a late
commit. Retry requires fencing the earlier backend/primary and an independently
quiescent DDL, role and ACL environment.

The disposable PostgreSQL lab assembles the existing attested pool with one
main and one ingress admission slot. It exercises the role grants, rejected
mutations and role/catalog drift, the attested authority constructor and
no-DSN-fallback behavior, TEMP/search-path interference, and restricted-role
child deletion through the existing foreign-key cascade. The checks are
scoped to this mapping and do not audit arbitrary security-definer functions,
all other runtime grants, or all consumers of the guarded database. Passing
them does not prove live ingress correctness, primary fencing, capacity,
production role provisioning, or P2.1 readiness.
