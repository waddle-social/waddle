# Outbox startup DDL repair

The final #1942 rollout was healthy, but one pod retried after a PostgreSQL
schema-object catalog collision during concurrent notification-outbox startup.
Current initialization unconditionally drops and recreates the queued coalescing
index in separate statements. Concurrent constructors can collide, and an already
serving writer can observe an absent uniqueness index.

## Implementation and verification plan

1. Add native regressions before changing production code: preserve a current
   index across repeated startup, exercise simultaneous PostgreSQL constructors
   on fresh/current/legacy schemas, and preserve frozen payloads, uncertainty and
   lineage through an index upgrade.
2. Serialize PostgreSQL outbox schema initialization with a dedicated advisory
   gate held by a physical connection detached from the exact existing pool.
   This preserves external-pool/search-path setup and leaves normal pool slots
   available for schema helpers and canonical transactions, including pool size
   one. Close the session on completion, failure and cancellation; never return a
   session advisory lock to a pool. Bound the new lock wait. Document the temporary
   extra startup connection and verify cleanup. Own schema initialization in a
   task that finishes under the gate if its caller is canceled; do not release
   the gate while a separately pooled DDL statement is still completing. Use a
   fixed database-local key so search-path aliases cannot select different gates.
3. Inspect the resolved queued index. Keep a current valid unique definition
   unchanged. Replace only a missing/stale definition and remove the legacy
   active index within one transaction, so serving writers cannot see an
   autocommit uniqueness gap. Preserve the exact queue/payload/uncertainty filter.
4. Keep canonical locks, quarantine, ancestry adoption and wire/guest contracts
   unchanged. Review the design before implementation and the integrated diff
   afterward; run native SQLite/Postgres regressions, formatting, strict clippy
   and required CI.
5. Merge and publish normally through GitOps. Preserve UUID and observer
   generation 2. Verify every pod source/image digest, readiness, restart counts,
   index validity and custody. This server change advances the cutover floor and
   must finish a Recreate rollout before a separate RollingUpdate flip.

Do not solve pool exhaustion by using the control-plane pool or reopening a DSN;
external pools intentionally do not retain a usable DSN. Do not wrap independent
schema/UoW helper transactions inside a pooled outer transaction. Do not rewrite
notification data or weaken uniqueness to make startup succeed.
