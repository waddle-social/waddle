# P2.1 dark guard and role-boundary laboratory

Status: experimental candidate and disposable tests, stacked on #2153. The SQL fixture is not registered in the migration catalog, called by startup, or a deployable role provisioner. This work does not change runtime credentials or deployment manifests, flip an epoch, or complete P2.1.

## Candidate boundary

The fixture replaces the existing PostgreSQL guard functions only inside its isolated test database. On each guarded statement it rejects isolation other than READ COMMITTED, resolves the epoch relation in the trigger table's schema, and checks that this backend already holds `AccessShareLock` on that relation. It then reacquires that same relation lock and executes a separate nonlocking epoch read in a VOLATILE function. Epoch and transaction-ID proof are required even at epoch zero. Legacy writers which omit either the prelock or proof therefore cannot be rolled alongside this candidate.

A transition must begin READ COMMITTED, take `ACCESS EXCLUSIVE` on the schema-qualified epoch relation before the UPDATE, and avoid protected-table DDL or DML until it commits. The row guard checks the previously granted lock rather than upgrading a row lock. The existing transaction-local GUC sentinel catches an accidental second advance in one transaction; it is not a boundary against an owner that can reset the GUC or disable its own triggers. Likewise, the runtime-set epoch proof is a consistency check, not authentication of arbitrary SQL submitted by a compromised runtime.

The statement trigger runs after PostgreSQL locks the DML target. Failing immediately when the epoch lock is missing avoids waiting there behind an exclusive transition. `pg_locks` establishes that the actual epoch lock is currently held; it cannot establish that a caller acquired it before any application locks. All production writers need a lexical transaction capability that enforces this ordering. The candidate intentionally does not provide such a capability or switch any caller.

## What the laboratory tests

Each real-PostgreSQL test creates a unique database, a NOLOGIN owner, a separate migration LOGIN which may assume that owner, and a runtime LOGIN with no owner or migrator membership. The fixture applies the real migration runner through V1027, installs the candidate separately, and grants the runtime role DML on the 12 manifest tables plus epoch and manifest SELECT. It revokes PUBLIC TEMP on its own database and checks effective role membership, SET ROLE, object ownership, schema CREATE, epoch column UPDATE, TEMP, trigger-disable and protected-table permissions. It never modifies these privileges in the shared test database.

The tests exercise zero-row INSERT, UPDATE and DELETE statements on all 12 actual tables at epochs zero and one, including missing prelocks, caller-supplied GUCs, missing or stale epoch proof, stale transaction ID and valid proof. They also exercise the TRUNCATE triggers, a direct owner UPDATE without the required exclusive lock, a duplicate advance, identified concurrent lock holders and waiters, a post-wait fresh epoch read, a repeatable-read snapshot fixed before transition, and savepoint lock release.

A separate negative experiment grants TEMP temporarily in the disposable database and creates shadows of all 12 guarded tables. Even with an actual epoch prelock, an unqualified write reaches the TEMP table and bypasses the real trigger. A spoofed TEMP epoch also does not change the qualified guard's result. Revoking TEMP prevents new TEMP creation but does not remove shadows already held by an existing session; draining old sessions and binding all correctness SQL to the intended objects remain required.

## Gates not implemented

- The migration runner applies all pending migrations in one transaction. It can hold protected-table DDL locks before reaching this candidate, and an old writer can have taken target-table locks before the new guard exists. Installation and subsequent protected-table migrations need an enforced quiescent owner-only protocol, old-session termination, reconnect fencing, and old-image admission control. The SQL artifact by itself is not that protocol.
- The existing physical-attestation installer still creates its NOLOGIN definer role and grants `pg_control_system()` privileges in its installation transaction. CNPG's app owner cannot do that. Extracting role provisioning safely requires an independently reviewed privileged provisioning and reconciliation contract, including commit-unknown recovery; this experiment does not alter that API.
- Physical pool attestation, an independent expected-identity handoff, a typed prelock authority, first-lock enforcement for all callers, role provisioning, restricted startup schema attachment, raw-pool removal and shared correctness-pool capacity remain unwired. No inference about a running CNPG cluster, actual failover, cancellation or authoritative commit recovery follows from this fixture.
