# Federation grant-fence experiment

Issue #2047 requires a host to reject newly admitted protected work after its
revocation boundary, including across processes and permission-store failures.
The current production SpiceDB check does not by itself serialize a later
effect against a concurrent revoke. This experiment is test-only: it does not
change production grant authority, routing, migrations or feature activation.

## Test topology

Use native PostgreSQL and PostgreSQL-backed SpiceDB, with separate application
and SpiceDB databases and roles. Spawn finite contenders as independent native
processes with explicit barriers, bounded deadlines and cleanup. Use real
permission checks and writes; no containers, silent prerequisite skips or
mocked store acknowledgements. Pin the repository and tool versions in results.
The local SpiceDB test version is not automatically equivalent to the deployed
operator's `stable` channel.

## Candidate fence

Use test-only scope guards with non-reused incarnation, monotonic generation and
closed/active state. Final effect admission locks and validates the exact active
generation through its PostgreSQL transaction. Mutations lock exclusively and
close the scope before any external write; projections are immutable and
generation-qualified, including all referenced graph edges. Evaluate SpiceDB
outside PostgreSQL locks and reject admission if a later guarded recheck sees a
changed generation. Recovery uses durable operation identity and graph digest;
unknown writes, stale publishers and process death must not reopen an older
generation or invent authority. This is a candidate to falsify, not a production
design or proof of every Waddle authorization path.

Compare PostgreSQL-canonical and SpiceDB-canonical grant variants using the same
guard. Explicitly test their different outage acknowledgements and graph-loss
recovery. Fail closed for protected reads when the permission authority is
unavailable; demonstrate independent scopes can continue within bounded limits.
Use a fresh versioned test identity namespace; this is not a migration of the
ambiguous production codec.

## Required evidence

Drive deterministic barriers for check-versus-revoke and admission-versus-revoke,
stale generation publication, grant/revoke/regrant ABA, unknown external-write
outcomes, process death and restart, scope move/delete/recreate, dependency and
schema drift, and graph recovery. Do not infer commit order from clock times or
sequence values. Exercise the distinction between an accepted frozen room child
and a newly admitted effect: preserving already accepted children and denying
every later release are different contracts. A separately reviewed ingress
change is required if the existing acceptance contract changes.

Record precisely whether an effect was admitted before or after the durable
fence. An already admitted exact attempt may finish after acknowledgement;
the stronger promise that no bytes appear afterward requires a separate drain
or sink fence and cannot be inferred from lease expiration. A passing prototype
does not prove production writer closure, schema immutability, actor/claim
fencing, codec migration, or end-to-end federation readiness.
